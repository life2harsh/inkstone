use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use uuid::Uuid;

use crate::auth::Auth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use inkstone_core::protocol::{
    CreateWorkspaceRequest, EncryptedGuestName, GuestJoinRequest, GuestJoinResponse,
    PaginatedResponse, PublicLinkAccessRequest, PublicLinkRequest, PublicLinkResponse,
    PutWorkspaceKeyEnvelopeRequest, UpdateWorkspaceRequest, WorkspaceKeyEnvelopeResponse,
    WorkspaceResponse,
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
    i64,
    bool,
    bool,
    i64,
);

const WORKSPACE_COLUMNS: &str = "w.id, w.encrypted_metadata, w.metadata_nonce, w.metadata_version,
     w.owner_id, w.created_at, w.updated_at,
     (SELECT COUNT(*) FROM workspace_members m JOIN users u ON u.id = m.user_id
       WHERE m.workspace_id = w.id AND u.public_link_workspace_id IS NULL
         AND u.guest_workspace_id IS NULL),
     EXISTS(SELECT 1 FROM users u WHERE u.public_link_workspace_id = w.id),
     w.link_guests_can_edit,
     (SELECT COUNT(*) FROM workspace_members m JOIN users u ON u.id = m.user_id
       WHERE m.workspace_id = w.id AND u.guest_workspace_id = w.id)";

fn workspace_response(row: WorkspaceRow) -> WorkspaceResponse {
    WorkspaceResponse {
        id: row.0,
        encrypted_metadata_b64: base64_encode(&row.1),
        metadata_nonce_b64: base64_encode(&row.2),
        metadata_version: row.3,
        owner_id: row.4,
        created_at: row.5,
        updated_at: row.6,
        member_count: row.7,
        public_link: row.8,
        public_link_editable: row.8 && row.9,
        guest_count: row.10,
    }
}

async fn load_workspace<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    workspace_id: Uuid,
) -> AppResult<WorkspaceResponse> {
    let row = sqlx::query_as::<_, WorkspaceRow>(&format!(
        "SELECT {WORKSPACE_COLUMNS} FROM workspaces w WHERE w.id = $1"
    ))
    .bind(workspace_id)
    .fetch_one(executor)
    .await?;
    Ok(workspace_response(row))
}

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

    Ok(Json(load_workspace(&state.db, workspace_id).await?))
}

async fn list_workspaces(
    State(state): State<AppState>,
    Auth(auth): Auth,
) -> AppResult<Json<PaginatedResponse<WorkspaceResponse>>> {
    let rows = sqlx::query_as::<_, WorkspaceRow>(&format!(
        "SELECT {WORKSPACE_COLUMNS}
         FROM workspaces w
         JOIN workspace_members wm ON wm.workspace_id = w.id
         WHERE wm.user_id = $1
         ORDER BY w.created_at"
    ))
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<WorkspaceResponse> = rows.into_iter().map(workspace_response).collect();

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
    if req.replace_version.is_none() && !matches!(role.as_str(), "owner" | "admin") {
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

    if let Some(expected) = req.replace_version {
        let version: i64 = sqlx::query_scalar(
            "UPDATE encrypted_workspace_keys
             SET encrypted_key = $3, nonce = $4, aad_version = $5,
                 version = version + 1, updated_at = NOW()
             WHERE workspace_id = $1 AND user_id = $2 AND version = $6
             RETURNING version",
        )
        .bind(workspace_id)
        .bind(auth.user_id)
        .bind(&encrypted_key)
        .bind(&nonce)
        .bind(req.aad_version)
        .bind(expected)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Conflict("Workspace key envelope changed".into()))?;
        return Ok(Json(WorkspaceKeyEnvelopeResponse {
            workspace_id,
            user_id: auth.user_id,
            encrypted_key_b64: base64_encode(&encrypted_key),
            nonce_b64: base64_encode(&nonce),
            aad_version: req.aad_version,
            version,
        }));
    }

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
    let stored: (Vec<u8>, Vec<u8>, i32, i64) = sqlx::query_as(
        "SELECT encrypted_key, nonce, aad_version, version FROM encrypted_workspace_keys
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
        version: stored.3,
    }))
}

async fn get_key_envelope(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<WorkspaceKeyEnvelopeResponse>> {
    db::verify_workspace_access(&state.db, workspace_id, auth.user_id).await?;
    let row = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, i32, i64)>(
        "SELECT encrypted_key, nonce, aad_version, version
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
        version: row.3,
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
    sqlx::query(
        "UPDATE workspaces SET encrypted_metadata=$1,metadata_nonce=$2,metadata_version=$3,updated_at=clock_timestamp()
         WHERE id=$4 AND metadata_version < $3",
    )
    .bind(metadata)
    .bind(nonce)
    .bind(req.metadata_version)
    .bind(workspace_id)
    .execute(&state.db)
    .await?;
    Ok(Json(load_workspace(&state.db, workspace_id).await?))
}

async fn require_workspace_admin(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    let role = db::workspace_role(&state.db, workspace_id, user_id).await?;
    if !role
        .as_deref()
        .is_some_and(|role| matches!(role, "owner" | "admin"))
    {
        return Err(AppError::Forbidden(
            "Workspace administrator access required".into(),
        ));
    }
    Ok(())
}

/// Replaces any earlier link, so turning it on again also rotates it.
async fn enable_public_link(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<PublicLinkRequest>,
) -> AppResult<Json<WorkspaceResponse>> {
    require_workspace_admin(&state, workspace_id, auth.user_id).await?;
    if !base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&req.token)
        .is_ok_and(|bytes| bytes.len() == 32)
    {
        return Err(AppError::BadRequest(
            "Public link token must be 32 random bytes".into(),
        ));
    }
    let link_user = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    let revoked = remove_link_users(&mut tx, workspace_id).await?;
    sqlx::query("UPDATE workspaces SET link_guests_can_edit = $2 WHERE id = $1")
        .bind(workspace_id)
        .bind(req.editable)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO users (id, public_link_workspace_id) VALUES ($1, $2)")
        .bind(link_user)
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO devices (id, user_id) VALUES ($1, $2)")
        .bind(device_id)
        .bind(link_user)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(workspace_id)
    .bind(link_user)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO auth_sessions (token_hash, user_id, device_id, expires_at)
         VALUES ($1, $2, $3, NOW() + INTERVAL '100 years')",
    )
    .bind(crate::auth::hash_secret(req.token.as_bytes()))
    .bind(link_user)
    .bind(device_id)
    .execute(&mut *tx)
    .await?;
    let workspace = load_workspace(&mut *tx, workspace_id).await?;
    tx.commit().await?;
    for hash in revoked {
        let _ = state.session_revocations.send(hash);
    }
    Ok(Json(workspace))
}

async fn remove_link_users(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
) -> AppResult<Vec<Vec<u8>>> {
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM auth_sessions s USING users u
         WHERE s.user_id = u.id
           AND (u.public_link_workspace_id = $1 OR u.guest_workspace_id = $1)
         RETURNING s.token_hash",
    )
    .bind(workspace_id)
    .fetch_all(&mut **tx)
    .await?;
    sqlx::query(
        "DELETE FROM workspace_members m USING users u
         WHERE m.user_id = u.id AND m.workspace_id = $1
           AND (u.public_link_workspace_id = $1 OR u.guest_workspace_id = $1)",
    )
    .bind(workspace_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE devices d SET revoked_at = NOW() FROM users u
         WHERE d.user_id = u.id AND u.guest_workspace_id = $1 AND d.revoked_at IS NULL",
    )
    .bind(workspace_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM users WHERE public_link_workspace_id = $1")
        .bind(workspace_id)
        .execute(&mut **tx)
        .await?;
    Ok(hashes)
}

async fn set_public_link_access(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<PublicLinkAccessRequest>,
) -> AppResult<Json<WorkspaceResponse>> {
    require_workspace_admin(&state, workspace_id, auth.user_id).await?;
    let mut tx = state.db.begin().await?;
    let linked: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE public_link_workspace_id = $1)",
    )
    .bind(workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    if !linked {
        return Err(AppError::NotFound("This vault has no public link".into()));
    }
    sqlx::query("UPDATE workspaces SET link_guests_can_edit = $2 WHERE id = $1")
        .bind(workspace_id)
        .bind(req.editable)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE workspace_members m SET role = $2 FROM users u
         WHERE m.user_id = u.id AND m.workspace_id = $1 AND u.guest_workspace_id = $1",
    )
    .bind(workspace_id)
    .bind(if req.editable { "editor" } else { "viewer" })
    .execute(&mut *tx)
    .await?;
    let workspace = load_workspace(&mut *tx, workspace_id).await?;
    tx.commit().await?;
    Ok(Json(workspace))
}

const MAX_GUESTS: i64 = 500;

async fn join_as_guest(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Json(req): Json<GuestJoinRequest>,
) -> AppResult<Json<GuestJoinResponse>> {
    let workspace_id = auth
        .public_link
        .ok_or_else(|| AppError::Forbidden("Open the shared link to join".into()))?;
    let name = decode_bounded(&req.name.ciphertext_b64, 17, 512, "encrypted name")?;
    let nonce = decode_bounded(&req.name.nonce_b64, 12, 12, "name nonce")?;
    let mut tx = state.db.begin().await?;
    let (editable, guests): (bool, i64) = sqlx::query_as(
        "SELECT w.link_guests_can_edit,
                (SELECT COUNT(*) FROM workspace_members m JOIN users u ON u.id = m.user_id
                  WHERE m.workspace_id = w.id AND u.guest_workspace_id = w.id)
         FROM workspaces w WHERE w.id = $1 FOR UPDATE",
    )
    .bind(workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    if !editable {
        return Err(AppError::Forbidden("This link is view only".into()));
    }
    if guests >= MAX_GUESTS {
        return Err(AppError::Conflict(
            "This vault has too many guests. Ask the owner to reset the link.".into(),
        ));
    }
    let user_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, guest_workspace_id, encrypted_guest_name, guest_name_nonce)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(workspace_id)
    .bind(&name)
    .bind(&nonce)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO devices (id, user_id) VALUES ($1, $2)")
        .bind(device_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'editor')",
    )
    .bind(workspace_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    let token = crate::auth::new_secret();
    sqlx::query(
        "INSERT INTO auth_sessions (token_hash, user_id, device_id, expires_at)
         VALUES ($1, $2, $3, NOW() + INTERVAL '1 year')",
    )
    .bind(crate::auth::hash_secret(token.as_bytes()))
    .bind(user_id)
    .bind(device_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(GuestJoinResponse {
        token,
        user_id,
        device_id,
    }))
}

async fn disable_public_link(
    State(state): State<AppState>,
    Auth(auth): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<WorkspaceResponse>> {
    require_workspace_admin(&state, workspace_id, auth.user_id).await?;
    let mut tx = state.db.begin().await?;
    let hashes = remove_link_users(&mut tx, workspace_id).await?;
    sqlx::query("UPDATE workspaces SET link_guests_can_edit = FALSE WHERE id = $1")
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    for hash in hashes {
        let _ = state.session_revocations.send(hash);
    }
    Ok(Json(load_workspace(&state.db, workspace_id).await?))
}

async fn public_link_details(
    State(state): State<AppState>,
    Auth(auth): Auth,
) -> AppResult<Json<PublicLinkResponse>> {
    let workspace_id = auth
        .public_link
        .or(auth.guest)
        .ok_or_else(|| AppError::Forbidden("Not a public link".into()))?;
    let device_id = auth
        .device_id
        .ok_or_else(|| AppError::Unauthorized("Public link has no device".into()))?;
    let guest = if auth.guest.is_some() {
        let (name, nonce): (Vec<u8>, Vec<u8>) = sqlx::query_as(
            "SELECT encrypted_guest_name, guest_name_nonce FROM users WHERE id = $1",
        )
        .bind(auth.user_id)
        .fetch_one(&state.db)
        .await?;
        Some(EncryptedGuestName {
            ciphertext_b64: base64_encode(&name),
            nonce_b64: base64_encode(&nonce),
        })
    } else {
        None
    };
    Ok(Json(PublicLinkResponse {
        workspace: load_workspace(&state.db, workspace_id).await?,
        user_id: auth.user_id,
        device_id,
        guest,
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
        .route(
            "/api/workspaces/{workspace_id}/public-link",
            put(enable_public_link)
                .patch(set_public_link_access)
                .delete(disable_public_link),
        )
        .route("/api/public-link", get(public_link_details))
        .route("/api/public-link/guests", post(join_as_guest))
}
