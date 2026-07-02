use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use crate::auth::DevAuth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use inkstone_core::protocol::{
    CreateDocRequest, DocResponse, PaginatedResponse, PostSnapshotRequest, PostUpdateRequest,
    SnapshotResponse, UpdateResponse,
};

async fn create_doc(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateDocRequest>,
) -> AppResult<Json<DocResponse>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;

    let doc_id = Uuid::new_v4();
    let encrypted_title = base64_decode(&req.encrypted_title_b64)?;
    let title_nonce = base64_decode(&req.title_nonce_b64)?;

    sqlx::query(
        "INSERT INTO docs (id, workspace_id, encrypted_title, title_nonce, created_by) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(doc_id)
    .bind(workspace_id)
    .bind(&encrypted_title)
    .bind(&title_nonce)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;

    let row = sqlx::query_as::<_, (Uuid, Uuid, Vec<u8>, Vec<u8>, Uuid, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, workspace_id, encrypted_title, title_nonce, created_by, created_at, updated_at FROM docs WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(DocResponse {
        id: row.0,
        workspace_id: row.1,
        encrypted_title_b64: base64_encode(&row.2),
        title_nonce_b64: base64_encode(&row.3),
        created_by: row.4,
        created_at: row.5,
        updated_at: row.6,
    }))
}

async fn list_docs(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<DocResponse>>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;

    let rows = sqlx::query_as::<_, (Uuid, Uuid, Vec<u8>, Vec<u8>, Uuid, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, workspace_id, encrypted_title, title_nonce, created_by, created_at, updated_at
         FROM docs WHERE workspace_id = $1
         ORDER BY updated_at DESC",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<DocResponse> = rows
        .into_iter()
        .map(|r| DocResponse {
            id: r.0,
            workspace_id: r.1,
            encrypted_title_b64: base64_encode(&r.2),
            title_nonce_b64: base64_encode(&r.3),
            created_by: r.4,
            created_at: r.5,
            updated_at: r.6,
        })
        .collect();

    let total = items.len() as i64;

    Ok(Json(PaginatedResponse { items, total }))
}

async fn get_doc(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(doc_id): Path<Uuid>,
) -> AppResult<Json<DocResponse>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;

    let row = sqlx::query_as::<_, Option<(Uuid, Uuid, Vec<u8>, Vec<u8>, Uuid, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>>(
        "SELECT id, workspace_id, encrypted_title, title_nonce, created_by, created_at, updated_at FROM docs WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => Ok(Json(DocResponse {
            id: r.0,
            workspace_id: r.1,
            encrypted_title_b64: base64_encode(&r.2),
            title_nonce_b64: base64_encode(&r.3),
            created_by: r.4,
            created_at: r.5,
            updated_at: r.6,
        })),
        None => Err(AppError::NotFound("Document not found".into())),
    }
}

async fn post_update(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<PostUpdateRequest>,
) -> AppResult<Json<UpdateResponse>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;

    let seq = db::allocate_seq(&state.db, doc_id).await?;
    let encrypted_update = base64_decode(&req.encrypted_update_b64)?;
    let nonce = base64_decode(&req.nonce_b64)?;

    sqlx::query(
        "INSERT INTO doc_updates (doc_id, seq, sender_device_id, encrypted_update, nonce, aad_version, client_update_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(doc_id)
    .bind(seq)
    .bind(req.sender_device_id)
    .bind(&encrypted_update)
    .bind(&nonce)
    .bind(req.aad_version)
    .bind(req.client_update_id)
    .execute(&state.db)
    .await?;

    sqlx::query("UPDATE docs SET updated_at = NOW() WHERE id = $1")
        .bind(doc_id)
        .execute(&state.db)
        .await?;

    let created_at = chrono::Utc::now();

    Ok(Json(UpdateResponse { seq, created_at }))
}

async fn list_updates(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(doc_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<UpdateResponse>>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;

    let rows = sqlx::query_as::<_, (i64, chrono::DateTime<chrono::Utc>)>(
        "SELECT seq, created_at FROM doc_updates WHERE doc_id = $1 ORDER BY seq ASC",
    )
    .bind(doc_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<UpdateResponse> = rows
        .into_iter()
        .map(|r| UpdateResponse {
            seq: r.0,
            created_at: r.1,
        })
        .collect();

    let total = items.len() as i64;

    Ok(Json(PaginatedResponse { items, total }))
}

async fn get_snapshot(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(doc_id): Path<Uuid>,
) -> AppResult<Json<SnapshotResponse>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;

    let row = sqlx::query_as::<_, (i64, Vec<u8>, Vec<u8>, chrono::DateTime<chrono::Utc>)>(
        "SELECT snapshot_version, encrypted_snapshot, nonce, created_at
         FROM doc_snapshots WHERE doc_id = $1
         ORDER BY snapshot_version DESC LIMIT 1",
    )
    .bind(doc_id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => Ok(Json(SnapshotResponse {
            snapshot_version: r.0,
            encrypted_snapshot_b64: base64_encode(&r.1),
            nonce_b64: base64_encode(&r.2),
            created_at: r.3,
        })),
        None => Err(AppError::NotFound("No snapshot found".into())),
    }
}

async fn post_snapshot(
    State(state): State<AppState>,
    DevAuth(auth): DevAuth,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<PostSnapshotRequest>,
) -> AppResult<Json<SnapshotResponse>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;

    let encrypted_snapshot = base64_decode(&req.encrypted_snapshot_b64)?;
    let nonce = base64_decode(&req.nonce_b64)?;

    sqlx::query(
        "INSERT INTO doc_snapshots (doc_id, snapshot_version, encrypted_snapshot, nonce)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (doc_id, snapshot_version) DO UPDATE
         SET encrypted_snapshot = $3, nonce = $4",
    )
    .bind(doc_id)
    .bind(req.snapshot_version)
    .bind(&encrypted_snapshot)
    .bind(&nonce)
    .execute(&state.db)
    .await?;

    sqlx::query("UPDATE docs SET updated_at = NOW() WHERE id = $1")
        .bind(doc_id)
        .execute(&state.db)
        .await?;

    Ok(Json(SnapshotResponse {
        snapshot_version: req.snapshot_version,
        encrypted_snapshot_b64: req.encrypted_snapshot_b64,
        nonce_b64: req.nonce_b64,
        created_at: chrono::Utc::now(),
    }))
}

fn base64_decode(input: &str) -> AppResult<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| AppError::BadRequest(format!("Invalid base64: {}", e)))
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/docs",
            post(create_doc).get(list_docs),
        )
        .route("/api/docs/{doc_id}", get(get_doc))
        .route(
            "/api/docs/{doc_id}/updates",
            post(post_update).get(list_updates),
        )
        .route(
            "/api/docs/{doc_id}/snapshot",
            get(get_snapshot).post(post_snapshot),
        )
}
