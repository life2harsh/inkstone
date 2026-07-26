use serde::Serialize;
use uuid::Uuid;

use crate::config::Config;
use crate::error::{AppError, AppResult};

#[derive(Serialize)]
struct ResendEmail<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'a str,
    text: &'a str,
}

pub async fn send(
    config: &Config,
    to: &str,
    subject: &str,
    text: &str,
    idempotency_key: Uuid,
) -> AppResult<()> {
    let (Some(api_key), Some(from)) = (
        config.resend_api_key.as_deref(),
        config.resend_from_email.as_deref(),
    ) else {
        // Config validation prevents this in deployed production. Tests and local
        // development intentionally use the same deterministic no-delivery path.
        tracing::info!(
            to,
            subject,
            "email suppressed because Resend is not configured"
        );
        return Ok(());
    };
    let response = reqwest::Client::new()
        .post("https://api.resend.com/emails")
        .bearer_auth(api_key)
        .header("Idempotency-Key", idempotency_key.to_string())
        .json(&ResendEmail {
            from,
            to: [to],
            subject,
            text,
        })
        .send()
        .await
        .map_err(|error| AppError::External(format!("Resend request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(AppError::External(format!(
            "Resend rejected the message with status {}",
            response.status()
        )));
    }
    Ok(())
}
