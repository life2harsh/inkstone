use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::routes;
use crate::state::AppState;

pub fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .merge(routes::health::router())
        .merge(routes::workspaces::router())
        .merge(routes::docs::router())
        .merge(routes::sync::router())
        .layer(cors)
        .with_state(state)
}
