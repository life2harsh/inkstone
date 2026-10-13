use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::extract::FromRequestParts;
use axum::http::header::COOKIE;
use axum::http::request::Parts;
use base64::Engine;
use inkstone_core::ids::{DeviceId, UserId};
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "inkstone_session";

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: UserId,
    pub email: Option<String>,
    pub device_id: Option<DeviceId>,
    pub session_hash: Option<Vec<u8>>,
    pub session_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub public_link: Option<Uuid>,
    pub guest: Option<Uuid>,
}

pub struct Auth(pub AuthUser);

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.config.dev_auth {
            if let Some(user_id) = parts
                .headers
                .get("x-dev-user-id")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| Uuid::parse_str(value).ok())
            {
                sqlx::query("INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING")
                    .bind(user_id)
                    .execute(&state.db)
                    .await?;

                let device_id = parts
                    .headers
                    .get("x-dev-device-id")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| Uuid::parse_str(value).ok());
                if let Some(device_id) = device_id {
                    sqlx::query(
                        "INSERT INTO devices (id, user_id) VALUES ($1, $2)
                         ON CONFLICT (id) DO UPDATE SET last_seen_at = NOW()",
                    )
                    .bind(device_id)
                    .bind(user_id)
                    .execute(&state.db)
                    .await?;
                    crate::db::verify_device_owner(&state.db, device_id, user_id).await?;
                }

                return Ok(Auth(AuthUser {
                    user_id,
                    email: None,
                    device_id,
                    session_hash: None,
                    session_expires_at: None,
                    public_link: None,
                    guest: None,
                }));
            }
        }

        let user = authenticate_session(&parts.headers, state).await?;
        if let Some(workspace_id) = user.public_link {
            if !public_link_may_request(&parts.method, parts.uri.path(), workspace_id) {
                return Err(AppError::Forbidden("Public links are read-only".into()));
            }
        }
        if let Some(workspace_id) = user.guest {
            if !guest_may_request(&parts.method, parts.uri.path(), workspace_id) {
                return Err(AppError::Forbidden(
                    "Guests can only work in the vault they joined".into(),
                ));
            }
        }
        Ok(Auth(user))
    }
}

/// A public link reads one vault's notes and files, and nothing else.
pub fn public_link_may_request(
    method: &axum::http::Method,
    path: &str,
    workspace_id: Uuid,
) -> bool {
    if method == axum::http::Method::POST {
        return path == "/api/public-link/guests";
    }
    if method != axum::http::Method::GET {
        return false;
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let own = |id: &str| Uuid::parse_str(id).is_ok_and(|id| id == workspace_id);
    match segments.as_slice() {
        ["api", "public-link"] => true,
        [
            "api",
            "workspaces",
            id,
            "docs" | "path-projections" | "assets",
        ] => own(id),
        ["api", "docs", _] | ["api", "docs", _, "updates" | "snapshot"] => true,
        ["api", "assets", _] | ["api", "assets", _, "versions", _, "chunks", _] => true,
        _ => false,
    }
}

pub fn guest_may_request(method: &axum::http::Method, path: &str, workspace_id: Uuid) -> bool {
    use axum::http::Method;
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let own = |id: &str| Uuid::parse_str(id).is_ok_and(|id| id == workspace_id);
    match segments.as_slice() {
        ["api", "public-link"] => method == Method::GET,
        ["api", "workspaces", id, "docs" | "assets"] => {
            own(id) && (method == Method::GET || method == Method::POST)
        }
        ["api", "workspaces", id, "path-projections"] => own(id) && method == Method::GET,
        ["api", "workspaces", id, "path-projections", _] => {
            own(id) && (method == Method::PUT || method == Method::DELETE)
        }
        ["api", "workspaces", id, "tree-mutations"] => own(id) && method == Method::POST,
        ["api", "docs", _]
        | ["api", "docs", _, "title" | "updates" | "snapshot"]
        | ["api", "assets", _]
        | ["api", "assets", _, "versions", _, "chunks", _] => true,
        _ => false,
    }
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

pub async fn authenticate_session(
    headers: &axum::http::HeaderMap,
    state: &AppState,
) -> Result<AuthUser, AppError> {
    sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= NOW()")
        .execute(&state.db)
        .await?;
    let bearer = bearer_token(headers);
    let token = bearer
        .or_else(|| cookie_value(headers, SESSION_COOKIE))
        .ok_or_else(|| AppError::Unauthorized("Authentication required".into()))?;
    let token_hash = hash_secret(token.as_bytes());
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Option<String>,
            Uuid,
            chrono::DateTime<chrono::Utc>,
            Option<Uuid>,
            Option<Uuid>,
        ),
    >(
        "SELECT s.user_id, u.email, s.device_id, s.expires_at, u.public_link_workspace_id,
                u.guest_workspace_id
             FROM auth_sessions s
             JOIN users u ON u.id = s.user_id
             JOIN devices d ON d.id = s.device_id AND d.user_id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > NOW() AND d.revoked_at IS NULL",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::Unauthorized("Invalid or expired session".into()))?;
    if bearer.is_some() != (row.4.is_some() || row.5.is_some()) {
        return Err(AppError::Unauthorized("Invalid or expired session".into()));
    }

    sqlx::query("UPDATE devices SET last_seen_at = NOW() WHERE id = $1")
        .bind(row.2)
        .execute(&state.db)
        .await?;

    Ok(AuthUser {
        user_id: row.0,
        email: row.1,
        device_id: Some(row.2),
        session_hash: Some(token_hash),
        session_expires_at: Some(row.3),
        public_link: row.4,
        guest: row.5,
    })
}

pub fn normalize_email(email: &str) -> Result<String, AppError> {
    let email = email.trim().to_lowercase();
    if email.len() > 320
        || !email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
    {
        return Err(AppError::BadRequest("Invalid email address".into()));
    }
    Ok(email)
}

pub fn validate_password(password: &str) -> Result<(), AppError> {
    if !(12..=1024).contains(&password.len()) {
        return Err(AppError::BadRequest(
            "Password must be between 12 and 1024 bytes".into(),
        ));
    }
    Ok(())
}

pub async fn record_rate_limit(
    state: &AppState,
    scope: &str,
    client_key: &str,
    max_attempts: i32,
    window_seconds: i32,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM rate_limits WHERE expires_at <= NOW()")
        .execute(&state.db)
        .await?;
    let key_hash = hash_secret(client_key.as_bytes());
    let attempts: i32 = sqlx::query_scalar(
        "INSERT INTO rate_limits (scope, client_key_hash, window_started_at, attempts, expires_at)
         VALUES ($1, $2, NOW(), 1, NOW() + make_interval(secs => $3))
         ON CONFLICT (scope, client_key_hash) DO UPDATE SET
           attempts = CASE WHEN rate_limits.expires_at <= NOW() THEN 1 ELSE rate_limits.attempts + 1 END,
           window_started_at = CASE WHEN rate_limits.expires_at <= NOW() THEN NOW() ELSE rate_limits.window_started_at END,
           expires_at = CASE WHEN rate_limits.expires_at <= NOW() THEN NOW() + make_interval(secs => $3) ELSE rate_limits.expires_at END
         RETURNING attempts",
    )
    .bind(scope)
    .bind(key_hash)
    .bind(window_seconds as f64)
    .fetch_one(&state.db)
    .await?;
    if attempts > max_attempts {
        return Err(AppError::TooManyRequests(
            "Too many requests; retry later".into(),
        ));
    }
    Ok(())
}

pub async fn clear_rate_limit(state: &AppState, scope: &str, client_key: &str) {
    let _ = sqlx::query("DELETE FROM rate_limits WHERE scope = $1 AND client_key_hash = $2")
        .bind(scope)
        .bind(hash_secret(client_key.as_bytes()))
        .execute(&state.db)
        .await;
}

pub fn client_key(headers: &axum::http::HeaderMap, discriminator: &str) -> String {
    // Only accept a single syntactically valid address. In particular, never trust
    // the attacker-controlled first element of a forwarded chain.
    let address = ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"]
        .into_iter()
        .find_map(|name| {
            let value = headers.get(name)?.to_str().ok()?.trim();
            (!value.contains(',') && value.parse::<std::net::IpAddr>().is_ok()).then_some(value)
        })
        .unwrap_or("direct");
    format!("{address}:{discriminator}")
}

pub async fn hash_password(state: &AppState, password: String) -> Result<String, AppError> {
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.argon2_semaphore.clone().acquire_owned(),
    )
    .await
    .map_err(|_| AppError::TooManyRequests("Password service is busy".into()))?
    .map_err(|_| AppError::Internal("Password hashing is unavailable".into()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut salt = [0_u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let salt = SaltString::encode_b64(&salt)
            .map_err(|_| AppError::Internal("Failed to encode password salt".into()))?;
        argon2id()?
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| AppError::Internal("Failed to hash password".into()))
    })
    .await
    .map_err(|_| AppError::Internal("Password hashing task failed".into()))?
}

pub async fn verify_password(
    state: &AppState,
    password: String,
    encoded: String,
) -> Result<bool, AppError> {
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.argon2_semaphore.clone().acquire_owned(),
    )
    .await
    .map_err(|_| AppError::TooManyRequests("Password service is busy".into()))?
    .map_err(|_| AppError::Internal("Password verification is unavailable".into()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let parsed = PasswordHash::new(&encoded)
            .map_err(|_| AppError::Internal("Stored password hash is invalid".into()))?;
        Ok(argon2id()?
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    })
    .await
    .map_err(|_| AppError::Internal("Password verification task failed".into()))?
}

fn argon2id() -> Result<Argon2<'static>, AppError> {
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|_| AppError::Internal("Invalid Argon2 parameters".into()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn new_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_secret(secret: &[u8]) -> Vec<u8> {
    Sha256::digest(secret).to_vec()
}

pub fn validate_secret_token(token: &str, kind: &str) -> Result<Vec<u8>, AppError> {
    if token.len() != 43 {
        return Err(AppError::BadRequest(format!("Invalid {kind} token")));
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| AppError::BadRequest(format!("Invalid {kind} token")))?;
    if decoded.len() != 32 {
        return Err(AppError::BadRequest(format!("Invalid {kind} token")));
    }
    Ok(hash_secret(token.as_bytes()))
}

pub fn secrets_equal(left: &[u8], right: &[u8]) -> bool {
    let left = Sha256::digest(left);
    let right = Sha256::digest(right);
    bool::from(left.ct_eq(&right))
}

pub fn session_cookie(token: &str, max_age_seconds: i64, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}{}",
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

fn cookie_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::hash_password;
    use crate::config::Config;
    use crate::state::AppState;
    use sqlx::postgres::PgPoolOptions;

    fn state() -> AppState {
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        AppState::new(
            db,
            Config {
                database_url: "redacted".into(),
                host: "127.0.0.1".into(),
                port: 8080,
                dev_auth: true,
                public_base_url: "http://localhost:8080".into(),
                resend_api_key: None,
                resend_from_email: None,
                smtp_host: None,
                smtp_port: None,
                smtp_username: None,
                smtp_password: None,
                smtp_from_email: None,
                email_optional: false,
                allowed_origins: Vec::new(),
                session_ttl_hours: 24,
                cookie_secure: false,
                log_level: "off".into(),
            },
        )
    }

    #[tokio::test]
    async fn argon2_work_waits_for_a_bounded_permit() {
        let state = state();
        let permits = state
            .argon2_semaphore
            .clone()
            .acquire_many_owned(4)
            .await
            .unwrap();
        let hash_state = state.clone();
        let hash = tokio::spawn(async move {
            hash_password(&hash_state, "correct horse battery staple".into()).await
        });
        tokio::task::yield_now().await;
        assert!(!hash.is_finished());

        drop(permits);
        let encoded = tokio::time::timeout(std::time::Duration::from_secs(5), hash)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(encoded.starts_with("$argon2id$"));
    }
}
