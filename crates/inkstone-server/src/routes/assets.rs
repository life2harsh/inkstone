use axum::extract::{DefaultBodyLimit, Path, State};
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
    ASSET_AAD_VERSION, AssetChunkResponse, AssetResponse, CreateAssetRequest, MAX_ASSET_CHUNKS,
    MAX_ASSET_ENCRYPTED_BYTES, PaginatedResponse, PutAssetChunkRequest, UpdateAssetMetadataRequest,
};

const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_CHUNK_CIPHERTEXT: usize = 1024 * 1024 + 16;

#[derive(sqlx::FromRow)]
struct AssetRow {
    id: Uuid,
    workspace_id: Uuid,
    version: i64,
    chunk_count: i32,
    encrypted_size: i64,
    encrypted_metadata: Vec<u8>,
    metadata_nonce: Vec<u8>,
    aad_version: i32,
    metadata_version: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    complete: bool,
}

async fn create_asset(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(request): Json<CreateAssetRequest>,
) -> AppResult<Json<AssetResponse>> {
    db::verify_workspace_write(&state.db, workspace_id, auth.user_id).await?;
    validate_header(&request)?;
    let metadata = decode_bounded(
        &request.encrypted_metadata_b64,
        16,
        MAX_METADATA_BYTES,
        "encrypted metadata",
    )?;
    let nonce = decode_nonce(&request.metadata_nonce_b64)?;
    sqlx::query(
        "INSERT INTO assets (id, workspace_id, version, chunk_count, encrypted_size,
            encrypted_metadata, metadata_nonce, aad_version, uploaded_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (id) DO NOTHING",
    )
    .bind(request.asset_id)
    .bind(workspace_id)
    .bind(request.version)
    .bind(request.chunk_count as i32)
    .bind(request.encrypted_size as i64)
    .bind(metadata)
    .bind(nonce)
    .bind(request.aad_version)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;
    let row = fetch_asset(&state, request.asset_id).await?;
    if row.workspace_id != workspace_id || row.version != request.version {
        return Err(AppError::Conflict("asset_id is already in use".into()));
    }
    Ok(Json(asset_response(row)))
}

async fn list_assets(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<AssetResponse>>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;
    let rows = sqlx::query_as::<_, AssetRow>(ASSET_SELECT)
        .bind(workspace_id)
        .fetch_all(&state.db)
        .await?;
    let items = rows.into_iter().map(asset_response).collect::<Vec<_>>();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn put_chunk(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path((asset_id, version, index)): Path<(Uuid, i64, u32)>,
    Json(request): Json<PutAssetChunkRequest>,
) -> AppResult<StatusCode> {
    let asset = fetch_asset(&state, asset_id).await?;
    db::verify_workspace_write(&state.db, asset.workspace_id, auth.user_id).await?;
    if version != asset.version
        || index >= asset.chunk_count as u32
        || request.aad_version != ASSET_AAD_VERSION
    {
        return Err(AppError::BadRequest(
            "invalid asset chunk identity or version".into(),
        ));
    }
    let ciphertext = decode_bounded(
        &request.ciphertext_b64,
        16,
        MAX_CHUNK_CIPHERTEXT,
        "asset chunk",
    )?;
    let nonce = decode_nonce(&request.nonce_b64)?;
    let inserted = sqlx::query(
        "INSERT INTO asset_chunks(asset_id,asset_version,chunk_index,ciphertext,nonce,aad_version)
         VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
    )
    .bind(asset_id)
    .bind(version)
    .bind(index as i32)
    .bind(&ciphertext)
    .bind(&nonce)
    .bind(request.aad_version)
    .execute(&state.db)
    .await?;
    if inserted.rows_affected() == 0 {
        let stored: (Vec<u8>, Vec<u8>, i32) = sqlx::query_as(
            "SELECT ciphertext,nonce,aad_version FROM asset_chunks WHERE asset_id=$1 AND asset_version=$2 AND chunk_index=$3",
        ).bind(asset_id).bind(version).bind(index as i32).fetch_one(&state.db).await?;
        if stored != (ciphertext, nonce, request.aad_version) {
            return Err(AppError::Conflict(
                "asset chunk already exists with different opaque bytes".into(),
            ));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_chunk(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path((asset_id, version, index)): Path<(Uuid, i64, u32)>,
) -> AppResult<Json<AssetChunkResponse>> {
    let asset = fetch_asset(&state, asset_id).await?;
    db::verify_workspace_access(&state.db, asset.workspace_id, auth.user_id).await?;
    let row: (Vec<u8>, Vec<u8>, i32) = sqlx::query_as(
        "SELECT ciphertext,nonce,aad_version FROM asset_chunks WHERE asset_id=$1 AND asset_version=$2 AND chunk_index=$3",
    ).bind(asset_id).bind(version).bind(index as i32).fetch_optional(&state.db).await?
      .ok_or_else(|| AppError::NotFound("asset chunk not found".into()))?;
    Ok(Json(AssetChunkResponse {
        asset_id,
        version,
        chunk_index: index,
        ciphertext_b64: encode(&row.0),
        nonce_b64: encode(&row.1),
        aad_version: row.2,
    }))
}

async fn delete_asset(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(asset_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let workspace_id: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM assets WHERE id=$1")
            .bind(asset_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(workspace_id) = workspace_id else {
        return Ok(StatusCode::NO_CONTENT);
    };
    db::verify_workspace_write(&state.db, workspace_id, auth.user_id).await?;
    sqlx::query("DELETE FROM assets WHERE id=$1")
        .bind(asset_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_metadata(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(asset_id): Path<Uuid>,
    Json(request): Json<UpdateAssetMetadataRequest>,
) -> AppResult<Json<AssetResponse>> {
    let current = fetch_asset(&state, asset_id).await?;
    db::verify_workspace_write(&state.db, current.workspace_id, auth.user_id).await?;
    if request.metadata_version <= 1 || request.aad_version != ASSET_AAD_VERSION {
        return Err(AppError::BadRequest(
            "invalid asset metadata version".into(),
        ));
    }
    let metadata = decode_bounded(
        &request.encrypted_metadata_b64,
        16,
        MAX_METADATA_BYTES,
        "encrypted metadata",
    )?;
    let nonce = decode_nonce(&request.metadata_nonce_b64)?;
    sqlx::query(
        "UPDATE assets SET encrypted_metadata=$1,metadata_nonce=$2,metadata_version=$3,updated_at=clock_timestamp()
         WHERE id=$4 AND metadata_version < $3",
    ).bind(metadata).bind(nonce).bind(request.metadata_version).bind(asset_id)
      .execute(&state.db).await?;
    Ok(Json(asset_response(fetch_asset(&state, asset_id).await?)))
}

const ASSET_SELECT: &str =
    "SELECT a.id,a.workspace_id,a.version,a.chunk_count,a.encrypted_size,a.encrypted_metadata,
     a.metadata_nonce,a.aad_version,a.metadata_version,a.created_at,a.updated_at,
     ((SELECT COUNT(*) FROM asset_chunks c WHERE c.asset_id=a.id AND c.asset_version=a.version)=a.chunk_count) AS complete
     FROM assets a WHERE a.workspace_id=$1 ORDER BY a.updated_at DESC";

async fn fetch_asset(state: &AppState, id: Uuid) -> AppResult<AssetRow> {
    sqlx::query_as::<_, AssetRow>(&ASSET_SELECT.replace("a.workspace_id=$1", "a.id=$1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("asset not found".into()))
}

fn asset_response(row: AssetRow) -> AssetResponse {
    AssetResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        version: row.version,
        chunk_count: row.chunk_count as u32,
        encrypted_size: row.encrypted_size as u64,
        encrypted_metadata_b64: encode(&row.encrypted_metadata),
        metadata_nonce_b64: encode(&row.metadata_nonce),
        aad_version: row.aad_version,
        metadata_version: row.metadata_version,
        complete: row.complete,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn validate_header(request: &CreateAssetRequest) -> AppResult<()> {
    if request.version <= 0
        || !(1..=MAX_ASSET_CHUNKS).contains(&request.chunk_count)
        || !(16..=MAX_ASSET_ENCRYPTED_BYTES).contains(&request.encrypted_size)
        || request.aad_version != ASSET_AAD_VERSION
    {
        return Err(AppError::BadRequest(
            "invalid asset limits or version".into(),
        ));
    }
    Ok(())
}

fn decode_nonce(value: &str) -> AppResult<Vec<u8>> {
    decode_bounded(value, 12, 12, "nonce")
}
fn decode_bounded(value: &str, min: usize, max: usize, label: &str) -> AppResult<Vec<u8>> {
    if value.len() > max.saturating_mul(4).div_ceil(3) + 4 {
        return Err(AppError::BadRequest(format!("{label} exceeds limit")));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| AppError::BadRequest(format!("invalid {label} base64")))?;
    if !(min..=max).contains(&bytes.len()) {
        return Err(AppError::BadRequest(format!("invalid {label} size")));
    }
    Ok(bytes)
}
fn encode(value: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(value)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/assets",
            post(create_asset).get(list_assets),
        )
        .route(
            "/api/assets/{asset_id}",
            axum::routing::patch(update_metadata).delete(delete_asset),
        )
        .route(
            "/api/assets/{asset_id}/versions/{version}/chunks/{index}",
            put(put_chunk).get(get_chunk),
        )
        .layer(DefaultBodyLimit::max(MAX_CHUNK_CIPHERTEXT * 4 / 3 + 4096))
}
