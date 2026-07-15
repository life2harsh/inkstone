use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use uuid::Uuid;

use crate::auth::DevAuth;
use crate::error::AppResult;
use crate::state::AppState;
use inkstone_core::protocol::{CreateWorkspaceRequest, PaginatedResponse, WorkspaceResponse};

async fn create_workspace(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Json(req): Json<CreateWorkspaceRequest>,
) -> AppResult<Json<WorkspaceResponse>> {
    let workspace_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO workspaces (id, name, description, owner_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(workspace_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(workspace_id)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;

    let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Uuid, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, name, description, owner_id, created_at, updated_at FROM workspaces WHERE id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(WorkspaceResponse {
        id: row.0,
        name: row.1,
        description: row.2,
        owner_id: row.3,
        created_at: row.4,
        updated_at: row.5,
    }))
}

async fn list_workspaces(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
) -> AppResult<Json<PaginatedResponse<WorkspaceResponse>>> {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Uuid, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT w.id, w.name, w.description, w.owner_id, w.created_at, w.updated_at
         FROM workspaces w
         JOIN workspace_members wm ON wm.workspace_id = w.id
         WHERE wm.user_id = $1
         ORDER BY w.updated_at DESC",
    )
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<WorkspaceResponse> = rows
        .into_iter()
        .map(|r| WorkspaceResponse {
            id: r.0,
            name: r.1,
            description: r.2,
            owner_id: r.3,
            created_at: r.4,
            updated_at: r.5,
        })
        .collect();

    let total = items.len() as i64;

    Ok(Json(PaginatedResponse { items, total }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/workspaces", post(create_workspace).get(list_workspaces))
}
