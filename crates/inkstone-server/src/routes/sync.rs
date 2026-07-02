use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use uuid::Uuid;

use crate::auth::DevAuth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::sync::rooms;
use inkstone_core::protocol::{ClientWsMessage, ServerWsMessage};

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(doc_id): Path<Uuid>,
    auth: DevAuth,
) -> AppResult<Response> {
    let device_id = auth
        .0
        .device_id
        .ok_or_else(|| AppError::BadRequest("x-dev-device-id header is required for WebSocket sync".into()))?;

    db::verify_doc_access(&state.db, doc_id, auth.0.user_id).await?;

    Ok(ws.on_upgrade(move |socket| {
        handle_socket(socket, state, doc_id, auth.0.user_id, device_id)
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

                            // Allocate sequence number atomically.
                            let seq = match db::allocate_seq(&db, doc_id).await {
                                Ok(s) => s,
                                Err(_) => continue,
                            };

                            let encrypted_update = match base64_decode(&encrypted_update_b64) {
                                Ok(b) => b,
                                Err(_) => continue,
                            };
                            let nonce = match base64_decode(&nonce_b64) {
                                Ok(b) => b,
                                Err(_) => continue,
                            };

                            // Persist the opaque encrypted update blob.
                            if let Err(e) = sqlx::query(
                                "INSERT INTO doc_updates (doc_id, seq, sender_device_id, encrypted_update, nonce, aad_version, client_update_id)
                                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                            )
                            .bind(doc_id)
                            .bind(seq)
                            .bind(device_id)
                            .bind(&encrypted_update)
                            .bind(&nonce)
                            .bind(aad_version)
                            .bind(client_update_id)
                            .execute(&db)
                            .await
                            {
                                tracing::error!("Failed to store update: {:?}", e);
                                continue;
                            }

                            sqlx::query("UPDATE docs SET updated_at = NOW() WHERE id = $1")
                                .bind(doc_id)
                                .execute(&db)
                                .await
                                .ok();

                            let broadcast_msg = ServerWsMessage::EncryptedUpdate {
                                doc_id,
                                sender_device_id: device_id,
                                seq,
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
