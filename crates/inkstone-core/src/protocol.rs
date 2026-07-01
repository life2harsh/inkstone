use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientWsMessage {
    #[serde(rename = "encrypted_update")]
    EncryptedUpdate {
        doc_id: Uuid,
        client_update_id: Uuid,
        encrypted_update_b64: String,
        nonce_b64: String,
        aad_version: i32,
    },

    #[serde(rename = "ping")]
    Ping,

    #[serde(rename = "presence")]
    Presence {
        doc_id: Uuid,
        encrypted_presence_b64: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerWsMessage {
    #[serde(rename = "encrypted_update")]
    EncryptedUpdate {
        doc_id: Uuid,
        sender_device_id: Uuid,
        seq: i64,
        encrypted_update_b64: String,
        nonce_b64: String,
        aad_version: i32,
    },

    #[serde(rename = "pong")]
    Pong,

    #[serde(rename = "presence")]
    Presence {
        doc_id: Uuid,
        sender_device_id: Uuid,
        encrypted_presence_b64: String,
    },

    #[serde(rename = "error")]
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDocRequest {
    pub encrypted_title_b64: String,
    pub title_nonce_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostUpdateRequest {
    pub encrypted_update_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub client_update_id: Uuid,
    pub sender_device_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSnapshotRequest {
    pub encrypted_snapshot_b64: String,
    pub nonce_b64: String,
    pub snapshot_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResponse {
    pub seq: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResponse {
    pub snapshot_version: i64,
    pub encrypted_snapshot_b64: String,
    pub nonce_b64: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub encrypted_title_b64: String,
    pub title_nonce_b64: String,
    pub created_by: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
}
