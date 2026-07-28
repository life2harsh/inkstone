use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use base64::Engine;
use inkstone_core::protocol::{
    PaginatedResponse, PathProjectionResponse, PutPathProjectionRequest,
    WorkspaceTreeMutationRequest,
};
use uuid::Uuid;

use crate::auth::Auth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(sqlx::FromRow)]
struct ProjectionRow {
    id: Uuid,
    workspace_id: Uuid,
    entry_kind: String,
    encrypted_path: Vec<u8>,
    path_nonce: Vec<u8>,
    projection_version: i64,
    aad_version: i32,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

async fn list(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<PathProjectionResponse>>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;
    let rows = sqlx::query_as::<_, ProjectionRow>(
        "SELECT id,workspace_id,entry_kind,encrypted_path,path_nonce,projection_version,aad_version,created_at,updated_at
         FROM workspace_path_projections WHERE workspace_id=$1 ORDER BY id",
    ).bind(workspace_id).fetch_all(&state.db).await?;
    let items = rows.into_iter().map(response).collect::<Vec<_>>();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn put(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path((workspace_id, entry_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<PutPathProjectionRequest>,
) -> AppResult<Json<PathProjectionResponse>> {
    db::verify_workspace_write(&state.db, workspace_id, auth.user_id).await?;
    if request.entry_kind != "folder" || request.projection_version <= 0 || request.aad_version != 1
    {
        return Err(AppError::BadRequest("invalid path projection".into()));
    }
    let path = decode(&request.encrypted_path_b64, 16, 16 * 1024, "encrypted path")?;
    let nonce = decode(&request.path_nonce_b64, 12, 12, "path nonce")?;
    sqlx::query(
        "INSERT INTO workspace_path_projections(id,workspace_id,entry_kind,encrypted_path,path_nonce,projection_version,aad_version,created_by)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT(id) DO UPDATE SET encrypted_path=EXCLUDED.encrypted_path,path_nonce=EXCLUDED.path_nonce,
             projection_version=EXCLUDED.projection_version,aad_version=EXCLUDED.aad_version,updated_at=clock_timestamp()
         WHERE workspace_path_projections.workspace_id=EXCLUDED.workspace_id
           AND workspace_path_projections.entry_kind=EXCLUDED.entry_kind
           AND workspace_path_projections.projection_version < EXCLUDED.projection_version",
    ).bind(entry_id).bind(workspace_id).bind(&request.entry_kind).bind(path).bind(nonce)
      .bind(request.projection_version).bind(request.aad_version).bind(auth.user_id)
      .execute(&state.db).await?;
    let row = fetch(&state, entry_id).await?;
    if row.workspace_id != workspace_id || row.entry_kind != request.entry_kind {
        return Err(AppError::Conflict(
            "projection identity is already in use".into(),
        ));
    }
    Ok(Json(response(row)))
}

async fn delete(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path((workspace_id, entry_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    db::verify_workspace_write(&state.db, workspace_id, auth.user_id).await?;
    sqlx::query("DELETE FROM workspace_path_projections WHERE id=$1 AND workspace_id=$2")
        .bind(entry_id)
        .bind(workspace_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mutate_tree(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(request): Json<WorkspaceTreeMutationRequest>,
) -> AppResult<StatusCode> {
    db::verify_workspace_write(&state.db, workspace_id, auth.user_id).await?;
    let total = request.doc_updates.len()
        + request.asset_updates.len()
        + request.projection_updates.len()
        + request.delete_doc_ids.len()
        + request.delete_asset_ids.len()
        + request.delete_projection_ids.len();
    if total == 0 || total > 1000 {
        return Err(AppError::BadRequest(
            "tree mutation must contain 1-1000 changes".into(),
        ));
    }
    let mut doc_updates = Vec::new();
    for update in request.doc_updates {
        if update.projection.projection_version <= 0 {
            return Err(AppError::BadRequest(
                "invalid document projection version".into(),
            ));
        }
        doc_updates.push((
            update.doc_id,
            decode(
                &update.projection.encrypted_path_b64,
                16,
                16384,
                "encrypted path",
            )?,
            decode(&update.projection.path_nonce_b64, 12, 12, "path nonce")?,
            update.projection.projection_version,
        ));
    }
    let mut asset_updates = Vec::new();
    for update in request.asset_updates {
        if update.metadata.metadata_version <= 1 || update.metadata.aad_version != 1 {
            return Err(AppError::BadRequest(
                "invalid asset metadata version".into(),
            ));
        }
        asset_updates.push((
            update.asset_id,
            decode(
                &update.metadata.encrypted_metadata_b64,
                16,
                65536,
                "encrypted metadata",
            )?,
            decode(
                &update.metadata.metadata_nonce_b64,
                12,
                12,
                "metadata nonce",
            )?,
            update.metadata.metadata_version,
        ));
    }
    let mut projection_updates = Vec::new();
    for update in request.projection_updates {
        if update.projection.entry_kind != "folder"
            || update.projection.projection_version <= 0
            || update.projection.aad_version != 1
        {
            return Err(AppError::BadRequest("invalid folder projection".into()));
        }
        projection_updates.push((
            update.entry_id,
            decode(
                &update.projection.encrypted_path_b64,
                16,
                16384,
                "encrypted path",
            )?,
            decode(&update.projection.path_nonce_b64, 12, 12, "path nonce")?,
            update.projection.projection_version,
        ));
    }
    let mut tx = state.db.begin().await?;
    for (id, path, nonce, version) in doc_updates {
        let result = sqlx::query("UPDATE docs SET encrypted_path=$1,path_nonce=$2,path_projection_version=$3,updated_at=clock_timestamp() WHERE id=$4 AND workspace_id=$5 AND path_projection_version < $3")
            .bind(path).bind(nonce).bind(version).bind(id).bind(workspace_id).execute(&mut *tx).await?;
        if result.rows_affected() == 0 {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM docs WHERE id=$1 AND workspace_id=$2)",
            )
            .bind(id)
            .bind(workspace_id)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(AppError::NotFound("tree document not found".into()));
            }
        }
    }
    for (id, metadata, nonce, version) in asset_updates {
        let result = sqlx::query("UPDATE assets SET encrypted_metadata=$1,metadata_nonce=$2,metadata_version=$3,updated_at=clock_timestamp() WHERE id=$4 AND workspace_id=$5 AND metadata_version < $3")
            .bind(metadata).bind(nonce).bind(version).bind(id).bind(workspace_id).execute(&mut *tx).await?;
        if result.rows_affected() == 0 {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM assets WHERE id=$1 AND workspace_id=$2)",
            )
            .bind(id)
            .bind(workspace_id)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(AppError::NotFound("tree asset not found".into()));
            }
        }
    }
    for (id, path, nonce, version) in projection_updates {
        let result = sqlx::query("UPDATE workspace_path_projections SET encrypted_path=$1,path_nonce=$2,projection_version=$3,updated_at=clock_timestamp() WHERE id=$4 AND workspace_id=$5 AND projection_version < $3")
            .bind(path).bind(nonce).bind(version).bind(id).bind(workspace_id).execute(&mut *tx).await?;
        if result.rows_affected() == 0 {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_path_projections WHERE id=$1 AND workspace_id=$2)").bind(id).bind(workspace_id).fetch_one(&mut *tx).await?;
            if !exists {
                return Err(AppError::NotFound("tree folder not found".into()));
            }
        }
    }
    for (table, ids) in [
        ("docs", request.delete_doc_ids),
        ("assets", request.delete_asset_ids),
        ("workspace_path_projections", request.delete_projection_ids),
    ] {
        for id in ids {
            let query = format!("DELETE FROM {table} WHERE id=$1 AND workspace_id=$2");
            sqlx::query(&query)
                .bind(id)
                .bind(workspace_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn fetch(state: &AppState, id: Uuid) -> AppResult<ProjectionRow> {
    sqlx::query_as(
        "SELECT id,workspace_id,entry_kind,encrypted_path,path_nonce,projection_version,aad_version,created_at,updated_at
         FROM workspace_path_projections WHERE id=$1",
    ).bind(id).fetch_optional(&state.db).await?
      .ok_or_else(|| AppError::NotFound("path projection not found".into()))
}

fn response(row: ProjectionRow) -> PathProjectionResponse {
    PathProjectionResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        entry_kind: row.entry_kind,
        encrypted_path_b64: encode(&row.encrypted_path),
        path_nonce_b64: encode(&row.path_nonce),
        projection_version: row.projection_version,
        aad_version: row.aad_version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn decode(value: &str, min: usize, max: usize, label: &str) -> AppResult<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| AppError::BadRequest(format!("invalid {label} base64")))?;
    if !(min..=max).contains(&bytes.len()) {
        return Err(AppError::BadRequest(format!("invalid {label} size")));
    }
    Ok(bytes)
}
fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/path-projections",
            axum::routing::get(list),
        )
        .route(
            "/api/workspaces/{workspace_id}/path-projections/{entry_id}",
            axum::routing::put(put).delete(delete),
        )
        .route(
            "/api/workspaces/{workspace_id}/tree-mutations",
            axum::routing::post(mutate_tree),
        )
}
