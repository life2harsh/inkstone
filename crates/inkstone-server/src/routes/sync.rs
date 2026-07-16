use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::db;
use crate::error::AppResult;
use crate::state::AppState;
use crate::sync::rooms;
use inkstone_core::protocol::{ClientWsMessage, ServerWsMessage};

#[derive(Deserialize)]
pub struct WsAuthParams {
    pub dev_user_id: Uuid,
    pub dev_device_id: Uuid,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(doc_id): Path<Uuid>,
    Query(params): Query<WsAuthParams>,
) -> AppResult<Response> {
    let user_id = params.dev_user_id;
    let device_id = params.dev_device_id;

    sqlx::query("INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING")
        .bind(user_id)
        .execute(&state.db)
        .await?;

    sqlx::query(
        "INSERT INTO devices (id, user_id) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET last_seen_at = NOW()",
    )
    .bind(device_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;

    db::verify_device_owner(&state.db, device_id, user_id).await?;
    db::verify_doc_access(&state.db, doc_id, user_id).await?;

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, doc_id, device_id)))
}

async fn handle_socket(socket: WebSocket, state: AppState, doc_id: Uuid, device_id: Uuid) {
    // ── 1. Join room broadcast ──────────────────────────────────────────────
    let (broadcast_tx, mut broadcast_rx) =
        rooms::join_or_create_async(&state.doc_rooms, doc_id).await;
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // ── 2. Create direct message channel (for Pong, Errors) ────────────────
    let (direct_tx, mut direct_rx) = mpsc::unbounded_channel::<ServerWsMessage>();

    // ── 3. Query current committed sequence ────────────────────────────────
    let current_seq = match db::current_seq(&state.db, doc_id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to get current_seq: {:?}", e);
            let error_msg = ServerWsMessage::Error {
                message: "Failed to query current sequence".into(),
            };
            if let Ok(json) = serde_json::to_string(&error_msg) {
                let _ = ws_sender.send(Message::Text(json.into())).await;
            }
            drop(broadcast_rx);
            drop(broadcast_tx);
            rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
            return;
        }
    };

    // ── 4. Send Ready directly to this connection ──────────────────────────
    let ready_msg = ServerWsMessage::Ready {
        doc_id,
        current_seq,
    };
    let ready_json = match serde_json::to_string(&ready_msg) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("Failed to serialize Ready message: {:?}", e);
            drop(broadcast_rx);
            drop(broadcast_tx);
            rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
            return;
        }
    };
    if let Err(e) = ws_sender.send(Message::Text(ready_json.into())).await {
        tracing::error!("Failed to send Ready message: {:?}", e);
        drop(broadcast_rx);
        drop(broadcast_tx);
        rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
        return;
    }

    // ── 5. Send task: listen to both broadcast AND direct channels ────────
    let mut send_handle = tokio::spawn(async move {
        use tokio::select;

        loop {
            select! {
                result = broadcast_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let json = match serde_json::to_string(&msg) {
                                Ok(j) => j,
                                Err(_) => continue,
                            };
                            if ws_sender.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Receiver lagged by {} messages, closing connection", n);
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
                Some(msg) = direct_rx.recv() => {
                    let json = match serde_json::to_string(&msg) {
                        Ok(j) => j,
                        Err(_) => continue,
                    };
                    if ws_sender.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // ── 6. Handle incoming messages ───────────────────────────────────────
    let mut recv_handle = tokio::spawn(async move {
        let db = state.db.clone();

        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    let parsed = serde_json::from_str::<ClientWsMessage>(&text);
                    match parsed {
                        Ok(ClientWsMessage::EncryptedUpdate {
                            doc_id: msg_doc_id,
                            update_id,
                            encrypted_update_b64,
                            nonce_b64,
                            aad_version,
                        }) => {
                            if msg_doc_id != doc_id {
                                let _ = direct_tx.send(ServerWsMessage::Error {
                                    message: "doc_id in message does not match path".into(),
                                });
                                continue;
                            }

                            let encrypted_update = match base64_decode(&encrypted_update_b64) {
                                Ok(b) => b,
                                Err(e) => {
                                    let _ = direct_tx.send(ServerWsMessage::Error {
                                        message: format!("Invalid encrypted_update_b64: {}", e),
                                    });
                                    continue;
                                }
                            };
                            let nonce = match base64_decode(&nonce_b64) {
                                Ok(b) => b,
                                Err(e) => {
                                    let _ = direct_tx.send(ServerWsMessage::Error {
                                        message: format!("Invalid nonce_b64: {}", e),
                                    });
                                    continue;
                                }
                            };

                            let stored = match db::insert_doc_update_idempotent(
                                &db,
                                doc_id,
                                device_id,
                                update_id.into(),
                                encrypted_update,
                                nonce,
                                aad_version,
                            )
                            .await
                            {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!("Failed to store update: {:?}", e);
                                    let _ = direct_tx.send(ServerWsMessage::Error {
                                        message: "Internal error storing update".into(),
                                    });
                                    continue;
                                }
                            };

                            sqlx::query("UPDATE docs SET updated_at = NOW() WHERE id = $1")
                                .bind(doc_id)
                                .execute(&db)
                                .await
                                .ok();

                            // Room broadcast: use canonical stored payload
                            // For duplicate update_id, the stored payload is the original
                            let canonical_encrypted_b64 = base64_encode(&stored.encrypted_update);
                            let canonical_nonce_b64 = base64_encode(&stored.nonce);
                            let broadcast_msg = ServerWsMessage::EncryptedUpdate {
                                doc_id,
                                sender_device_id: stored.sender_device_id,
                                update_id,
                                seq: stored.seq,
                                encrypted_update_b64: canonical_encrypted_b64,
                                nonce_b64: canonical_nonce_b64,
                                aad_version: stored.aad_version,
                            };
                            let _ = broadcast_tx.send(broadcast_msg);
                        }

                        Ok(ClientWsMessage::Ping) => {
                            // Direct: only this connection gets the Pong
                            let _ = direct_tx.send(ServerWsMessage::Pong);
                        }

                        Ok(ClientWsMessage::Presence {
                            doc_id: msg_doc_id,
                            encrypted_presence_b64,
                        }) => {
                            if msg_doc_id != doc_id {
                                continue;
                            }
                            // Room broadcast: presence to all collaborators
                            let broadcast_msg = ServerWsMessage::Presence {
                                doc_id,
                                sender_device_id: device_id,
                                encrypted_presence_b64,
                            };
                            let _ = broadcast_tx.send(broadcast_msg);
                        }

                        Err(_) => {
                            let _ = direct_tx.send(ServerWsMessage::Error {
                                message: "Unknown message type".into(),
                            });
                        }
                    }
                }
                Message::Binary(_) => {
                    let _ = direct_tx.send(ServerWsMessage::Error {
                        message: "Binary WebSocket messages are not supported".into(),
                    });
                }
                Message::Close(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    });

    // ── 7. Wait for either task, then abort sibling before room cleanup ──
    tokio::select! {
        _ = &mut send_handle => {
            recv_handle.abort();
            let _ = recv_handle.await;
        }
        _ = &mut recv_handle => {
            send_handle.abort();
            let _ = send_handle.await;
        }
    }

    rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| format!("Base64 decode error: {}", e))
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/sync/docs/{doc_id}/ws", get(ws_handler))
}
