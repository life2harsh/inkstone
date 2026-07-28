use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use base64::Engine;
use inkstone_core::protocol::{
    ApproveInvitationKeyRequest, CreateInvitationRequest, CreateInvitationResponse,
    GenericMessageResponse, InvitationKeyRequestResponse, InvitationResponse,
    OutgoingInvitationResponse, PaginatedResponse, SubmitInvitationKeyRequest,
    TransferOwnershipRequest, UpdateMemberRoleRequest, WorkspaceMemberResponse,
};
use uuid::Uuid;

use crate::auth::{self, Auth};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const ROLES: &[&str] = &["owner", "admin", "editor", "viewer"];
const MAX_KEY_ENVELOPE_BYTES: usize = 64 * 1024;

async fn require_admin(state: &AppState, workspace_id: Uuid, user_id: Uuid) -> AppResult<String> {
    let role = db::workspace_role(&state.db, workspace_id, user_id)
        .await?
        .ok_or_else(|| AppError::Forbidden("Workspace administrator access required".into()))?;
    if !matches!(role.as_str(), "owner" | "admin") {
        return Err(AppError::Forbidden(
            "Workspace administrator access required".into(),
        ));
    }
    Ok(role)
}

async fn create_invitation(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateInvitationRequest>,
) -> AppResult<Json<CreateInvitationResponse>> {
    let actor_role = require_admin(&state, workspace_id, user.user_id.into()).await?;
    validate_role_change(&actor_role, &req.role)?;
    let email = auth::normalize_email(&req.email)?;
    let expires = req.expires_in_hours.unwrap_or(168);
    if !(1..=720).contains(&expires) {
        return Err(AppError::BadRequest(
            "Invitation expiry must be between 1 and 720 hours".into(),
        ));
    }
    let already_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_members wm JOIN users u ON u.id = wm.user_id
         WHERE wm.workspace_id = $1 AND LOWER(u.email) = $2)",
    )
    .bind(workspace_id)
    .bind(&email)
    .fetch_one(&state.db)
    .await?;
    if already_member {
        return Err(AppError::Conflict(
            "User is already a workspace member".into(),
        ));
    }

    let token = auth::new_secret();
    let id = Uuid::new_v4();
    let delivery_key = Uuid::new_v4();
    let expires_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "INSERT INTO invitations
           (id, workspace_id, inviter_id, invitee_email, role, token_hash, expires_at,
            delivery_idempotency_key, last_sent_at)
         VALUES ($1,$2,$3,$4,$5,$6,NOW()+make_interval(hours => $7::int),$8,NOW())
         RETURNING expires_at",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(user.user_id)
    .bind(&email)
    .bind(&req.role)
    .bind(auth::hash_secret(token.as_bytes()))
    .bind(expires as i32)
    .bind(delivery_key)
    .fetch_one(&state.db)
    .await
    .map_err(unique_pending)?;
    send_invitation(
        &state,
        &email,
        workspace_id,
        &token,
        expires_at,
        delivery_key,
    )
    .await?;
    Ok(Json(CreateInvitationResponse {
        id,
        workspace_id,
        email,
        role: req.role,
        expires_at,
        token: if state.config.dev_auth || state.config.resend_api_key.is_none() {
            token
        } else {
            String::new()
        },
    }))
}

fn unique_pending(error: sqlx::Error) -> AppError {
    if error.as_database_error().and_then(|e| e.code()).as_deref() == Some("23505") {
        AppError::Conflict("A pending invitation already exists".into())
    } else {
        error.into()
    }
}

async fn send_invitation(
    state: &AppState,
    email: &str,
    workspace_id: Uuid,
    token: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
    key: Uuid,
) -> AppResult<()> {
    let link = format!(
        "{}/#invitation={token}",
        state.config.public_base_url.trim_end_matches('/')
    );
    crate::email::send(&state.config, email, "You are invited to Inkstone", &format!(
        "You were invited to workspace {workspace_id}.\n\n{link}\n\nThis token-only link expires at {}. Workspace keys are transferred separately after you upload a client-generated public key.",
        expires_at.to_rfc3339()), key).await
}

async fn list_incoming(
    State(state): State<AppState>,
    Auth(user): Auth,
) -> AppResult<Json<PaginatedResponse<InvitationResponse>>> {
    let email = user
        .email
        .ok_or_else(|| AppError::BadRequest("Development user has no email".into()))?;
    let rows = sqlx::query_as::<_, (Uuid,Uuid,String,Uuid,String,String,chrono::DateTime<chrono::Utc>)>(
        "SELECT i.id,i.workspace_id,'Encrypted workspace',i.inviter_id,i.invitee_email,i.role,i.expires_at
         FROM invitations i JOIN workspaces w ON w.id=i.workspace_id
         WHERE LOWER(i.invitee_email)=$1 AND i.accepted=FALSE AND i.revoked_at IS NULL AND i.expires_at>NOW()
         ORDER BY i.created_at DESC",
    ).bind(email).fetch_all(&state.db).await?;
    let items = rows
        .into_iter()
        .map(|r| InvitationResponse {
            id: r.0,
            workspace_id: r.1,
            workspace_name: r.2,
            inviter_id: r.3,
            email: r.4,
            role: r.5,
            expires_at: r.6,
        })
        .collect::<Vec<_>>();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn list_outgoing(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<OutgoingInvitationResponse>>> {
    require_admin(&state, workspace_id, user.user_id.into()).await?;
    let rows = sqlx::query_as::<_, (Uuid,String,String,chrono::DateTime<chrono::Utc>,chrono::DateTime<chrono::Utc>,Option<chrono::DateTime<chrono::Utc>>,bool,Option<chrono::DateTime<chrono::Utc>>,bool)>(
        "SELECT i.id,i.invitee_email,i.role,i.expires_at,i.created_at,i.last_sent_at,i.accepted,i.revoked_at,
                EXISTS(SELECT 1 FROM invitation_key_requests kr WHERE kr.invitation_id=i.id AND kr.approved_at IS NOT NULL)
         FROM invitations i WHERE i.workspace_id=$1 ORDER BY i.created_at DESC",
    ).bind(workspace_id).fetch_all(&state.db).await?;
    let items = rows
        .into_iter()
        .map(|r| {
            let state_name = if r.7.is_some() {
                "revoked"
            } else if r.8 {
                "active"
            } else if r.6 {
                "awaiting_key"
            } else if r.3 <= chrono::Utc::now() {
                "expired"
            } else {
                "pending"
            };
            OutgoingInvitationResponse {
                id: r.0,
                workspace_id,
                email: r.1,
                role: r.2,
                expires_at: r.3,
                created_at: r.4,
                last_sent_at: r.5,
                state: state_name.into(),
            }
        })
        .collect::<Vec<_>>();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn resend(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path((workspace_id, id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<GenericMessageResponse>> {
    require_admin(&state, workspace_id, user.user_id.into()).await?;
    let token = auth::new_secret();
    let delivery = Uuid::new_v4();
    let row = sqlx::query_as::<_, (String,chrono::DateTime<chrono::Utc>)>(
        "UPDATE invitations SET token_hash=$1,last_sent_at=NOW(),delivery_idempotency_key=$2
         WHERE id=$3 AND workspace_id=$4 AND accepted=FALSE AND revoked_at IS NULL AND expires_at>NOW()
         RETURNING invitee_email,expires_at",
    ).bind(auth::hash_secret(token.as_bytes())).bind(delivery).bind(id).bind(workspace_id)
      .fetch_optional(&state.db).await?.ok_or_else(|| AppError::Conflict("Invitation cannot be resent".into()))?;
    send_invitation(&state, &row.0, workspace_id, &token, row.1, delivery).await?;
    Ok(Json(GenericMessageResponse {
        message: "Invitation resent.".into(),
    }))
}

async fn revoke(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path((workspace_id, id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    require_admin(&state, workspace_id, user.user_id.into()).await?;
    let changed=sqlx::query("UPDATE invitations SET revoked_at=NOW(),token_hash=digest(gen_random_uuid()::text,'sha256')
        WHERE id=$1 AND workspace_id=$2 AND revoked_at IS NULL
          AND NOT EXISTS(SELECT 1 FROM invitation_key_requests kr WHERE kr.invitation_id=invitations.id AND kr.approved_at IS NOT NULL)")
        .bind(id).bind(workspace_id).execute(&state.db).await?;
    if changed.rows_affected() != 1 {
        return Err(AppError::Conflict("Invitation cannot be revoked".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn submit_key_request(
    State(state): State<AppState>,
    Auth(user): Auth,
    Json(req): Json<SubmitInvitationKeyRequest>,
) -> AppResult<Json<InvitationKeyRequestResponse>> {
    let email = user
        .email
        .ok_or_else(|| AppError::BadRequest("A verified account email is required".into()))?;
    let token_hash = auth::validate_secret_token(&req.token, "invitation")?;
    if req.key_version <= 0 {
        return Err(AppError::BadRequest("Invalid public key version".into()));
    }
    let public_key = decode(&req.public_key_b64, 32, 32, "public key")?;
    let mut tx = state.db.begin().await?;
    let invitation=sqlx::query_as::<_,(Uuid,Uuid,String,String,chrono::DateTime<chrono::Utc>,bool,Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id,workspace_id,invitee_email,role,expires_at,accepted,revoked_at FROM invitations WHERE token_hash=$1 FOR UPDATE",
    ).bind(token_hash).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::NotFound("Invitation not found".into()))?;
    if invitation.1 != req.workspace_id
        || invitation.2.to_lowercase() != email
        || invitation.4 <= chrono::Utc::now()
        || invitation.6.is_some()
    {
        return Err(AppError::Forbidden("Invalid or expired invitation".into()));
    }
    if invitation.5 {
        return Err(AppError::Conflict(
            "Invitation token has already been used".into(),
        ));
    }
    let id = Uuid::new_v4();
    let requested_at:chrono::DateTime<chrono::Utc>=sqlx::query_scalar(
        "INSERT INTO invitation_key_requests(id,invitation_id,workspace_id,user_id,public_key,key_version)
         VALUES($1,$2,$3,$4,$5,$6) RETURNING requested_at",
    ).bind(id).bind(invitation.0).bind(invitation.1).bind(user.user_id).bind(&public_key).bind(req.key_version)
      .fetch_one(&mut *tx).await.map_err(unique_pending)?;
    sqlx::query(
        "UPDATE invitations SET accepted=TRUE,accepted_at=NOW(),accepted_by=$1 WHERE id=$2",
    )
    .bind(user.user_id)
    .bind(invitation.0)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(InvitationKeyRequestResponse {
        id,
        invitation_id: invitation.0,
        workspace_id: invitation.1,
        user_id: user.user_id.into(),
        email,
        role: invitation.3,
        public_key_b64: encode(&public_key),
        key_version: req.key_version,
        requested_at,
    }))
}

async fn list_key_requests(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<InvitationKeyRequestResponse>>> {
    require_admin(&state, workspace_id, user.user_id.into()).await?;
    let rows=sqlx::query_as::<_,(Uuid,Uuid,Uuid,String,String,Vec<u8>,i32,chrono::DateTime<chrono::Utc>)>(
        "SELECT kr.id,kr.invitation_id,kr.user_id,u.email,i.role,kr.public_key,kr.key_version,kr.requested_at
         FROM invitation_key_requests kr JOIN invitations i ON i.id=kr.invitation_id JOIN users u ON u.id=kr.user_id
         WHERE kr.workspace_id=$1 AND kr.approved_at IS NULL AND i.revoked_at IS NULL
         ORDER BY kr.requested_at",
    ).bind(workspace_id).fetch_all(&state.db).await?;
    let items = rows
        .into_iter()
        .map(|r| InvitationKeyRequestResponse {
            id: r.0,
            invitation_id: r.1,
            workspace_id,
            user_id: r.2,
            email: r.3,
            role: r.4,
            public_key_b64: encode(&r.5),
            key_version: r.6,
            requested_at: r.7,
        })
        .collect::<Vec<_>>();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn approve(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path((workspace_id, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ApproveInvitationKeyRequest>,
) -> AppResult<Json<WorkspaceMemberResponse>> {
    let actor_role = require_admin(&state, workspace_id, user.user_id.into()).await?;
    let encrypted = decode(
        &req.encrypted_key_b64,
        16,
        MAX_KEY_ENVELOPE_BYTES,
        "encrypted key envelope",
    )?;
    let nonce = decode(&req.nonce_b64, 12, 12, "nonce")?;
    if req.aad_version != 1 {
        return Err(AppError::BadRequest("Unsupported aad_version".into()));
    }
    let mut tx = state.db.begin().await?;
    let row=sqlx::query_as::<_,(Uuid,String,String)>(
        "SELECT kr.user_id,i.role,u.email FROM invitation_key_requests kr JOIN invitations i ON i.id=kr.invitation_id
         JOIN users u ON u.id=kr.user_id WHERE kr.id=$1 AND kr.workspace_id=$2
           AND kr.approved_at IS NULL AND i.revoked_at IS NULL FOR UPDATE OF kr",
    ).bind(id).bind(workspace_id).fetch_optional(&mut *tx).await?.ok_or_else(||AppError::NotFound("Pending key request not found".into()))?;
    validate_role_change(&actor_role, &row.1)?;
    let joined_at:chrono::DateTime<chrono::Utc>=sqlx::query_scalar(
        "INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,$3) RETURNING joined_at",
    ).bind(workspace_id).bind(row.0).bind(&row.1).fetch_one(&mut *tx).await.map_err(unique_pending)?;
    sqlx::query("INSERT INTO encrypted_workspace_keys(workspace_id,user_id,encrypted_key,nonce,aad_version,version) VALUES($1,$2,$3,$4,$5,1)")
        .bind(workspace_id).bind(row.0).bind(encrypted).bind(nonce).bind(req.aad_version).execute(&mut *tx).await?;
    sqlx::query("UPDATE invitation_key_requests SET approved_at=NOW(),approved_by=$1 WHERE id=$2")
        .bind(user.user_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(WorkspaceMemberResponse {
        user_id: row.0,
        email: row.2,
        role: row.1,
        joined_at,
    }))
}

async fn list_members(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<PaginatedResponse<WorkspaceMemberResponse>>> {
    db::verify_workspace_access(&state.db, workspace_id, user.user_id).await?;
    let rows=sqlx::query_as::<_,(Uuid,String,String,chrono::DateTime<chrono::Utc>)>(
        "SELECT wm.user_id,COALESCE(u.email,''),wm.role,wm.joined_at FROM workspace_members wm JOIN users u ON u.id=wm.user_id WHERE wm.workspace_id=$1 ORDER BY wm.joined_at",
    ).bind(workspace_id).fetch_all(&state.db).await?;
    let items = rows
        .into_iter()
        .map(|r| WorkspaceMemberResponse {
            user_id: r.0,
            email: r.1,
            role: r.2,
            joined_at: r.3,
        })
        .collect::<Vec<_>>();
    Ok(Json(PaginatedResponse {
        total: items.len() as i64,
        items,
    }))
}

async fn update_member(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path((workspace_id, member_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateMemberRoleRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    let actor = require_admin(&state, workspace_id, user.user_id.into()).await?;
    validate_role_change(&actor, &req.role)?;
    let mut tx = state.db.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let old: Option<String> = sqlx::query_scalar(
        "SELECT role FROM workspace_members WHERE workspace_id=$1 AND user_id=$2",
    )
    .bind(workspace_id)
    .bind(member_id)
    .fetch_optional(&mut *tx)
    .await?;
    let old = old.ok_or_else(|| AppError::NotFound("Member not found".into()))?;
    if actor != "owner" && old == "owner" {
        return Err(AppError::Forbidden(
            "Only an owner can modify an owner".into(),
        ));
    }
    ensure_not_primary_owner(&mut tx, workspace_id, member_id, Some(&req.role)).await?;
    ensure_not_last_owner(&mut tx, workspace_id, &old, Some(&req.role)).await?;
    sqlx::query("UPDATE workspace_members SET role=$1 WHERE workspace_id=$2 AND user_id=$3")
        .bind(&req.role)
        .bind(workspace_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(GenericMessageResponse {
        message: "Member role updated.".into(),
    }))
}

async fn remove_member(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path((workspace_id, member_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let actor = require_admin(&state, workspace_id, user.user_id.into()).await?;
    let mut tx = state.db.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM workspace_members WHERE workspace_id=$1 AND user_id=$2",
    )
    .bind(workspace_id)
    .bind(member_id)
    .fetch_optional(&mut *tx)
    .await?;
    let role = role.ok_or_else(|| AppError::NotFound("Member not found".into()))?;
    if actor != "owner" && role == "owner" {
        return Err(AppError::Forbidden(
            "Only an owner can remove an owner".into(),
        ));
    }
    ensure_not_primary_owner(&mut tx, workspace_id, member_id, None).await?;
    ensure_not_last_owner(&mut tx, workspace_id, &role, None).await?;
    sqlx::query("DELETE FROM encrypted_workspace_keys WHERE workspace_id=$1 AND user_id=$2")
        .bind(workspace_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM workspace_members WHERE workspace_id=$1 AND user_id=$2")
        .bind(workspace_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM workspace_members WHERE workspace_id=$1 AND user_id=$2",
    )
    .bind(workspace_id)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let role = role.ok_or_else(|| AppError::NotFound("Membership not found".into()))?;
    ensure_not_primary_owner(&mut tx, workspace_id, user.user_id.into(), None).await?;
    ensure_not_last_owner(&mut tx, workspace_id, &role, None).await?;
    sqlx::query("DELETE FROM encrypted_workspace_keys WHERE workspace_id=$1 AND user_id=$2")
        .bind(workspace_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM workspace_members WHERE workspace_id=$1 AND user_id=$2")
        .bind(workspace_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn transfer(
    State(state): State<AppState>,
    Auth(user): Auth,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<TransferOwnershipRequest>,
) -> AppResult<Json<GenericMessageResponse>> {
    if require_admin(&state, workspace_id, user.user_id.into()).await? != "owner" {
        return Err(AppError::Forbidden(
            "Only an owner can transfer ownership".into(),
        ));
    }
    if req.new_owner_user_id == Uuid::from(user.user_id) {
        return Err(AppError::BadRequest(
            "New owner must be another member".into(),
        ));
    }
    let mut tx = state.db.begin().await?;
    lock_members(&mut tx, workspace_id).await?;
    let changed = sqlx::query(
        "UPDATE workspace_members SET role='owner' WHERE workspace_id=$1 AND user_id=$2",
    )
    .bind(workspace_id)
    .bind(req.new_owner_user_id)
    .execute(&mut *tx)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(AppError::NotFound(
            "New owner must be an active member".into(),
        ));
    }
    sqlx::query("UPDATE workspace_members SET role='admin' WHERE workspace_id=$1 AND user_id=$2")
        .bind(workspace_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE workspaces SET owner_id=$1,updated_at=NOW() WHERE id=$2")
        .bind(req.new_owner_user_id)
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(GenericMessageResponse {
        message: "Ownership transferred.".into(),
    }))
}

fn validate_role_change(actor: &str, role: &str) -> AppResult<()> {
    if !ROLES.contains(&role) {
        return Err(AppError::BadRequest("Invalid workspace role".into()));
    }
    if actor != "owner" && role == "owner" {
        return Err(AppError::Forbidden(
            "Only an owner can assign owner role".into(),
        ));
    }
    Ok(())
}
async fn lock_members(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
) -> AppResult<()> {
    sqlx::query("SELECT user_id FROM workspace_members WHERE workspace_id=$1 FOR UPDATE")
        .bind(workspace_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(())
}
async fn ensure_not_last_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    old: &str,
    new: Option<&str>,
) -> AppResult<()> {
    if old == "owner" && new != Some("owner") {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workspace_members WHERE workspace_id=$1 AND role='owner'",
        )
        .bind(workspace_id)
        .fetch_one(&mut **tx)
        .await?;
        if count <= 1 {
            return Err(AppError::Conflict(
                "Workspace must retain at least one owner".into(),
            ));
        }
    }
    Ok(())
}

async fn ensure_not_primary_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    user_id: Uuid,
    new_role: Option<&str>,
) -> AppResult<()> {
    if new_role == Some("owner") {
        return Ok(());
    }
    let primary_owner: Uuid =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE id=$1 FOR UPDATE")
            .bind(workspace_id)
            .fetch_one(&mut **tx)
            .await?;
    if primary_owner == user_id {
        return Err(AppError::Conflict(
            "Use ownership transfer before demoting, removing, or leaving as primary owner".into(),
        ));
    }
    Ok(())
}
fn decode(value: &str, min: usize, max: usize, name: &str) -> AppResult<Vec<u8>> {
    if value.len() > max * 4 / 3 + 4 {
        return Err(AppError::BadRequest(format!("{name} exceeds size limit")));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| AppError::BadRequest(format!("Invalid {name} base64")))?;
    if !(min..=max).contains(&bytes.len()) {
        return Err(AppError::BadRequest(format!("Invalid {name} size")));
    }
    Ok(bytes)
}
fn encode(value: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(value)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/invitations", get(list_incoming))
        .route("/api/invitations/key-request", post(submit_key_request))
        .route(
            "/api/workspaces/{workspace_id}/invitations",
            post(create_invitation).get(list_outgoing),
        )
        .route(
            "/api/workspaces/{workspace_id}/invitations/{id}/resend",
            post(resend),
        )
        .route(
            "/api/workspaces/{workspace_id}/invitations/{id}",
            delete(revoke),
        )
        .route(
            "/api/workspaces/{workspace_id}/key-requests",
            get(list_key_requests),
        )
        .route(
            "/api/workspaces/{workspace_id}/key-requests/{id}/approve",
            post(approve),
        )
        .route("/api/workspaces/{workspace_id}/members", get(list_members))
        .route(
            "/api/workspaces/{workspace_id}/members/{member_id}",
            patch(update_member).delete(remove_member),
        )
        .route("/api/workspaces/{workspace_id}/leave", post(leave))
        .route(
            "/api/workspaces/{workspace_id}/transfer-ownership",
            post(transfer),
        )
}
