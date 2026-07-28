use axum::Router;
use axum::http::header::{ACCEPT, CONTENT_TYPE};
use axum::http::{HeaderValue, Method};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::routes;
use crate::state::AppState;

pub fn create_router(state: AppState) -> Router {
    let cors = if state.config.allowed_origins.is_empty() {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let origins = state
            .config
            .allowed_origins
            .iter()
            .map(|origin| HeaderValue::from_str(origin).expect("validated origin"));
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([ACCEPT, CONTENT_TYPE])
            .allow_credentials(true)
    };

    Router::new()
        .merge(routes::health::router())
        .merge(routes::auth::router())
        .merge(routes::devices::router())
        .merge(routes::recovery::router())
        .merge(routes::workspaces::router())
        .merge(routes::invitations::router())
        .merge(routes::docs::router())
        .merge(routes::assets::router())
        .merge(routes::projections::router())
        .merge(routes::sync::router())
        .layer(cors)
        .with_state(state)
}
