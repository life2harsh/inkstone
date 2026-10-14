use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, header::ORIGIN};
use axum::response::Response;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::sync::rooms;
use crate::sync::{EncryptedPresenceFrame, OutboundMessage};
use inkstone_core::protocol::{ClientWsMessage, ServerWsMessage};

const MAX_UPDATE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PRESENCE_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = MAX_UPDATE_BYTES * 4 / 3 + 64 * 1024;
const MAX_SOCKET_MESSAGE_BYTES: usize = MAX_TEXT_BYTES + 1024 * 1024;
const DIRECT_CHANNEL_CAPACITY: usize = 32;
const WS_REVALIDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Deserialize)]
pub struct WsAuthParams {
    pub device_id: Option<Uuid>,
    pub dev_user_id: Option<Uuid>,
    pub dev_device_id: Option<Uuid>,
    pub link: Option<String>,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(doc_id): Path<Uuid>,
    Query(params): Query<WsAuthParams>,
    headers: HeaderMap,
) -> AppResult<Response> {
    validate_origin(&state, &headers)?;
    // Dev mode takes identities from the query, or a real session when none
    // is given, so the actual sign-in flow also works against a dev server.
    let dev_identity =
        state.config.dev_auth && (params.dev_user_id.is_some() || params.dev_device_id.is_some());
    let (user_id, device_id, session_hash, session_expires_at, read_only) = if dev_identity {
        let user_id = params
            .dev_user_id
            .ok_or_else(|| AppError::Unauthorized("dev_user_id is required in dev mode".into()))?;
        let device_id = params.dev_device_id.ok_or_else(|| {
            AppError::Unauthorized("dev_device_id is required in dev mode".into())
        })?;
        sqlx::query("INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING")
            .bind(user_id)
            .execute(&state.db)
            .await?;
        sqlx::query(
            "INSERT INTO devices (id, user_id) VALUES ($1, $2)
             ON CONFLICT (id) DO UPDATE SET last_seen_at = NOW()",
        )
        .bind(device_id)
        .bind(user_id)
        .execute(&state.db)
        .await?;
        db::verify_device_owner(&state.db, device_id, user_id).await?;
        (user_id, device_id, None, None, false)
    } else {
        let mut headers = headers.clone();
        if let Some(link) = params.link.as_deref() {
            let value = format!("Bearer {link}")
                .parse()
                .map_err(|_| AppError::Unauthorized("Invalid public link".into()))?;
            headers.insert(axum::http::header::AUTHORIZATION, value);
        }
        let session = auth::authenticate_session(&headers, &state).await?;
        let session_device = session
            .device_id
            .ok_or_else(|| AppError::Unauthorized("Session has no device".into()))?;
        let device_id = match session.public_link.or(session.guest) {
            Some(_) => session_device,
            None => params.device_id.ok_or_else(|| {
                AppError::BadRequest("device_id query parameter is required".into())
            })?,
        };
        if device_id != session_device {
            return Err(AppError::Forbidden(
                "device_id does not match the authenticated session".into(),
            ));
        }
        (
            session.user_id,
            device_id,
            session.session_hash,
            session.session_expires_at,
            session.public_link.is_some(),
        )
    };

    db::doc_role(&state.db, doc_id, user_id)
        .await?
        .ok_or_else(|| AppError::Forbidden("Document access denied".into()))?;
    let session_revocations = state.session_revocations.subscribe();
    Ok(ws
        .max_message_size(MAX_SOCKET_MESSAGE_BYTES)
        .max_frame_size(MAX_SOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                state,
                doc_id,
                user_id,
                device_id,
                session_hash,
                session_expires_at,
                session_revocations,
                read_only,
            )
        }))
}

fn validate_origin(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let origin = headers.get(ORIGIN).and_then(|value| value.to_str().ok());
    if !state.config.dev_auth && origin.is_none() {
        return Err(AppError::Forbidden("WebSocket Origin is required".into()));
    }
    if let Some(origin) = origin {
        if !state.config.allowed_origins.is_empty()
            && !state
                .config
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        {
            return Err(AppError::Forbidden(
                "WebSocket Origin is not allowed".into(),
            ));
        }
    }
    Ok(())
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    doc_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
    session_hash: Option<Vec<u8>>,
    session_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    session_revocations: tokio::sync::broadcast::Receiver<Vec<u8>>,
    read_only: bool,
) {
    let (broadcast_tx, mut broadcast_rx) =
        rooms::join_or_create_async(&state.doc_rooms, doc_id).await;
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (direct_tx, mut direct_rx) = mpsc::channel::<OutboundMessage>(DIRECT_CHANNEL_CAPACITY);

    if !websocket_is_authorized(
        &state.db,
        doc_id,
        user_id,
        device_id,
        session_hash.as_deref(),
    )
    .await
    {
        rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
        return;
    }

    let current_seq = match db::current_seq(&state.db, doc_id).await {
        Ok(seq) => seq,
        Err(error) => {
            tracing::error!(?error, "failed to query current sequence");
            send_ws(
                &mut ws_sender,
                ServerWsMessage::Error {
                    message: "Failed to query current sequence".into(),
                }
                .into(),
            )
            .await;
            rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
            return;
        }
    };
    if !send_ws(
        &mut ws_sender,
        ServerWsMessage::Ready {
            doc_id,
            current_seq,
        }
        .into(),
    )
    .await
    {
        rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
        return;
    }

    let send_db = state.db.clone();
    let send_session_hash = session_hash.clone();
    let mut send_handle = tokio::spawn(async move {
        loop {
            let (message, revalidate) = tokio::select! {
                result = broadcast_rx.recv() => match result {
                    // Presence is ephemeral, already encrypted, and sent many
                    // times a second; the periodic check and session
                    // revocation cover it without a query per frame.
                    Ok(message) => {
                        let durable = !matches!(message, OutboundMessage::EncryptedPresence(_));
                        (message, durable)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::warn!(count, "WebSocket receiver lagged; closing");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                message = direct_rx.recv() => match message {
                    Some(message) => (message, false),
                    None => break,
                },
            };
            if (revalidate
                && !websocket_is_authorized(
                    &send_db,
                    doc_id,
                    user_id,
                    device_id,
                    send_session_hash.as_deref(),
                )
                .await)
                || !send_ws(&mut ws_sender, message).await
            {
                break;
            }
        }
    });

    let db = state.db.clone();
    let recv_session_hash = session_hash.clone();
    let mut recv_handle = tokio::spawn(async move {
        let mut message_window_started = std::time::Instant::now();
        let mut durable_count = 0_u32;
        let mut presence_count = 0_u32;
        let mut last_presence_counter = 0_u64;
        while let Some(result) = ws_receiver.next().await {
            let message = match result {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(%doc_id, %device_id, %error, "WebSocket receive failed; closing");
                    break;
                }
            };
            if message_window_started.elapsed() >= std::time::Duration::from_secs(1) {
                message_window_started = std::time::Instant::now();
                durable_count = 0;
                presence_count = 0;
            }
            match message {
                Message::Text(text) => {
                    if text.len() > MAX_TEXT_BYTES {
                        tracing::warn!(%doc_id, %device_id, bytes = text.len(), "WebSocket message exceeds size limit");
                        send_error(&direct_tx, "WebSocket message exceeds size limit").await;
                        continue;
                    }
                    let message = serde_json::from_str::<ClientWsMessage>(&text);
                    if read_only && !matches!(message, Ok(ClientWsMessage::Ping)) {
                        send_error(&direct_tx, "Public links are read-only").await;
                        continue;
                    }
                    match message {
                        Ok(ClientWsMessage::EncryptedUpdate {
                            doc_id: message_doc_id,
                            update_id,
                            encrypted_update_b64,
                            nonce_b64,
                            aad_version,
                        }) => {
                            durable_count += 1;
                            if durable_count > 120 {
                                send_error(&direct_tx, "Durable update rate limit exceeded").await;
                                continue;
                            }
                            if message_doc_id != doc_id {
                                send_error(&direct_tx, "doc_id in message does not match path")
                                    .await;
                                continue;
                            }
                            let (encrypted_update, nonce) = match validate_update(
                                &encrypted_update_b64,
                                &nonce_b64,
                                aad_version,
                            ) {
                                Ok(envelope) => envelope,
                                Err(message) => {
                                    send_error(&direct_tx, &message).await;
                                    continue;
                                }
                            };
                            let Some(role) = websocket_role(
                                &db,
                                doc_id,
                                user_id,
                                device_id,
                                recv_session_hash.as_deref(),
                            )
                            .await
                            else {
                                break;
                            };
                            if !db::WRITE_ROLES.contains(&role.as_str()) {
                                send_error(&direct_tx, "Workspace role is read-only").await;
                                continue;
                            }
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
                                Ok(stored) => stored,
                                Err(AppError::Conflict(message)) => {
                                    send_error(&direct_tx, &message).await;
                                    continue;
                                }
                                Err(error) => {
                                    tracing::error!(?error, "failed to store WebSocket update");
                                    send_error(&direct_tx, "Failed to store update").await;
                                    continue;
                                }
                            };
                            let message = ServerWsMessage::EncryptedUpdate {
                                doc_id,
                                sender_device_id: stored.sender_device_id,
                                update_id,
                                seq: stored.seq,
                                encrypted_update_b64: base64_encode(&stored.encrypted_update),
                                nonce_b64: base64_encode(&stored.nonce),
                                aad_version: stored.aad_version,
                            };
                            let _ = broadcast_tx.send(message.into());
                        }
                        Ok(ClientWsMessage::Ping) => {
                            let _ = direct_tx.send(ServerWsMessage::Pong.into()).await;
                        }
                        Ok(ClientWsMessage::Presence { .. }) => {
                            send_error(
                                &direct_tx,
                                "Versioned encrypted presence metadata is required",
                            )
                            .await;
                        }
                        Ok(ClientWsMessage::EncryptedPresence {
                            doc_id: message_doc_id,
                            encrypted_presence_b64,
                            nonce_b64,
                            aad_version,
                            counter,
                        }) => {
                            presence_count += 1;
                            if presence_count > 60 {
                                send_error(&direct_tx, "Presence rate limit exceeded").await;
                                continue;
                            }
                            if message_doc_id != doc_id {
                                send_error(&direct_tx, "doc_id in message does not match path")
                                    .await;
                                continue;
                            }
                            let valid_ciphertext = base64_decode(&encrypted_presence_b64)
                                .is_ok_and(|bytes| {
                                    (16..=MAX_PRESENCE_BYTES).contains(&bytes.len())
                                });
                            let valid_nonce =
                                base64_decode(&nonce_b64).is_ok_and(|bytes| bytes.len() == 12);
                            if !valid_ciphertext
                                || !valid_nonce
                                || aad_version != 1
                                || counter <= last_presence_counter
                            {
                                send_error(&direct_tx, "Invalid encrypted presence envelope").await;
                                continue;
                            }
                            last_presence_counter = counter;
                            let _ = broadcast_tx.send(OutboundMessage::EncryptedPresence(
                                EncryptedPresenceFrame {
                                    message_type: "encrypted_presence",
                                    doc_id,
                                    sender_device_id: device_id,
                                    encrypted_presence_b64,
                                    nonce_b64,
                                    aad_version,
                                    counter,
                                },
                            ));
                        }
                        Err(_) => send_error(&direct_tx, "Unknown message type").await,
                    }
                }
                Message::Binary(_) => {
                    send_error(&direct_tx, "Binary WebSocket messages are not supported").await
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    });

    let periodic_db = state.db.clone();
    let periodic_session_hash = session_hash.clone();
    let periodic_revalidation = async move {
        let mut interval = tokio::time::interval(WS_REVALIDATE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            if !websocket_is_authorized(
                &periodic_db,
                doc_id,
                user_id,
                device_id,
                periodic_session_hash.as_deref(),
            )
            .await
            {
                break;
            }
        }
    };
    let revocation = wait_for_session_revocation(session_revocations, session_hash.clone());
    let expiry = wait_for_session_expiry(session_expires_at);
    tokio::pin!(periodic_revalidation, revocation, expiry);

    tokio::select! {
        _ = &mut send_handle => {
            recv_handle.abort();
            let _ = recv_handle.await;
        }
        _ = &mut recv_handle => {
            send_handle.abort();
            let _ = send_handle.await;
        }
        _ = &mut periodic_revalidation => {
            send_handle.abort();
            recv_handle.abort();
            let _ = send_handle.await;
            let _ = recv_handle.await;
        }
        _ = &mut revocation => {
            send_handle.abort();
            recv_handle.abort();
            let _ = send_handle.await;
            let _ = recv_handle.await;
        }
        _ = &mut expiry => {
            send_handle.abort();
            recv_handle.abort();
            let _ = send_handle.await;
            let _ = recv_handle.await;
        }
    }
    rooms::remove_empty_room_async(&state.doc_rooms, doc_id).await;
}

async fn send_ws<S>(sender: &mut S, message: OutboundMessage) -> bool
where
    S: futures::Sink<Message> + Unpin,
{
    let Ok(json) = serde_json::to_string(&message) else {
        return true;
    };
    sender.send(Message::Text(json.into())).await.is_ok()
}

async fn send_error(sender: &mpsc::Sender<OutboundMessage>, message: &str) {
    let _ = sender
        .send(
            ServerWsMessage::Error {
                message: message.into(),
            }
            .into(),
        )
        .await;
}

async fn websocket_role(
    db: &sqlx::PgPool,
    doc_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
    session_hash: Option<&[u8]>,
) -> Option<String> {
    match db::websocket_role(db, doc_id, user_id, device_id, session_hash).await {
        Ok(role) => role,
        Err(error) => {
            tracing::error!(?error, "failed to revalidate WebSocket authorization");
            None
        }
    }
}

async fn websocket_is_authorized(
    db: &sqlx::PgPool,
    doc_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
    session_hash: Option<&[u8]>,
) -> bool {
    websocket_role(db, doc_id, user_id, device_id, session_hash)
        .await
        .is_some()
}

async fn wait_for_session_revocation(
    mut receiver: tokio::sync::broadcast::Receiver<Vec<u8>>,
    session_hash: Option<Vec<u8>>,
) {
    let Some(session_hash) = session_hash else {
        futures::future::pending::<()>().await;
        return;
    };
    loop {
        match receiver.recv().await {
            Ok(revoked) if revoked == session_hash => return,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
            | Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn wait_for_session_expiry(expires_at: Option<chrono::DateTime<chrono::Utc>>) {
    let Some(expires_at) = expires_at else {
        futures::future::pending::<()>().await;
        return;
    };
    let delay = (expires_at - chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    tokio::time::sleep(delay).await;
}

fn validate_update(
    encrypted_update_b64: &str,
    nonce_b64: &str,
    aad_version: i32,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    if aad_version != 1 {
        return Err("Unsupported aad_version".into());
    }
    if encrypted_update_b64.len() > MAX_UPDATE_BYTES * 4 / 3 + 4 {
        return Err("Encrypted update exceeds size limit".into());
    }
    let update = base64_decode(encrypted_update_b64)
        .map_err(|_| "Invalid encrypted_update_b64".to_string())?;
    if !(16..=MAX_UPDATE_BYTES).contains(&update.len()) {
        return Err("Encrypted update has invalid size".into());
    }
    let nonce = base64_decode(nonce_b64).map_err(|_| "Invalid nonce_b64".to_string())?;
    if nonce.len() != 12 {
        return Err("nonce must be exactly 12 bytes".into());
    }
    Ok((update, nonce))
}

fn base64_decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(input)
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/sync/docs/{doc_id}/ws", get(ws_handler))
}

#[cfg(test)]
mod tests {
    use super::validate_origin;
    use crate::config::Config;
    use crate::state::AppState;
    use axum::http::{HeaderMap, HeaderValue, header::ORIGIN};
    use sqlx::postgres::PgPoolOptions;

    fn state(dev_auth: bool) -> AppState {
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        AppState::new(
            db,
            Config {
                database_url: "redacted".into(),
                host: "127.0.0.1".into(),
                port: 8080,
                dev_auth,
                public_base_url: if dev_auth {
                    "http://localhost:8080".into()
                } else {
                    "https://inkstone.test".into()
                },
                resend_api_key: None,
                resend_from_email: None,
                smtp_host: None,
                smtp_port: None,
                smtp_username: None,
                smtp_password: None,
                smtp_from_email: None,
                email_optional: false,
                allowed_origins: vec!["https://inkstone.example".into()],
                session_ttl_hours: 24,
                cookie_secure: true,
                log_level: "off".into(),
            },
        )
    }

    #[tokio::test]
    async fn production_websocket_origin_is_required_and_allowlisted() {
        let state = state(false);
        assert!(validate_origin(&state, &HeaderMap::new()).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("https://attacker.example"));
        assert!(validate_origin(&state, &headers).is_err());
        headers.insert(ORIGIN, HeaderValue::from_static("https://inkstone.example"));
        assert!(validate_origin(&state, &headers).is_ok());
    }

    #[tokio::test]
    async fn development_websocket_can_omit_origin() {
        assert!(validate_origin(&state(true), &HeaderMap::new()).is_ok());
    }
}
