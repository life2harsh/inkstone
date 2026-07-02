use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures::stream::SplitSink;
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
    db::verify_doc_access(&state.db, doc_id, auth.0.user_id).await?;

    Ok(ws.on_upgrade(move |socket| {
        handle_socket(socket, state, doc_id, auth)
    }))
}

async fn handle_socket(socket: WebSocket, state: AppState, doc_id: Uuid, auth: DevAuth) {
    let (tx, mut rx) = rooms::join_or_create_async(&state.doc_rooms, doc_id).await;

    let (mut ws_sender, mut ws_receiver) = socket.split();

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

    let recv_task = tokio::spawn(async move {
        let tx_clone = tx;
        let db = state.db.clone();

        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    let parsed = serde_json::from_str::<ClientWsMessage>(&text);
                    match parsed {
                        Ok(ClientWsMessage::EncryptedUpdate {
                            doc_id,
                            client_update_id,
                            encrypted_update_b64,
                            nonce_b64,
                            aad_version,
                        }) => {
                            let sender_device_id = auth.0.device_id.unwrap_or_else(Uuid::new_v4);

                            let seq = match sqlx::query_scalar::<_, Option<i64>>(
                                "SELECT MAX(seq) FROM doc_updates WHERE doc_id = $1",
                            )
                            .bind(doc_id)
                            .fetch_one(&db)
                            .await
                            {
                                Ok(Some(s)) => s + 1,
                                Ok(None) => 1,
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

                            if let Err(e) = sqlx::query(
                                "INSERT INTO doc_updates (doc_id, seq, sender_device_id, encrypted_update, nonce, aad_version, client_update_id)
                                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                            )
                            .bind(doc_id)
                            .bind(seq)
                            .bind(sender_device_id)
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

                            let broadcast_msg = ServerWsMessage::EncryptedUpdate {
                                doc_id,
                                sender_device_id,
                                seq,
                                encrypted_update_b64,
                                nonce_b64,
                                aad_version,
                            };

                            let _ = tx_clone.send(broadcast_msg);
                        }

                        Ok(ClientWsMessage::Ping) => {
                            let _ = tx_clone.send(ServerWsMessage::Pong);
                        }

                        Ok(ClientWsMessage::Presence {
                            doc_id,
                            encrypted_presence_b64,
                        }) => {
                            let sender_device_id = auth.0.device_id.unwrap_or_else(Uuid::new_v4);
                            let broadcast_msg = ServerWsMessage::Presence {
                                doc_id,
                                sender_device_id,
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
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| format!("Base64 decode error: {}", e))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/sync/docs/{doc_id}/ws", get(ws_handler))
}
