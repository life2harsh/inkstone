use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::Auth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use inkstone_core::ids::UpdateId;
use inkstone_core::protocol::{
    CreateDocRequest, DocResponse, EncryptedUpdateResponse, PaginatedResponse, PostSnapshotRequest,
    PostUpdateRequest, ServerWsMessage, SnapshotResponse, UpdateDocPathProjectionRequest,
    UpdateDocTitleProjectionRequest, UpdateResponse,
};

const NONCE_LEN: usize = 12;
const MAX_TITLE_BYTES: usize = 16 * 1024;
const MAX_UPDATE_BYTES: usize = 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 10 * 1024 * 1024;

#[derive(Deserialize)]
pub struct ListUpdatesParams {
    pub after_seq: Option<i64>,
    pub through_seq: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct DocRow {
    id: Uuid,
    workspace_id: Uuid,
    encrypted_title: Option<Vec<u8>>,
    title_nonce: Option<Vec<u8>>,
    title_projection_seq: i64,
    encrypted_path: Vec<u8>,
    path_nonce: Vec<u8>,
    path_projection_version: i64,
    created_by: Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    #[sqlx(default)]
    current_seq: Option<i64>,
}

async fn create_doc(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateDocRequest>,
) -> AppResult<Json<DocResponse>> {
    db::verify_workspace_write(&state.db, workspace_id, auth.user_id).await?;
    let (encrypted_title, title_nonce) = match (
        req.encrypted_title_b64.as_deref(),
        req.title_nonce_b64.as_deref(),
    ) {
        (Some(encrypted), Some(nonce)) => (
            Some(decode_envelope(
                encrypted,
                16,
                MAX_TITLE_BYTES,
                "encrypted title",
            )?),
            Some(decode_nonce(nonce)?),
        ),
        (None, None) => (None, None),
        _ => {
            return Err(AppError::BadRequest(
                "encrypted title and nonce must be provided together".into(),
            ));
        }
    };
    let encrypted_path = decode_envelope(
        &req.encrypted_path_b64,
        16,
        MAX_TITLE_BYTES,
        "encrypted path",
    )?;
    let path_nonce = decode_nonce(&req.path_nonce_b64)?;

    sqlx::query(
        "INSERT INTO docs
            (id, workspace_id, encrypted_title, title_nonce, encrypted_path, path_nonce, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
    )
    .bind(req.doc_id)
    .bind(workspace_id)
    .bind(&encrypted_title)
    .bind(&title_nonce)
    .bind(&encrypted_path)
    .bind(&path_nonce)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;

    let row = fetch_doc(&state, req.doc_id).await?;
    if row.workspace_id != workspace_id
        || row.encrypted_path != encrypted_path
        || row.path_nonce != path_nonce
        || row.created_by != auth.user_id
    {
        return Err(AppError::Conflict(
            "doc_id is already used by a different document".into(),
        ));
    }
    Ok(Json(doc_response(row)))
}

async fn list_docs(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<DocResponse>>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;
    let rows = sqlx::query_as::<_, DocRow>(
        "SELECT id, workspace_id, encrypted_title, title_nonce, title_projection_seq,
                encrypted_path, path_nonce, path_projection_version,
                created_by, created_at, updated_at,
                COALESCE((SELECT c.current_seq FROM doc_update_counters c WHERE c.doc_id = docs.id), 0)
                    AS current_seq
         FROM docs WHERE workspace_id = $1 ORDER BY updated_at DESC",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<_> = rows.into_iter().map(doc_response).collect();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn get_doc(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
) -> AppResult<Json<DocResponse>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;
    Ok(Json(doc_response(fetch_doc(&state, doc_id).await?)))
}

async fn update_doc_path_projection(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<UpdateDocPathProjectionRequest>,
) -> AppResult<Json<DocResponse>> {
    db::verify_doc_write(&state.db, doc_id, auth.user_id).await?;
    if req.projection_version <= 0 {
        return Err(AppError::BadRequest(
            "path projection version must be positive".into(),
        ));
    }
    let encrypted_path = decode_envelope(
        &req.encrypted_path_b64,
        16,
        MAX_TITLE_BYTES,
        "encrypted path",
    )?;
    let path_nonce = decode_nonce(&req.path_nonce_b64)?;
    let row = sqlx::query_as::<_, DocRow>(
        "UPDATE docs
         SET encrypted_path = $1, path_nonce = $2, path_projection_version = $3,
             updated_at = clock_timestamp()
         WHERE id = $4 AND path_projection_version < $3
         RETURNING id, workspace_id, encrypted_title, title_nonce, title_projection_seq,
                   encrypted_path, path_nonce, path_projection_version,
                   created_by, created_at, updated_at",
    )
    .bind(&encrypted_path)
    .bind(&path_nonce)
    .bind(req.projection_version)
    .bind(doc_id)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some(row) => Ok(Json(doc_response(row))),
        None => Ok(Json(doc_response(fetch_doc(&state, doc_id).await?))),
    }
}

async fn update_doc_title_projection(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<UpdateDocTitleProjectionRequest>,
) -> AppResult<Json<DocResponse>> {
    db::verify_doc_write(&state.db, doc_id, auth.user_id).await?;
    if req.base_seq < 0 || req.base_seq > db::current_seq(&state.db, doc_id).await? {
        return Err(AppError::BadRequest(
            "title projection base_seq is outside the committed document history".into(),
        ));
    }
    let encrypted_title = decode_envelope(
        &req.encrypted_title_b64,
        16,
        MAX_TITLE_BYTES,
        "encrypted title",
    )?;
    let title_nonce = decode_nonce(&req.title_nonce_b64)?;
    let row = sqlx::query_as::<_, DocRow>(
        "UPDATE docs
         SET encrypted_title = $1, title_nonce = $2, title_projection_seq = $3,
             updated_at = clock_timestamp()
         WHERE id = $4 AND title_projection_seq <= $3
         RETURNING id, workspace_id, encrypted_title, title_nonce, title_projection_seq,
                   encrypted_path, path_nonce, path_projection_version,
                   created_by, created_at, updated_at",
    )
    .bind(&encrypted_title)
    .bind(&title_nonce)
    .bind(req.base_seq)
    .bind(doc_id)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some(row) => Ok(Json(doc_response(row))),
        None => Ok(Json(doc_response(fetch_doc(&state, doc_id).await?))),
    }
}

async fn delete_doc(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let workspace_id: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM docs WHERE id = $1")
            .bind(doc_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(_workspace_id) = workspace_id else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let deleted: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM docs d
         USING workspace_members wm
         WHERE d.id = $1
           AND wm.workspace_id = d.workspace_id
           AND wm.user_id = $2
           AND wm.role IN ('owner', 'admin', 'editor')
         RETURNING d.id",
    )
    .bind(doc_id)
    .bind(auth.user_id)
    .fetch_optional(&state.db)
    .await?;
    if deleted.is_none() {
        return Err(AppError::Forbidden(
            "User does not have write access to this document".into(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn post_update(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<PostUpdateRequest>,
) -> AppResult<Json<UpdateResponse>> {
    let device_id = auth
        .device_id
        .ok_or_else(|| AppError::BadRequest("Authenticated device_id is required".into()))?;
    if req.sender_device_id != device_id {
        return Err(AppError::BadRequest(
            "sender_device_id does not match authenticated device".into(),
        ));
    }
    db::verify_doc_write(&state.db, doc_id, auth.user_id).await?;
    db::verify_device_owner(&state.db, device_id, auth.user_id).await?;
    validate_aad_version(req.aad_version)?;
    let encrypted_update = decode_envelope(
        &req.encrypted_update_b64,
        16,
        MAX_UPDATE_BYTES,
        "encrypted update",
    )?;
    let nonce = decode_nonce(&req.nonce_b64)?;
    let stored = db::insert_doc_update_idempotent(
        &state.db,
        doc_id,
        device_id,
        req.update_id.into(),
        encrypted_update,
        nonce,
        req.aad_version,
    )
    .await?;
    if let Some(room) = state.doc_rooms.read().await.get(&doc_id).cloned() {
        let _ = room.send(
            ServerWsMessage::EncryptedUpdate {
                doc_id,
                sender_device_id: stored.sender_device_id,
                update_id: req.update_id,
                seq: stored.seq,
                encrypted_update_b64: base64_encode(&stored.encrypted_update),
                nonce_b64: base64_encode(&stored.nonce),
                aad_version: stored.aad_version,
            }
            .into(),
        );
    }
    Ok(Json(UpdateResponse {
        seq: stored.seq,
        created_at: stored.created_at,
    }))
}

async fn list_updates(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
    Query(params): Query<ListUpdatesParams>,
) -> AppResult<Json<PaginatedResponse<EncryptedUpdateResponse>>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;
    let after_seq = params.after_seq.unwrap_or(0);
    let through_seq = params.through_seq;
    let limit = params.limit.unwrap_or(500);
    if after_seq < 0 {
        return Err(AppError::BadRequest("after_seq cannot be negative".into()));
    }
    if through_seq.is_some_and(|through| through < after_seq) {
        return Err(AppError::BadRequest(
            "through_seq cannot be less than after_seq".into(),
        ));
    }
    if !(1..=1000).contains(&limit) {
        return Err(AppError::BadRequest(
            "limit must be between 1 and 1000".into(),
        ));
    }

    let rows = sqlx::query_as::<
        _,
        (
            i64,
            Uuid,
            Uuid,
            Vec<u8>,
            Vec<u8>,
            i32,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT seq, update_id, sender_device_id, encrypted_update, nonce, aad_version, created_at
         FROM doc_updates
         WHERE doc_id = $1 AND seq > $2 AND ($3::bigint IS NULL OR seq <= $3)
         ORDER BY seq ASC LIMIT $4",
    )
    .bind(doc_id)
    .bind(after_seq)
    .bind(through_seq)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM doc_updates
         WHERE doc_id = $1 AND seq > $2 AND ($3::bigint IS NULL OR seq <= $3)",
    )
    .bind(doc_id)
    .bind(after_seq)
    .bind(through_seq)
    .fetch_one(&state.db)
    .await?;
    let items = rows
        .into_iter()
        .map(|row| EncryptedUpdateResponse {
            seq: row.0,
            update_id: UpdateId(row.1),
            sender_device_id: row.2,
            encrypted_update_b64: base64_encode(&row.3),
            nonce_b64: base64_encode(&row.4),
            aad_version: row.5,
            created_at: row.6,
        })
        .collect();
    Ok(Json(PaginatedResponse { items, total }))
}

async fn get_snapshot(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
) -> AppResult<Json<SnapshotResponse>> {
    db::verify_doc_access(&state.db, doc_id, auth.user_id).await?;
    let row = sqlx::query_as::<_, (i64, Vec<u8>, Vec<u8>, i32, chrono::DateTime<chrono::Utc>)>(
        "SELECT base_seq, encrypted_snapshot, nonce, aad_version, created_at
         FROM doc_snapshots WHERE doc_id = $1 ORDER BY base_seq DESC LIMIT 1",
    )
    .bind(doc_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("No snapshot found".into()))?;
    Ok(Json(SnapshotResponse {
        base_seq: row.0,
        encrypted_snapshot_b64: base64_encode(&row.1),
        nonce_b64: base64_encode(&row.2),
        aad_version: row.3,
        created_at: row.4,
    }))
}

async fn post_snapshot(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<PostSnapshotRequest>,
) -> AppResult<Json<SnapshotResponse>> {
    db::verify_doc_write(&state.db, doc_id, auth.user_id).await?;
    if req.base_seq < 0 {
        return Err(AppError::BadRequest(
            "Snapshot base_seq cannot be negative".into(),
        ));
    }
    let current = db::current_seq(&state.db, doc_id).await?;
    if req.base_seq > current {
        return Err(AppError::BadRequest(format!(
            "Snapshot base_seq {} is ahead of document current_seq {}",
            req.base_seq, current
        )));
    }
    validate_aad_version(req.aad_version)?;
    let encrypted_snapshot = decode_envelope(
        &req.encrypted_snapshot_b64,
        16,
        MAX_SNAPSHOT_BYTES,
        "encrypted snapshot",
    )?;
    let nonce = decode_nonce(&req.nonce_b64)?;
    let created_at = sqlx::query_scalar(
        "INSERT INTO doc_snapshots (doc_id, base_seq, encrypted_snapshot, nonce, aad_version)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (doc_id, base_seq) DO UPDATE
         SET encrypted_snapshot = $3, nonce = $4, aad_version = $5
         RETURNING created_at",
    )
    .bind(doc_id)
    .bind(req.base_seq)
    .bind(&encrypted_snapshot)
    .bind(&nonce)
    .bind(req.aad_version)
    .fetch_one(&state.db)
    .await?;
    sqlx::query("UPDATE docs SET updated_at = NOW() WHERE id = $1")
        .bind(doc_id)
        .execute(&state.db)
        .await?;
    Ok(Json(SnapshotResponse {
        base_seq: req.base_seq,
        encrypted_snapshot_b64: req.encrypted_snapshot_b64,
        nonce_b64: req.nonce_b64,
        aad_version: req.aad_version,
        created_at,
    }))
}

async fn fetch_doc(state: &AppState, doc_id: Uuid) -> AppResult<DocRow> {
    sqlx::query_as(
        "SELECT id, workspace_id, encrypted_title, title_nonce, title_projection_seq,
                encrypted_path, path_nonce, path_projection_version,
                created_by, created_at, updated_at
         FROM docs WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Document not found".into()))
}

fn doc_response(row: DocRow) -> DocResponse {
    DocResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        encrypted_title_b64: row.encrypted_title.as_deref().map(base64_encode),
        title_nonce_b64: row.title_nonce.as_deref().map(base64_encode),
        title_projection_seq: row.title_projection_seq,
        encrypted_path_b64: base64_encode(&row.encrypted_path),
        path_nonce_b64: base64_encode(&row.path_nonce),
        path_projection_version: row.path_projection_version,
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
        current_seq: row.current_seq,
    }
}

fn base64_decode(input: &str) -> AppResult<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|_| AppError::BadRequest("Invalid base64".into()))
}

fn decode_nonce(input: &str) -> AppResult<Vec<u8>> {
    let nonce = base64_decode(input)?;
    if nonce.len() != NONCE_LEN {
        return Err(AppError::BadRequest(format!(
            "nonce must be exactly {NONCE_LEN} bytes"
        )));
    }
    Ok(nonce)
}

fn decode_envelope(input: &str, min: usize, max: usize, name: &str) -> AppResult<Vec<u8>> {
    if input.len() > max.saturating_mul(4).div_ceil(3) + 4 {
        return Err(AppError::BadRequest(format!("{name} exceeds size limit")));
    }
    let bytes = base64_decode(input)?;
    if !(min..=max).contains(&bytes.len()) {
        return Err(AppError::BadRequest(format!(
            "{name} must be between {min} and {max} bytes"
        )));
    }
    Ok(bytes)
}

fn validate_aad_version(version: i32) -> AppResult<()> {
    if version != 1 {
        return Err(AppError::BadRequest("Unsupported aad_version".into()));
    }
    Ok(())
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
        .route(
            "/api/docs/{doc_id}",
            get(get_doc)
                .patch(update_doc_path_projection)
                .delete(delete_doc),
        )
        .route(
            "/api/docs/{doc_id}/title",
            axum::routing::patch(update_doc_title_projection),
        )
        .route(
            "/api/docs/{doc_id}/updates",
            post(post_update).get(list_updates),
        )
        .route(
            "/api/docs/{doc_id}/snapshot",
            get(get_snapshot).post(post_snapshot),
        )
        .layer(DefaultBodyLimit::max(MAX_SNAPSHOT_BYTES * 4 / 3 + 4096))
}
