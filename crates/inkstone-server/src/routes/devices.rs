use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use inkstone_core::protocol::{DeviceResponse, GenericMessageResponse, SessionResponse};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

async fn list(
    State(state): State<AppState>,
    Auth(user): Auth,
) -> AppResult<Json<Vec<DeviceResponse>>> {
    sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= NOW()")
        .execute(&state.db)
        .await?;
    let devices = sqlx::query_as::<
        _,
        (
            Uuid,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id, device_name, created_at, last_seen_at FROM devices
         WHERE user_id = $1 AND revoked_at IS NULL ORDER BY last_seen_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    let sessions = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            Vec<u8>,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id, device_id, token_hash, created_at, expires_at FROM auth_sessions
         WHERE user_id = $1 AND expires_at > NOW() ORDER BY created_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        devices
            .into_iter()
            .map(|d| DeviceResponse {
                id: d.0,
                name: d.1,
                created_at: d.2,
                last_seen_at: d.3,
                current: user.device_id == Some(d.0.into()),
                sessions: sessions
                    .iter()
                    .filter(|s| s.1 == d.0)
                    .map(|s| SessionResponse {
                        id: s.0,
                        device_id: s.1,
                        created_at: s.3,
                        expires_at: s.4,
                        current: user.session_hash.as_deref() == Some(s.2.as_slice()),
                    })
                    .collect(),
            })
            .collect(),
    ))
}

async fn revoke_session(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<GenericMessageResponse>> {
    let hash: Option<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM auth_sessions WHERE id = $1 AND user_id = $2 RETURNING token_hash",
    )
    .bind(id)
    .bind(user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(hash) = hash else {
        return Err(AppError::NotFound("Session not found".into()));
    };
    let _ = state.session_revocations.send(hash);
    Ok(Json(GenericMessageResponse {
        message: "Session revoked.".into(),
    }))
}

async fn revoke_device(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<GenericMessageResponse>> {
    let mut tx = state.db.begin().await?;
    let changed = sqlx::query("UPDATE devices SET revoked_at = NOW() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL")
        .bind(id).bind(user.user_id).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(AppError::NotFound("Device not found".into()));
    }
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM auth_sessions WHERE device_id = $1 AND user_id = $2 RETURNING token_hash",
    )
    .bind(id)
    .bind(user.user_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for hash in hashes {
        let _ = state.session_revocations.send(hash);
    }
    Ok(Json(GenericMessageResponse {
        message: "Device revoked.".into(),
    }))
}

async fn revoke_others(
    State(state): State<AppState>,
    Auth(user): Auth,
) -> AppResult<Json<GenericMessageResponse>> {
    let current = user
        .session_hash
        .ok_or_else(|| AppError::BadRequest("A session is required".into()))?;
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM auth_sessions WHERE user_id = $1 AND token_hash <> $2 RETURNING token_hash",
    )
    .bind(user.user_id)
    .bind(&current)
    .fetch_all(&state.db)
    .await?;
    for hash in hashes {
        let _ = state.session_revocations.send(hash);
    }
    Ok(Json(GenericMessageResponse {
        message: "Other sessions revoked.".into(),
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/devices", get(list))
        .route("/api/auth/devices/{id}", delete(revoke_device))
        .route("/api/auth/sessions/{id}", delete(revoke_session))
        .route("/api/auth/sessions/revoke-others", post(revoke_others))
}
