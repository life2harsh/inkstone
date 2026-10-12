use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use inkstone_core::protocol::{
    ChangePasswordRequest, GenericMessageResponse, RecoveryEnvelopeRequest,
    RecoveryEnvelopeResponse,
};

use crate::auth::{self, Auth};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const MAX_ENVELOPE: usize = 64 * 1024;

async fn get_envelopes(
    State(state): State<AppState>,
    Auth(user): Auth,
) -> AppResult<Json<Vec<RecoveryEnvelopeResponse>>> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            i64,
            Vec<u8>,
            Vec<u8>,
            i32,
            i32,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT kind, version, ciphertext, nonce, aad_version, wrapping_version, updated_at
         FROM account_recovery_envelopes WHERE user_id = $1 ORDER BY kind",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| RecoveryEnvelopeResponse {
                kind: r.0,
                version: r.1,
                ciphertext_b64: encode(&r.2),
                nonce_b64: encode(&r.3),
                aad_version: r.4,
                wrapping_version: r.5,
                updated_at: r.6,
            })
            .collect(),
    ))
}

async fn put_envelope(
    State(state): State<AppState>,
    Auth(user): Auth,
    Json(req): Json<RecoveryEnvelopeRequest>,
) -> AppResult<Json<RecoveryEnvelopeResponse>> {
    let mut tx = state.db.begin().await?;
    let response = store_envelope(&mut tx, user.user_id, req).await?;
    tx.commit().await?;
    Ok(Json(response))
}

/// The vault key is wrapped by the password, so the two change together.
async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Auth(user): Auth,
    Json(req): Json<ChangePasswordRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    auth::validate_password(&req.current_password)?;
    auth::validate_password(&req.new_password)?;
    if req.passphrase_envelope.kind != "passphrase" {
        return Err(AppError::BadRequest(
            "Expected the password vault envelope".into(),
        ));
    }
    let client_key = auth::client_key(&headers, &user.user_id.to_string());
    auth::record_rate_limit(&state, "change_password", &client_key, 10, 60).await?;
    let current_hash: Option<Option<String>> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user.user_id)
            .fetch_optional(&state.db)
            .await?;
    let current_hash = current_hash
        .flatten()
        .ok_or_else(|| AppError::Forbidden("Account has no password".into()))?;
    if !auth::verify_password(&state, req.current_password, current_hash).await? {
        return Err(AppError::Forbidden("Current password is incorrect".into()));
    }
    auth::clear_rate_limit(&state, "change_password", &client_key).await;
    let password_hash = auth::hash_password(&state, req.new_password).await?;
    let mut tx = state.db.begin().await?;
    store_envelope(&mut tx, user.user_id, req.passphrase_envelope).await?;
    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(password_hash)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(GenericMessageResponse {
        message: "Password changed.".into(),
    }))
}

async fn store_envelope(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: inkstone_core::ids::UserId,
    req: RecoveryEnvelopeRequest,
) -> AppResult<RecoveryEnvelopeResponse> {
    if !matches!(req.kind.as_str(), "passphrase" | "recovery_key")
        || req.aad_version <= 0
        || req.wrapping_version <= 0
    {
        return Err(AppError::BadRequest(
            "Invalid recovery envelope metadata".into(),
        ));
    }
    let ciphertext = decode(&req.ciphertext_b64, 16, MAX_ENVELOPE, "ciphertext")?;
    let nonce = decode(&req.nonce_b64, 12, 12, "nonce")?;
    let expected = req.expected_version.unwrap_or(0);
    let row = sqlx::query_as::<_, (i64, chrono::DateTime<chrono::Utc>, i64)>(if expected == 0 {
        "INSERT INTO account_recovery_envelopes
               (user_id, kind, version, ciphertext, nonce, aad_version, wrapping_version)
             VALUES ($1, $2, 1, $4, $5, $6, $7)
             ON CONFLICT (user_id, kind) DO NOTHING
             RETURNING version, updated_at, $3::bigint"
    } else {
        "UPDATE account_recovery_envelopes SET
               version = version + 1, ciphertext = $4, nonce = $5,
               aad_version = $6, wrapping_version = $7, updated_at = NOW()
             WHERE user_id = $1 AND kind = $2 AND version = $3
             RETURNING version, updated_at, $3::bigint"
    })
    .bind(user_id)
    .bind(&req.kind)
    .bind(expected)
    .bind(&ciphertext)
    .bind(&nonce)
    .bind(req.aad_version)
    .bind(req.wrapping_version)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::Conflict("Recovery envelope version conflict".into()))?;
    Ok(RecoveryEnvelopeResponse {
        kind: req.kind,
        version: row.0,
        ciphertext_b64: encode(&ciphertext),
        nonce_b64: encode(&nonce),
        aad_version: req.aad_version,
        wrapping_version: req.wrapping_version,
        updated_at: row.1,
    })
}

fn decode(value: &str, min: usize, max: usize, name: &str) -> AppResult<Vec<u8>> {
    if value.len() > max * 4 / 3 + 4 {
        return Err(AppError::BadRequest(format!("{name} exceeds size limit")));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| AppError::BadRequest(format!("Invalid {name} base64")))?;
    if !(min..=max).contains(&bytes.len()) {
        return Err(AppError::BadRequest(format!("Invalid {name} size")));
    }
    Ok(bytes)
}

fn encode(value: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(value)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/auth/recovery-envelopes",
            get(get_envelopes).put(put_envelope),
        )
        .route("/api/auth/password", post(change_password))
}
