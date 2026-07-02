use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::db;
use crate::error::{AppError, AppResult};
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

    // Ensure user exists (dev auth: auto-create).
    sqlx::query("INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING")
        .bind(user_id)
        .execute(&state.db)
        .await?;

    // Ensure device exists (dev auth: auto-create).
    sqlx::query(
        "INSERT INTO devices (id, user_id) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET last_seen_at = NOW()",
    )
    .bind(device_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;

    // Verify device belongs to user.
    db::verify_device_owner(&state.db, device_id, user_id).await?;

    // Verify user has access to this document.
    db::verify_doc_access(&state.db, doc_id, user_id).await?;

    Ok(ws.on_upgrade(move |socket| {
        handle_socket(socket, state, doc_id, user_id, device_id)
    }))
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    doc_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
) {
    let (tx, mut rx) = rooms::join_or_create_async(&state.doc_rooms, doc_id).await;
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Send task: forward broadcast messages to this WebSocket client.
    // The sender receives its own broadcast. The client must ignore
    // messages matching its own client_update_id to avoid echo.
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if ws_sender
                .send(Message::Text(json.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Recv task: read messages from WebSocket and broadcast to room.
    let recv_task = tokio::spawn(async move {
        let tx_clone = tx;
        let db = state.db.clone();

        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    let parsed = serde_json::from_str::<ClientWsMessage>(&text);
                    match parsed {
                        Ok(ClientWsMessage::EncryptedUpdate {
                            doc_id: msg_doc_id,
                            client_update_id,
                            encrypted_update_b64,
                            nonce_b64,
                            aad_version,
                        }) => {
                            // Reject if the message's doc_id does not match the path param.
                            if msg_doc_id != doc_id {
                                let _ = tx_clone
                                    .send(ServerWsMessage::Error {
                                        message: "doc_id in message does not match path".into(),
                                    });
                                continue;
                            }

                            let encrypted_update = match base64_decode(&encrypted_update_b64) {
                                Ok(b) => b,
                                Err(e) => {
                                    let _ = tx_clone.send(ServerWsMessage::Error {
                                        message: format!("Invalid encrypted_update_b64: {}", e),
                                    });
                                    continue;
                                }
                            };
                            let nonce = match base64_decode(&nonce_b64) {
                                Ok(b) => b,
                                Err(e) => {
                                    let _ = tx_clone.send(ServerWsMessage::Error {
                                        message: format!("Invalid nonce_b64: {}", e),
                                    });
                                    continue;
                                }
                            };

                            // Persist idempotently (handles retry/dup).
                            let stored = match db::insert_doc_update_idempotent(
                                &db,
                                doc_id,
                                device_id,
                                client_update_id,
                                encrypted_update,
                                nonce,
                                aad_version,
                            )
                            .await
                            {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!("Failed to store update: {:?}", e);
                                    let _ = tx_clone.send(ServerWsMessage::Error {
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

                            let broadcast_msg = ServerWsMessage::EncryptedUpdate {
                                doc_id,
                                sender_device_id: device_id,
                                client_update_id,
                                seq: stored.seq,
                                encrypted_update_b64,
                                nonce_b64,
                                aad_version,
                            };

                            // Broadcast to ALL clients in the room, including the sender.
                            // The sender client ignores its own update by matching client_update_id.
                            let _ = tx_clone.send(broadcast_msg);
                        }

                        Ok(ClientWsMessage::Ping) => {
                            let _ = tx_clone.send(ServerWsMessage::Pong);
                        }

                        Ok(ClientWsMessage::Presence {
                            doc_id: msg_doc_id,
                            encrypted_presence_b64,
                        }) => {
                            if msg_doc_id != doc_id {
                                continue;
                            }
                            let broadcast_msg = ServerWsMessage::Presence {
                                doc_id,
                                sender_device_id: device_id,
                                encrypted_presence_b64,
                            };
                            let _ = tx_clone.send(broadcast_msg);
                        }

                        Err(_) => {
                            let _ = tx_clone
                                .send(ServerWsMessage::Error {
                                    message: "Unknown message type".into(),
                                });
                        }
                    }
                }
                Message::Close(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| format!("Base64 decode error: {}", e))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/sync/docs/{doc_id}/ws", get(ws_handler))
}
