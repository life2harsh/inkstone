use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{post, put};
use axum::{Json, Router};
use base64::Engine;
use uuid::Uuid;

use crate::auth::Auth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use inkstone_core::protocol::{
    CreateWorkspaceRequest, PaginatedResponse, PutWorkspaceKeyEnvelopeRequest,
    UpdateWorkspaceRequest, WorkspaceKeyEnvelopeResponse, WorkspaceResponse,
};

const MAX_KEY_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;

type WorkspaceRow = (
    Uuid,
    Vec<u8>,
    Vec<u8>,
    i64,
    Uuid,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

async fn create_workspace(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Json(req): Json<CreateWorkspaceRequest>,
) -> AppResult<Json<WorkspaceResponse>> {
    let encrypted_metadata = decode_bounded(
        &req.encrypted_metadata_b64,
        16,
        MAX_METADATA_BYTES,
        "encrypted workspace metadata",
    )?;
    let metadata_nonce =
        decode_bounded(&req.metadata_nonce_b64, 12, 12, "workspace metadata nonce")?;
    let workspace_id = req.workspace_id;
    let mut tx = state.db.begin().await?;

    sqlx::query("INSERT INTO workspaces (id, encrypted_metadata, metadata_nonce, owner_id) VALUES ($1, $2, $3, $4)")
        .bind(workspace_id)
        .bind(&encrypted_metadata)
        .bind(&metadata_nonce)
        .bind(auth.user_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(workspace_id)
    .bind(auth.user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let row = sqlx::query_as::<_, WorkspaceRow>(
        "SELECT id, encrypted_metadata, metadata_nonce, metadata_version, owner_id, created_at, updated_at FROM workspaces WHERE id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(WorkspaceResponse {
        id: row.0,
        encrypted_metadata_b64: base64_encode(&row.1),
        metadata_nonce_b64: base64_encode(&row.2),
        metadata_version: row.3,
        owner_id: row.4,
        created_at: row.5,
        updated_at: row.6,
    }))
}

async fn list_workspaces(
    State(state): State<AppState>,
    Auth(auth): Auth,
) -> AppResult<Json<PaginatedResponse<WorkspaceResponse>>> {
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Vec<u8>,
            Vec<u8>,
            i64,
            Uuid,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT w.id, w.encrypted_metadata, w.metadata_nonce, w.metadata_version, w.owner_id, w.created_at, w.updated_at
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
            encrypted_metadata_b64: base64_encode(&r.1),
            metadata_nonce_b64: base64_encode(&r.2),
            metadata_version: r.3,
            owner_id: r.4,
            created_at: r.5,
            updated_at: r.6,
        })
        .collect();

    let total = items.len() as i64;

    Ok(Json(PaginatedResponse { items, total }))
}

async fn put_key_envelope(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<PutWorkspaceKeyEnvelopeRequest>,
) -> AppResult<Json<WorkspaceKeyEnvelopeResponse>> {
    let role = db::workspace_role(&state.db, workspace_id, auth.user_id)
        .await?
        .ok_or_else(|| AppError::Forbidden("Workspace administrator access required".into()))?;
    if !matches!(role.as_str(), "owner" | "admin") {
        return Err(AppError::Forbidden(
            "Workspace administrator access required".into(),
        ));
    }
    if req.aad_version != 1 {
        return Err(AppError::BadRequest("Unsupported aad_version".into()));
    }
    let encrypted_key = decode_bounded(
        &req.encrypted_key_b64,
        16,
        MAX_KEY_ENVELOPE_BYTES,
        "encrypted key envelope",
    )?;
    let nonce = decode_bounded(&req.nonce_b64, 12, 12, "nonce")?;

    sqlx::query(
        "INSERT INTO encrypted_workspace_keys
            (workspace_id, user_id, encrypted_key, nonce, aad_version)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (workspace_id, user_id) DO NOTHING",
    )
    .bind(workspace_id)
    .bind(auth.user_id)
    .bind(&encrypted_key)
    .bind(&nonce)
    .bind(req.aad_version)
    .execute(&state.db)
    .await?;
    let stored: (Vec<u8>, Vec<u8>, i32) = sqlx::query_as(
        "SELECT encrypted_key, nonce, aad_version FROM encrypted_workspace_keys
         WHERE workspace_id=$1 AND user_id=$2",
    )
    .bind(workspace_id)
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await?;
    if stored.0 != encrypted_key || stored.1 != nonce || stored.2 != req.aad_version {
        return Err(AppError::Conflict(
            "Workspace key envelope already exists and cannot be silently overwritten".into(),
        ));
    }

    Ok(Json(WorkspaceKeyEnvelopeResponse {
        workspace_id,
        user_id: auth.user_id,
        encrypted_key_b64: base64_encode(&encrypted_key),
        nonce_b64: base64_encode(&nonce),
        aad_version: req.aad_version,
    }))
}

async fn get_key_envelope(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<WorkspaceKeyEnvelopeResponse>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;
    let row = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, i32)>(
        "SELECT encrypted_key, nonce, aad_version
         FROM encrypted_workspace_keys
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(auth.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Workspace key envelope not found".into()))?;

    Ok(Json(WorkspaceKeyEnvelopeResponse {
        workspace_id,
        user_id: auth.user_id,
        encrypted_key_b64: base64_encode(&row.0),
        nonce_b64: base64_encode(&row.1),
        aad_version: row.2,
    }))
}

async fn update_workspace(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<UpdateWorkspaceRequest>,
) -> AppResult<Json<WorkspaceResponse>> {
    let role = db::workspace_role(&state.db, workspace_id, auth.user_id).await?;
    if !role
        .as_deref()
        .is_some_and(|role| matches!(role, "owner" | "admin"))
    {
        return Err(AppError::Forbidden(
            "Workspace administrator access required".into(),
        ));
    }
    if req.metadata_version <= 1 {
        return Err(AppError::BadRequest(
            "metadata_version must be greater than one".into(),
        ));
    }
    let metadata = decode_bounded(
        &req.encrypted_metadata_b64,
        16,
        MAX_METADATA_BYTES,
        "encrypted workspace metadata",
    )?;
    let nonce = decode_bounded(&req.metadata_nonce_b64, 12, 12, "workspace metadata nonce")?;
    let row = sqlx::query_as::<_, WorkspaceRow>(
        "UPDATE workspaces SET encrypted_metadata=$1,metadata_nonce=$2,metadata_version=$3,updated_at=clock_timestamp()
         WHERE id=$4 AND metadata_version < $3
         RETURNING id,encrypted_metadata,metadata_nonce,metadata_version,owner_id,created_at,updated_at",
    ).bind(metadata).bind(nonce).bind(req.metadata_version).bind(workspace_id)
      .fetch_optional(&state.db).await?;
    let row = match row {
        Some(row) => row,
        None => sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id,encrypted_metadata,metadata_nonce,metadata_version,owner_id,created_at,updated_at FROM workspaces WHERE id=$1"
        ).bind(workspace_id).fetch_one(&state.db).await?,
    };
    Ok(Json(WorkspaceResponse {
        id: row.0,
        encrypted_metadata_b64: base64_encode(&row.1),
        metadata_nonce_b64: base64_encode(&row.2),
        metadata_version: row.3,
        owner_id: row.4,
        created_at: row.5,
        updated_at: row.6,
    }))
}

async fn delete_workspace(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    let owner_id: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE id=$1 FOR UPDATE")
            .bind(workspace_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(owner_id) = owner_id else {
        return Ok(StatusCode::NO_CONTENT);
    };
    if owner_id != auth.user_id {
        return Err(AppError::Forbidden(
            "Only the primary owner can delete a workspace".into(),
        ));
    }
    sqlx::query("DELETE FROM workspaces WHERE id=$1")
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn decode_bounded(input: &str, min: usize, max: usize, name: &str) -> AppResult<Vec<u8>> {
    if input.len() > max.saturating_mul(4).div_ceil(3) + 4 {
        return Err(AppError::BadRequest(format!("{name} exceeds size limit")));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|_| AppError::BadRequest(format!("Invalid {name} base64")))?;
    if !(min..=max).contains(&decoded.len()) {
        return Err(AppError::BadRequest(format!(
            "{name} must be between {min} and {max} bytes"
        )));
    }
    Ok(decoded)
}

fn base64_encode(input: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces",
            post(create_workspace).get(list_workspaces),
        )
        .route(
            "/api/workspaces/{workspace_id}/key-envelope",
            put(put_key_envelope).get(get_key_envelope),
        )
        .route(
            "/api/workspaces/{workspace_id}",
            axum::routing::patch(update_workspace).delete(delete_workspace),
        )
}
