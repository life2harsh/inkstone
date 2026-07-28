use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::state::AppState;

async fn health_check(State(state): State<AppState>) -> Result<Json<Value>, StatusCode> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(json!({
        "status": "ok",
        "version": "0.1.0",
    })))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health_check))
}
