use base64::Engine;
use inkstone_server::{app, config::Config, db, state::AppState};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn setup() -> (String, PgPool) {
    let pool = db::create_test_pool().await;
    cleanup_tables(&pool).await;
    let base_url = spawn_server(pool.clone()).await;
    (base_url, pool)
}

async fn cleanup_tables(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE TABLE
         audit_events, invitations, comments, assets,
         doc_snapshots, doc_updates, doc_update_counters, docs,
         encrypted_workspace_keys, workspace_members, workspaces,
         devices, identities, users
         CASCADE",
    )
    .execute(pool)
    .await
    .ok();
}

pub async fn setup_with_pool(pool: PgPool) -> String {
    cleanup_tables(&pool).await;
    spawn_server(pool).await
}

async fn spawn_server(pool: PgPool) -> String {
    let config = Config {
        database_url: String::new(),
        host: "127.0.0.1".into(),
        port: 0,
        dev_auth: true,
        oidc_issuer: None,
        oidc_client_id: None,
        oidc_client_secret: None,
        log_level: "off".into(),
    };
    let state = AppState::new(pool, config);
    let router = app::create_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind test server");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{}", addr)
}

pub fn dev_user_id() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}

pub fn dev_device_id() -> Uuid {
    Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
}

pub fn other_user_id() -> Uuid {
    Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
}

pub fn client() -> reqwest::Client {
    reqwest::Client::new()
}

pub fn dev_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-dev-user-id",
        reqwest::header::HeaderValue::from_static("11111111-1111-1111-1111-111111111111"),
    );
    headers.insert(
        "x-dev-device-id",
        reqwest::header::HeaderValue::from_static("22222222-2222-2222-2222-222222222222"),
    );
    headers
}

pub fn dev_user_only_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-dev-user-id",
        reqwest::header::HeaderValue::from_static("11111111-1111-1111-1111-111111111111"),
    );
    headers
}

pub fn other_user_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-dev-user-id",
        reqwest::header::HeaderValue::from_static("33333333-3333-3333-3333-333333333333"),
    );
    headers.insert(
        "x-dev-device-id",
        reqwest::header::HeaderValue::from_static("44444444-4444-4444-4444-444444444444"),
    );
    headers
}

pub fn b64_encode(input: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}
