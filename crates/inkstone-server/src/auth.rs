use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;
use inkstone_core::ids::{DeviceId, UserId};

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: UserId,
    pub device_id: Option<DeviceId>,
}

pub struct DevAuth(pub AuthUser);

impl FromRequestParts<AppState> for DevAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let headers: &HeaderMap = &parts.headers;

        let user_id = headers
            .get("x-dev-user-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| {
                AppError::Unauthorized("Missing or invalid x-dev-user-id header".into())
            })?;

        sqlx::query(
            "INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(user_id)
        .execute(&state.db)
        .await?;

        let device_id = headers
            .get("x-dev-device-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok());

        if let Some(did) = device_id {
            sqlx::query(
                "INSERT INTO devices (id, user_id) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
            )
            .bind(did)
            .bind(user_id)
            .execute(&state.db)
            .await?;
        }

        Ok(DevAuth(AuthUser {
            user_id,
            device_id,
        }))
    }
}
