use tracing_subscriber::EnvFilter;

use inkstone_server::{app, config, db, state};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env();
    tracing::info!("Starting Inkstone server with config: {:?}", config);

    let pool = db::create_pool(&config.database_url).await;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    let app_state = state::AppState::new(pool, config.clone());
    let router = app::create_router(app_state);

    let addr = config.addr();
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind address");

    axum::serve(listener, router)
        .await
        .expect("Server failed");
}
