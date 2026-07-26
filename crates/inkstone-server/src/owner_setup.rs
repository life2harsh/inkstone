use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth;
use crate::config::Config;
use crate::error::{AppError, AppResult};

pub const DEFAULT_EXPIRY_HOURS: i64 = 24;
pub const MAX_EXPIRY_HOURS: i64 = 168;

pub struct IssuedOwnerSetup {
    pub email: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub delivery_idempotency_key: Uuid,
}

impl IssuedOwnerSetup {
    pub fn link(&self, config: &Config) -> String {
        format!(
            "{}/#owner-setup={}",
            config.public_base_url.trim_end_matches('/'),
            self.token
        )
    }
}

pub async fn issue(
    pool: &PgPool,
    email: &str,
    expires_in_hours: i64,
) -> AppResult<IssuedOwnerSetup> {
    if !(1..=MAX_EXPIRY_HOURS).contains(&expires_in_hours) {
        return Err(AppError::BadRequest(format!(
            "Expiry must be between 1 and {MAX_EXPIRY_HOURS} hours"
        )));
    }
    let email = auth::normalize_email(email)?;
    let token = auth::new_secret();
    let token_hash = auth::hash_secret(token.as_bytes());
    let delivery_idempotency_key = Uuid::new_v4();

    let mut tx = pool.begin().await?;
    sqlx::query("LOCK TABLE users IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let user_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE email IS NOT NULL AND password_hash IS NOT NULL)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if user_exists {
        return Err(AppError::Conflict(
            "Owner setup has already been completed".into(),
        ));
    }

    let expires_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "INSERT INTO owner_setup
            (singleton, email, token_hash, issued_at, expires_at, delivery_idempotency_key)
         VALUES (TRUE, $1, $2, NOW(), NOW() + make_interval(hours => $3::int), $4)
         ON CONFLICT (singleton) DO UPDATE SET
            email = EXCLUDED.email,
            token_hash = EXCLUDED.token_hash,
            issued_at = EXCLUDED.issued_at,
            expires_at = EXCLUDED.expires_at,
            delivery_idempotency_key = EXCLUDED.delivery_idempotency_key,
            delivered_at = NULL
         WHERE owner_setup.completed_at IS NULL
         RETURNING expires_at",
    )
    .bind(&email)
    .bind(token_hash)
    .bind(expires_in_hours as i32)
    .bind(delivery_idempotency_key)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(expires_at) = expires_at else {
        return Err(AppError::Conflict(
            "Owner setup has already been completed".into(),
        ));
    };
    tx.commit().await?;

    Ok(IssuedOwnerSetup {
        email,
        token,
        expires_at,
        delivery_idempotency_key,
    })
}

#[derive(Serialize)]
struct ResendEmail<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'a str,
    text: String,
}

pub async fn send(pool: &PgPool, config: &Config, setup: &IssuedOwnerSetup) -> AppResult<()> {
    let api_key = config
        .resend_api_key
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("RESEND_API_KEY is required with --send".into()))?;
    let from = config
        .resend_from_email
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("RESEND_FROM_EMAIL is required with --send".into()))?;
    let link = setup.link(config);
    let body = ResendEmail {
        from,
        to: [&setup.email],
        subject: "Complete your Inkstone owner setup",
        // Text-only mail prevents open pixels and link rewriting by tracking systems.
        text: format!(
            "Complete your Inkstone owner setup:\n\n{link}\n\nThis one-time link expires at {}.",
            setup.expires_at.to_rfc3339()
        ),
    };
    let response = reqwest::Client::new()
        .post("https://api.resend.com/emails")
        .bearer_auth(api_key)
        .header(
            "Idempotency-Key",
            setup.delivery_idempotency_key.to_string(),
        )
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::External(format!("Resend request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(AppError::External(format!(
            "Resend rejected the message with status {}",
            response.status()
        )));
    }
    sqlx::query(
        "UPDATE owner_setup SET delivered_at = NOW()
         WHERE singleton = TRUE AND delivery_idempotency_key = $1 AND completed_at IS NULL",
    )
    .bind(setup.delivery_idempotency_key)
    .execute(pool)
    .await?;
    Ok(())
}
