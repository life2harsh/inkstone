use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue};
use axum::routing::{get, post};
use axum::{Json, Router};
use inkstone_core::protocol::{
    AuthUserResponse, ConsumeEmailVerificationRequest, ConsumePasswordResetRequest,
    GenericMessageResponse, InvitationRegisterRequest, LoginRequest, OwnerSetupRequest,
    RegisterRequest, TokenRequest,
};
use uuid::Uuid;

use crate::auth::{self, Auth};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

async fn register(
    State(state): State<AppState>,
    Json(req): Json<InvitationRegisterRequest>,
) -> AppResult<(HeaderMap, Json<AuthUserResponse>)> {
    let invitation_hash = auth::validate_secret_token(&req.invitation_token, "invitation")?;
    register_user(state, req.registration, invitation_hash).await
}

async fn register_user(
    state: AppState,
    req: RegisterRequest,
    invitation_hash: Vec<u8>,
) -> AppResult<(HeaderMap, Json<AuthUserResponse>)> {
    let email = auth::normalize_email(&req.email)?;
    auth::validate_password(&req.password)?;

    let eligible: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM invitations
            WHERE token_hash = $1 AND LOWER(invitee_email) = $2
              AND accepted = FALSE AND expires_at > NOW()
         )",
    )
    .bind(&invitation_hash)
    .bind(&email)
    .fetch_one(&state.db)
    .await?;
    if !eligible {
        return Err(AppError::Forbidden(
            "A valid invitation for this email address is required".into(),
        ));
    }

    let password_hash = auth::hash_password(&state, req.password).await?;
    let user_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("LOCK TABLE users IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE email IS NOT NULL AND password_hash IS NOT NULL",
    )
    .fetch_one(&mut *tx)
    .await?;
    if user_count == 0 {
        return Err(AppError::Conflict(
            "Owner setup must be completed before registration".into(),
        ));
    }
    let invitation_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM invitations
         WHERE token_hash = $1 AND LOWER(invitee_email) = $2
           AND accepted = FALSE AND expires_at > NOW()
         FOR UPDATE",
    )
    .bind(&invitation_hash)
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;
    if invitation_id.is_none() {
        return Err(AppError::Forbidden(
            "A valid invitation for this email address is required".into(),
        ));
    }

    let inserted = sqlx::query(
        "INSERT INTO users (id, email, password_hash, email_verified_at) VALUES ($1, $2, $3, NOW())",
    )
        .bind(user_id)
        .bind(&email)
        .bind(password_hash)
        .execute(&mut *tx)
        .await;
    if let Err(error) = inserted {
        if error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref()
            == Some("23505")
        {
            return Err(AppError::Conflict(
                "Email address is already registered".into(),
            ));
        }
        return Err(error.into());
    }

    sqlx::query("INSERT INTO devices (id, user_id, device_name) VALUES ($1, $2, $3)")
        .bind(req.device_id)
        .bind(user_id)
        .bind(req.device_name.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref()
                == Some("23505")
            {
                AppError::Conflict("Device is already registered".into())
            } else {
                error.into()
            }
        })?;

    let token = auth::new_secret();
    let token_hash = auth::hash_secret(token.as_bytes());
    sqlx::query(
        "INSERT INTO auth_sessions (token_hash, user_id, device_id, expires_at)
         VALUES ($1, $2, $3, NOW() + make_interval(hours => $4::int))",
    )
    .bind(token_hash)
    .bind(user_id)
    .bind(req.device_id)
    .bind(state.config.session_ttl_hours as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    auth_response(&state, token, user_id, email, req.device_id)
}

async fn complete_owner_setup(
    State(state): State<AppState>,
    Json(req): Json<OwnerSetupRequest>,
) -> AppResult<(HeaderMap, Json<AuthUserResponse>)> {
    let supplied_hash = auth::validate_secret_token(&req.token, "owner setup")?;
    auth::validate_password(&req.password)?;
    let pending = sqlx::query_as::<_, (String, Vec<u8>)>(
        "SELECT email, token_hash FROM owner_setup
         WHERE singleton = TRUE AND completed_at IS NULL AND expires_at > NOW()",
    )
    .fetch_optional(&state.db)
    .await?;
    let Some((email, stored_hash)) = pending else {
        let completed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM owner_setup WHERE singleton = TRUE AND completed_at IS NOT NULL)",
        )
        .fetch_one(&state.db)
        .await?;
        return Err(if completed {
            AppError::Conflict("Owner setup has already been completed".into())
        } else {
            AppError::Forbidden("Invalid or expired owner setup link".into())
        });
    };
    if !auth::secrets_equal(&stored_hash, &supplied_hash) {
        return Err(AppError::Forbidden(
            "Invalid or expired owner setup link".into(),
        ));
    }
    if let Some(supplied) = &req.email {
        if auth::normalize_email(supplied)? != email {
            return Err(AppError::BadRequest(
                "This setup link was issued for a different email".into(),
            ));
        }
    }

    let password_hash = auth::hash_password(&state, req.password).await?;
    let user_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("LOCK TABLE users IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let locked = sqlx::query_as::<_, (String, Vec<u8>)>(
        "SELECT email, token_hash FROM owner_setup
         WHERE singleton = TRUE AND completed_at IS NULL AND expires_at > NOW()
         FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some((locked_email, locked_hash)) = locked else {
        return Err(AppError::Conflict(
            "Owner setup is no longer available".into(),
        ));
    };
    if locked_email != email || !auth::secrets_equal(&locked_hash, &supplied_hash) {
        return Err(AppError::Forbidden(
            "Invalid or expired owner setup link".into(),
        ));
    }
    let user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE email IS NOT NULL AND password_hash IS NOT NULL",
    )
    .fetch_one(&mut *tx)
    .await?;
    if user_count != 0 {
        return Err(AppError::Conflict(
            "Owner setup has already been completed".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, email_verified_at) VALUES ($1, $2, $3, NOW())",
    )
        .bind(user_id)
        .bind(&email)
        .bind(password_hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO devices (id, user_id, device_name) VALUES ($1, $2, $3)")
        .bind(req.device_id)
        .bind(user_id)
        .bind(req.device_name.as_deref())
        .execute(&mut *tx)
        .await?;
    let session_token = auth::new_secret();
    sqlx::query(
        "INSERT INTO auth_sessions (token_hash, user_id, device_id, expires_at)
         VALUES ($1, $2, $3, NOW() + make_interval(hours => $4::int))",
    )
    .bind(auth::hash_secret(session_token.as_bytes()))
    .bind(user_id)
    .bind(req.device_id)
    .bind(state.config.session_ttl_hours as i32)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE owner_setup SET token_hash = NULL, expires_at = NULL,
            completed_at = NOW(), owner_user_id = $1
         WHERE singleton = TRUE",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    auth_response(&state, session_token, user_id, email, req.device_id)
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> AppResult<(HeaderMap, Json<AuthUserResponse>)> {
    auth::validate_password(&req.password)?;
    let email = auth::normalize_email(&req.email)?;
    let client_key = auth::client_key(&headers, &email);
    auth::record_rate_limit(&state, "login", &client_key, 10, 60).await?;
    let row = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, password_hash FROM users WHERE LOWER(email) = $1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let Some((user_id, password_hash)) = row else {
        let _ = auth::hash_password(&state, req.password).await?;
        return Err(AppError::Unauthorized("Invalid email or password".into()));
    };
    if !auth::verify_password(&state, req.password, password_hash).await? {
        return Err(AppError::Unauthorized("Invalid email or password".into()));
    }
    auth::clear_rate_limit(&state, "login", &client_key).await;

    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO devices (id, user_id, device_name) VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE SET device_name = COALESCE(EXCLUDED.device_name, devices.device_name), last_seen_at = NOW()
         WHERE devices.user_id = EXCLUDED.user_id",
    )
    .bind(req.device_id)
    .bind(user_id)
    .bind(req.device_name.as_deref())
    .execute(&mut *tx)
    .await?;
    let owned: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM devices WHERE id = $1 AND user_id = $2)")
            .bind(req.device_id)
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
    if !owned {
        return Err(AppError::Forbidden("Device belongs to another user".into()));
    }

    let token = auth::new_secret();
    let token_hash = auth::hash_secret(token.as_bytes());
    sqlx::query(
        "INSERT INTO auth_sessions (token_hash, user_id, device_id, expires_at)
         VALUES ($1, $2, $3, NOW() + make_interval(hours => $4::int))",
    )
    .bind(token_hash)
    .bind(user_id)
    .bind(req.device_id)
    .bind(state.config.session_ttl_hours as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    auth_response(&state, token, user_id, email, req.device_id)
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Auth(auth_user): Auth,
) -> AppResult<HeaderMap> {
    if !state.config.dev_auth {
        let origin = headers
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| AppError::Forbidden("Allowed Origin header required".into()))?;
        if !state
            .config
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
        {
            return Err(AppError::Forbidden("Origin is not allowed".into()));
        }
    }
    if let Some(session_hash) = auth_user.session_hash {
        sqlx::query("DELETE FROM auth_sessions WHERE token_hash = $1")
            .bind(&session_hash)
            .execute(&state.db)
            .await?;
        let _ = state.session_revocations.send(session_hash);
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&auth::clear_session_cookie(state.config.cookie_secure))
            .map_err(|_| AppError::Internal("Failed to create session cookie".into()))?,
    );
    Ok(headers)
}

async fn me(Auth(auth_user): Auth) -> AppResult<Json<AuthUserResponse>> {
    let email = auth_user
        .email
        .ok_or_else(|| AppError::BadRequest("Development user has no email".into()))?;
    let device_id = auth_user
        .device_id
        .ok_or_else(|| AppError::BadRequest("Authenticated session has no device".into()))?;
    Ok(Json(AuthUserResponse {
        id: auth_user.user_id.into(),
        email,
        device_id: device_id.into(),
    }))
}

async fn request_email_verification(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TokenRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    request_account_token(state, headers, req, "email_verification").await
}

async fn request_password_reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TokenRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    request_account_token(state, headers, req, "password_reset").await
}

async fn request_account_token(
    state: AppState,
    headers: HeaderMap,
    req: TokenRequest,
    purpose: &'static str,
) -> AppResult<Json<GenericMessageResponse>> {
    let email = auth::normalize_email(&req.email)?;
    let client_key = auth::client_key(&headers, &email);
    auth::record_rate_limit(&state, purpose, &client_key, 5, 3600).await?;
    let user_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE LOWER(email) = $1")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;
    if let Some(user_id) = user_id {
        let token = auth::new_secret();
        let idempotency_key = Uuid::new_v4();
        sqlx::query(
            "DELETE FROM auth_tokens WHERE user_id = $1 AND purpose = $2 AND consumed_at IS NULL",
        )
        .bind(user_id)
        .bind(purpose)
        .execute(&state.db)
        .await?;
        sqlx::query(
            "INSERT INTO auth_tokens
               (token_hash, purpose, user_id, email, expires_at, delivery_idempotency_key)
             VALUES ($1, $2, $3, $4, NOW() + INTERVAL '1 hour', $5)",
        )
        .bind(auth::hash_secret(token.as_bytes()))
        .bind(purpose)
        .bind(user_id)
        .bind(&email)
        .bind(idempotency_key)
        .execute(&state.db)
        .await?;
        let (subject, fragment, explanation) = if purpose == "password_reset" {
            (
                "Reset your Inkstone account password",
                "password-reset",
                "After resetting, you'll need your recovery key to unlock your notes.",
            )
        } else {
            (
                "Verify your Inkstone email",
                "verify-email",
                "This verifies your account email.",
            )
        };
        let link = format!(
            "{}/#{fragment}={token}&email={}",
            state.config.public_base_url.trim_end_matches('/'),
            email
                .replace('%', "%25")
                .replace('&', "%26")
                .replace('#', "%23")
        );
        if let Err(error) = crate::email::send(
            &state.config,
            &email,
            subject,
            &format!("{explanation}\n\n{link}\n\nThis single-use link expires in one hour."),
            idempotency_key,
        )
        .await
        {
            tracing::error!(?error, purpose, "account email delivery failed");
        }
    }
    Ok(Json(GenericMessageResponse {
        message: "If the account is eligible, a single-use email has been sent.".into(),
    }))
}

async fn consume_email_verification(
    State(state): State<AppState>,
    Json(req): Json<ConsumeEmailVerificationRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    let token_hash = auth::validate_secret_token(&req.token, "email verification")?;
    let updated = sqlx::query(
        "WITH consumed AS (
           UPDATE auth_tokens SET consumed_at = NOW()
           WHERE token_hash = $1 AND purpose = 'email_verification'
             AND consumed_at IS NULL AND expires_at > NOW()
           RETURNING user_id
         )
         UPDATE users SET email_verified_at = COALESCE(email_verified_at, NOW())
         WHERE id = (SELECT user_id FROM consumed)",
    )
    .bind(token_hash)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::BadRequest(
            "Invalid or expired email verification token".into(),
        ));
    }
    Ok(Json(GenericMessageResponse {
        message: "Email verified.".into(),
    }))
}

async fn consume_password_reset(
    State(state): State<AppState>,
    Json(req): Json<ConsumePasswordResetRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    auth::validate_password(&req.password)?;
    let token_hash = auth::validate_secret_token(&req.token, "password reset")?;
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT t.user_id, u.email FROM auth_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = $1 AND t.purpose = 'password_reset'
         AND t.consumed_at IS NULL AND t.expires_at > NOW()",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await?;
    let Some((user_id, account_email)) = row else {
        return Err(AppError::BadRequest(
            "Invalid or expired password reset token".into(),
        ));
    };
    if let Some(supplied) = &req.email {
        if Some(auth::normalize_email(supplied)?) != account_email.map(|e| e.to_lowercase()) {
            return Err(AppError::BadRequest(
                "This reset link was issued for a different email".into(),
            ));
        }
    }
    let password_hash = auth::hash_password(&state, req.password).await?;
    let mut tx = state.db.begin().await?;
    let consumed: Option<Uuid> = sqlx::query_scalar(
        "UPDATE auth_tokens SET consumed_at = NOW()
         WHERE token_hash = $1 AND purpose = 'password_reset'
           AND user_id = $2 AND consumed_at IS NULL AND expires_at > NOW()
         RETURNING user_id",
    )
    .bind(token_hash)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if consumed.is_none() {
        return Err(AppError::BadRequest(
            "Invalid or expired password reset token".into(),
        ));
    }
    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(password_hash)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let revoked: Vec<Vec<u8>> =
        sqlx::query_scalar("DELETE FROM auth_sessions WHERE user_id = $1 RETURNING token_hash")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    for hash in revoked {
        let _ = state.session_revocations.send(hash);
    }
    Ok(Json(GenericMessageResponse {
        message: "Password reset. Sign in, then use your recovery key to unlock your notes.".into(),
    }))
}

fn auth_response(
    state: &AppState,
    token: String,
    user_id: Uuid,
    email: String,
    device_id: Uuid,
) -> AppResult<(HeaderMap, Json<AuthUserResponse>)> {
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&auth::session_cookie(
            &token,
            state.config.session_ttl_hours * 3600,
            state.config.cookie_secure,
        ))
        .map_err(|_| AppError::Internal("Failed to create session cookie".into()))?,
    );
    Ok((
        headers,
        Json(AuthUserResponse {
            id: user_id,
            email,
            device_id,
        }),
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/owner-setup", post(complete_owner_setup))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route(
            "/api/auth/email-verification/request",
            post(request_email_verification),
        )
        .route(
            "/api/auth/email-verification/consume",
            post(consume_email_verification),
        )
        .route(
            "/api/auth/password-reset/request",
            post(request_password_reset),
        )
        .route(
            "/api/auth/password-reset/consume",
            post(consume_password_reset),
        )
        .layer(DefaultBodyLimit::max(4096))
}
