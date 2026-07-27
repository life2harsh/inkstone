use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use inkstone_core::protocol::{RecoveryEnvelopeRequest, RecoveryEnvelopeResponse};

use crate::auth::Auth;
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
    let row = sqlx::query_as::<_, (i64, chrono::DateTime<chrono::Utc>)>(
        "INSERT INTO account_recovery_envelopes
           (user_id, kind, version, ciphertext, nonce, aad_version, wrapping_version)
         SELECT $1, $2, 1, $4, $5, $6, $7 WHERE $3 = 0
         ON CONFLICT (user_id, kind) DO UPDATE SET
           version = account_recovery_envelopes.version + 1,
           ciphertext = EXCLUDED.ciphertext, nonce = EXCLUDED.nonce,
           aad_version = EXCLUDED.aad_version, wrapping_version = EXCLUDED.wrapping_version,
           updated_at = NOW()
         WHERE account_recovery_envelopes.version = $3
         RETURNING version, updated_at",
    )
    .bind(user.user_id)
    .bind(&req.kind)
    .bind(expected)
    .bind(&ciphertext)
    .bind(&nonce)
    .bind(req.aad_version)
    .bind(req.wrapping_version)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::Conflict("Recovery envelope version conflict".into()))?;
    Ok(Json(RecoveryEnvelopeResponse {
        kind: req.kind,
        version: row.0,
        ciphertext_b64: encode(&ciphertext),
        nonce_b64: encode(&nonce),
        aad_version: req.aad_version,
        wrapping_version: req.wrapping_version,
        updated_at: row.1,
    }))
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
    Router::new().route(
        "/api/auth/recovery-envelopes",
        get(get_envelopes).put(put_envelope),
    )
}
