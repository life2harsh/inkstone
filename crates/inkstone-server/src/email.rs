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

pub fn is_configured(config: &Config) -> bool {
    config.smtp_host.is_some()
        || (config.resend_api_key.is_some() && config.resend_from_email.is_some())
}

pub async fn send(
    config: &Config,
    to: &str,
    subject: &str,
    text: &str,
    idempotency_key: Uuid,
) -> AppResult<()> {
    if config.smtp_host.is_some() {
        send_smtp(config, to, subject, text).await
    } else if config.resend_api_key.is_some() {
        send_resend(config, to, subject, text, idempotency_key).await
    } else {
        // Config validation prevents this in deployed production. Tests and local
        // development intentionally use the same deterministic no-delivery path.
        tracing::info!(
            to,
            subject,
            "email suppressed because no provider is configured"
        );
        Ok(())
    }
}

async fn send_smtp(config: &Config, to: &str, subject: &str, text: &str) -> AppResult<()> {
    use lettre::message::{Mailbox, Message, header::ContentType};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::transport::smtp::client::Tls;
    use lettre::{SmtpTransport, Transport};

    let host = config
        .smtp_host
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("SMTP_HOST is not configured".into()))?;
    let port = config.smtp_port.unwrap_or(587);
    let from = config
        .smtp_from_email
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("SMTP_FROM_EMAIL is not configured".into()))?;
    let sender: Mailbox = from
        .parse()
        .map_err(|error| AppError::BadRequest(format!("Invalid SMTP_FROM_EMAIL: {error}")))?;
    let recipient: Mailbox = to
        .parse()
        .map_err(|error| AppError::BadRequest(format!("Invalid recipient email: {error}")))?;

    let email = Message::builder()
        .from(sender)
        .to(recipient)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(text.to_owned())
        .map_err(|error| AppError::External(format!("Failed to build SMTP message: {error}")))?;

    let tls = if port == 465 {
        Tls::Wrapper(
            lettre::transport::smtp::client::TlsParameters::new(host.to_owned()).map_err(
                |error| AppError::External(format!("Failed to configure SMTP TLS: {error}")),
            )?,
        )
    } else {
        Tls::Required(
            lettre::transport::smtp::client::TlsParameters::new(host.to_owned()).map_err(
                |error| AppError::External(format!("Failed to configure SMTP TLS: {error}")),
            )?,
        )
    };
    let mut builder = SmtpTransport::builder_dangerous(host.to_owned())
        .port(port)
        .tls(tls);
    if let (Some(username), Some(password)) = (
        config.smtp_username.as_deref(),
        config.smtp_password.as_deref(),
    ) {
        builder = builder.credentials(Credentials::new(username.to_owned(), password.to_owned()));
    }
    let transport = builder.build();

    transport
        .send(&email)
        .map_err(|error| AppError::External(format!("SMTP delivery failed: {error}")))?;
    Ok(())
}

async fn send_resend(
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
        return Err(AppError::BadRequest(
            "RESEND_API_KEY and RESEND_FROM_EMAIL are not configured".into(),
        ));
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
