use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::state::AppState;

async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": "0.1.0",
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health_check))
}
