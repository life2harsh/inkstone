use tracing_subscriber::EnvFilter;

use inkstone_server::{app, config, db, email, owner_setup, state};

struct OwnerSetupIssueArgs {
    email: String,
    expires_in_hours: i64,
    send: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env().expect("Invalid server configuration");
    let pool = db::create_pool(&config.database_url).await;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let issue_args = parse_owner_setup_issue(&args).unwrap_or_else(|message| {
            eprintln!("{message}");
            std::process::exit(2);
        });
        if issue_args.send && !email::is_configured(&config) {
            eprintln!("an email provider (RESEND or SMTP) is required with --send");
            std::process::exit(2);
        }
        let setup = owner_setup::issue(&pool, &issue_args.email, issue_args.expires_in_hours)
            .await
            .unwrap_or_else(|error| {
                eprintln!("Failed to issue owner setup link: {error}");
                std::process::exit(1);
            });
        if issue_args.send {
            owner_setup::send(&pool, &config, &setup)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("Failed to send owner setup email: {error}");
                    std::process::exit(1);
                });
            println!(
                "Owner setup email sent to {} (expires {}).",
                setup.email,
                setup.expires_at.to_rfc3339()
            );
        } else {
            // The operator explicitly requested the one-time link on stdout.
            println!("{}", setup.link(&config));
        }
        return;
    }

    tracing::info!("Starting Inkstone server with config: {:?}", config);

    let app_state = state::AppState::new(pool, config.clone());
    let router = app::create_router(app_state);

    let addr = config.addr();
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind address");

    axum::serve(listener, router).await.expect("Server failed");
}

fn parse_owner_setup_issue(args: &[String]) -> Result<OwnerSetupIssueArgs, String> {
    if args.first().map(String::as_str) != Some("owner-setup")
        || args.get(1).map(String::as_str) != Some("issue")
    {
        return Err(usage());
    }
    let mut email = None;
    let mut expires_in_hours = owner_setup::DEFAULT_EXPIRY_HOURS;
    let mut send = false;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--email" => {
                index += 1;
                email = args.get(index).cloned();
                if email.is_none() {
                    return Err("--email requires a value".into());
                }
            }
            "--expires-in-hours" => {
                index += 1;
                expires_in_hours = args
                    .get(index)
                    .ok_or_else(|| "--expires-in-hours requires a value".to_string())?
                    .parse()
                    .map_err(|_| "--expires-in-hours must be an integer".to_string())?;
            }
            "--send" => send = true,
            _ => return Err(usage()),
        }
        index += 1;
    }
    Ok(OwnerSetupIssueArgs {
        email: email.ok_or_else(|| "--email is required".to_string())?,
        expires_in_hours,
        send,
    })
}

fn usage() -> String {
    "Usage: inkstone-server owner-setup issue --email EMAIL [--expires-in-hours HOURS] [--send]"
        .into()
}

#[cfg(test)]
mod tests {
    use super::parse_owner_setup_issue;

    #[test]
    fn parses_owner_setup_issue_command() {
        let args = [
            "owner-setup",
            "issue",
            "--email",
            "owner@example.com",
            "--expires-in-hours",
            "12",
            "--send",
        ]
        .map(str::to_string);
        let parsed = parse_owner_setup_issue(&args).unwrap();
        assert_eq!(parsed.email, "owner@example.com");
        assert_eq!(parsed.expires_in_hours, 12);
        assert!(parsed.send);
    }
}
