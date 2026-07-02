use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: Uuid,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Identity {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: String,
    pub provider_subject: String,
    pub provider_email: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub device_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Doc {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub encrypted_title: Vec<u8>,
    pub title_nonce: Vec<u8>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct DocUpdate {
    pub id: i64,
    pub doc_id: Uuid,
    pub seq: i64,
    pub sender_device_id: Option<Uuid>,
    pub encrypted_update: Vec<u8>,
    pub nonce: Vec<u8>,
    pub aad_version: i32,
    pub client_update_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct DocSnapshot {
    pub id: i64,
    pub doc_id: Uuid,
    pub snapshot_version: i64,
    pub encrypted_snapshot: Vec<u8>,
    pub nonce: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Asset {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub doc_id: Option<Uuid>,
    pub filename: String,
    pub content_type: String,
    pub file_size: i64,
    pub storage_path: String,
    pub encrypted_metadata: Option<Vec<u8>>,
    pub metadata_nonce: Option<Vec<u8>>,
    pub uploaded_by: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Comment {
    pub id: Uuid,
    pub doc_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_id: Uuid,
    pub encrypted_content: Vec<u8>,
    pub content_nonce: Vec<u8>,
    pub anchor_data: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct EncryptedWorkspaceKey {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub encrypted_key: Vec<u8>,
    pub nonce: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Invitation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub inviter_id: Uuid,
    pub invitee_email: String,
    pub role: String,
    pub accepted: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AuditEvent {
    pub id: i64,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}
