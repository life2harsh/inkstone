use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::UpdateId;

pub const INK_PRESENCE_VERSION: u32 = 2;
pub const ASSET_AAD_VERSION: i32 = 1;
pub const ASSET_CHUNK_SIZE: usize = 1024 * 1024;
pub const MAX_ASSET_CHUNKS: u32 = 256;
pub const MAX_ASSET_ENCRYPTED_BYTES: u64 = 256 * 1024 * 1024 + 4096;

/// Platform-neutral routing envelope for ephemeral Ink presence.
/// `stonepen_preview` is produced and consumed only by Stonepen and is opaque
/// to Inkstone. This envelope is always carried inside encrypted presence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InkPresencePayload {
    pub version: u32,
    pub block_id: Uuid,
    pub stream_epoch: Uuid,
    #[serde(with = "base64_bytes")]
    pub stonepen_preview: Vec<u8>,
}

/// Bytes as one base64 string rather than a JSON array of numbers, which
/// would be several times larger on every live stroke update.
mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientWsMessage {
    #[serde(rename = "encrypted_update")]
    EncryptedUpdate {
        doc_id: Uuid,
        update_id: UpdateId,
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

    #[serde(rename = "encrypted_presence")]
    EncryptedPresence {
        doc_id: Uuid,
        encrypted_presence_b64: String,
        nonce_b64: String,
        aad_version: i32,
        counter: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerWsMessage {
    #[serde(rename = "encrypted_update")]
    EncryptedUpdate {
        doc_id: Uuid,
        sender_device_id: Uuid,
        update_id: UpdateId,
        seq: i64,
        encrypted_update_b64: String,
        nonce_b64: String,
        aad_version: i32,
    },

    /// Sent directly to a joining client with the current committed sequence.
    #[serde(rename = "ready")]
    Ready { doc_id: Uuid, current_seq: i64 },

    #[serde(rename = "pong")]
    Pong,

    #[serde(rename = "presence")]
    Presence {
        doc_id: Uuid,
        sender_device_id: Uuid,
        encrypted_presence_b64: String,
    },

    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub workspace_id: Uuid,
    pub encrypted_metadata_b64: String,
    pub metadata_nonce_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateWorkspaceRequest {
    pub encrypted_metadata_b64: String,
    pub metadata_nonce_b64: String,
    pub metadata_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAssetRequest {
    pub asset_id: Uuid,
    pub version: i64,
    pub chunk_count: u32,
    pub encrypted_size: u64,
    pub encrypted_metadata_b64: String,
    pub metadata_nonce_b64: String,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAssetMetadataRequest {
    pub encrypted_metadata_b64: String,
    pub metadata_nonce_b64: String,
    pub metadata_version: i64,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutAssetChunkRequest {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub version: i64,
    pub chunk_count: u32,
    pub encrypted_size: u64,
    pub encrypted_metadata_b64: String,
    pub metadata_nonce_b64: String,
    pub aad_version: i32,
    pub metadata_version: i64,
    pub complete: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutPathProjectionRequest {
    pub entry_kind: String,
    pub encrypted_path_b64: String,
    pub path_nonce_b64: String,
    pub projection_version: i64,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathProjectionResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub entry_kind: String,
    pub encrypted_path_b64: String,
    pub path_nonce_b64: String,
    pub projection_version: i64,
    pub aad_version: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeDocPathUpdate {
    pub doc_id: Uuid,
    #[serde(flatten)]
    pub projection: UpdateDocPathProjectionRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeAssetMetadataUpdate {
    pub asset_id: Uuid,
    #[serde(flatten)]
    pub metadata: UpdateAssetMetadataRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreePathProjectionUpdate {
    pub entry_id: Uuid,
    #[serde(flatten)]
    pub projection: PutPathProjectionRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTreeMutationRequest {
    pub doc_updates: Vec<TreeDocPathUpdate>,
    pub asset_updates: Vec<TreeAssetMetadataUpdate>,
    pub projection_updates: Vec<TreePathProjectionUpdate>,
    pub delete_doc_ids: Vec<Uuid>,
    pub delete_asset_ids: Vec<Uuid>,
    pub delete_projection_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetChunkResponse {
    pub asset_id: Uuid,
    pub version: i64,
    pub chunk_index: u32,
    pub ciphertext_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDocRequest {
    pub doc_id: Uuid,
    pub encrypted_title_b64: Option<String>,
    pub title_nonce_b64: Option<String>,
    pub encrypted_path_b64: String,
    pub path_nonce_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDocPathProjectionRequest {
    pub encrypted_path_b64: String,
    pub path_nonce_b64: String,
    pub projection_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDocTitleProjectionRequest {
    pub encrypted_title_b64: String,
    pub title_nonce_b64: String,
    pub base_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub device_id: Uuid,
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapRequest {
    #[serde(flatten)]
    pub registration: RegisterRequest,
    #[serde(default)]
    pub bootstrap_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerSetupRequest {
    pub token: String,
    #[serde(default)]
    pub email: Option<String>,
    pub password: String,
    pub device_id: Uuid,
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationRegisterRequest {
    #[serde(flatten)]
    pub registration: RegisterRequest,
    pub invitation_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    pub device_id: Uuid,
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthUserResponse {
    pub id: Uuid,
    pub email: String,
    pub device_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitationRequest {
    pub email: String,
    pub role: String,
    pub expires_in_hours: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitationResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingInvitationResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub workspace_name: String,
    pub inviter_id: Uuid,
    pub email: String,
    pub role: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptInvitationRequest {
    pub token: String,
    pub workspace_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitInvitationKeyRequest {
    pub token: String,
    pub workspace_id: Uuid,
    pub public_key_b64: String,
    pub key_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationKeyRequestResponse {
    pub id: Uuid,
    pub invitation_id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub public_key_b64: String,
    pub key_version: i32,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproveInvitationKeyRequest {
    pub encrypted_key_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMemberRoleRequest {
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferOwnershipRequest {
    pub new_owner_user_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptInvitationResponse {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub joined_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMemberResponse {
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_name: Option<EncryptedGuestName>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedGuestName {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumeEmailVerificationRequest {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumePasswordResetRequest {
    pub token: String,
    #[serde(default)]
    pub email: Option<String>,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
    pub passphrase_envelope: RecoveryEnvelopeRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericMessageResponse {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEnvelopeRequest {
    pub kind: String,
    pub expected_version: Option<i64>,
    pub ciphertext_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub wrapping_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEnvelopeResponse {
    pub kind: String,
    pub version: i64,
    pub ciphertext_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub wrapping_version: i32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceResponse {
    pub id: Uuid,
    pub name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
    pub current: bool,
    pub sessions: Vec<SessionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub id: Uuid,
    pub device_id: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutWorkspaceKeyEnvelopeRequest {
    pub encrypted_key_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceKeyEnvelopeResponse {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub encrypted_key_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    #[serde(default = "first_version")]
    pub version: i64,
}

fn first_version() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostUpdateRequest {
    pub encrypted_update_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub update_id: UpdateId,
    pub sender_device_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSnapshotRequest {
    pub encrypted_snapshot_b64: String,
    pub nonce_b64: String,
    pub base_seq: i64,
    pub aad_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResponse {
    pub seq: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedUpdateResponse {
    pub seq: i64,
    pub update_id: UpdateId,
    pub sender_device_id: Uuid,
    pub encrypted_update_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResponse {
    pub base_seq: i64,
    pub encrypted_snapshot_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub encrypted_title_b64: Option<String>,
    pub title_nonce_b64: Option<String>,
    pub title_projection_seq: i64,
    pub encrypted_path_b64: String,
    pub path_nonce_b64: String,
    pub path_projection_version: i64,
    pub created_by: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_seq: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceResponse {
    pub id: Uuid,
    pub encrypted_metadata_b64: String,
    pub metadata_nonce_b64: String,
    pub metadata_version: i64,
    pub owner_id: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub public_link: bool,
    #[serde(default)]
    pub public_link_editable: bool,
    #[serde(default)]
    pub guest_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicLinkRequest {
    pub token: String,
    #[serde(default)]
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicLinkAccessRequest {
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicLinkResponse {
    pub workspace: WorkspaceResponse,
    pub user_id: Uuid,
    pub device_id: Uuid,
    #[serde(default)]
    pub guest: Option<EncryptedGuestName>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestJoinRequest {
    pub name: EncryptedGuestName,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestJoinResponse {
    pub token: String,
    pub user_id: Uuid,
    pub device_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
}

#[cfg(test)]
mod tests {
    use super::{
        ApproveInvitationKeyRequest, BootstrapRequest, CreateDocRequest, INK_PRESENCE_VERSION,
        InkPresencePayload, InvitationRegisterRequest, OwnerSetupRequest,
        PutWorkspaceKeyEnvelopeRequest, RecoveryEnvelopeRequest, RegisterRequest,
        UpdateDocPathProjectionRequest, UpdateDocTitleProjectionRequest,
    };
    use uuid::Uuid;

    #[test]
    fn create_doc_request_round_trips_client_generated_id() {
        let request = CreateDocRequest {
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some("opaque-title".into()),
            title_nonce_b64: Some("title-nonce".into()),
            encrypted_path_b64: "opaque-path".into(),
            path_nonce_b64: "path-nonce".into(),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: CreateDocRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.doc_id, request.doc_id);
    }

    #[test]
    fn workspace_key_envelope_request_is_opaque() {
        let request = PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: "opaque-ciphertext".into(),
            nonce_b64: "opaque-nonce".into(),
            aad_version: 1,
            replace_version: None,
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: PutWorkspaceKeyEnvelopeRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.encrypted_key_b64, request.encrypted_key_b64);
        assert_eq!(decoded.nonce_b64, request.nonce_b64);
        assert_eq!(decoded.aad_version, 1);
    }

    #[test]
    fn key_and_recovery_protocols_expose_only_opaque_envelopes() {
        let recovery = serde_json::to_value(RecoveryEnvelopeRequest {
            kind: "recovery_key".into(),
            expected_version: Some(4),
            ciphertext_b64: "opaque-ciphertext".into(),
            nonce_b64: "opaque-nonce".into(),
            aad_version: 1,
            wrapping_version: 1,
        })
        .unwrap();
        let approval = serde_json::to_value(ApproveInvitationKeyRequest {
            encrypted_key_b64: "recipient-bound-ciphertext".into(),
            nonce_b64: "opaque-nonce".into(),
            aad_version: 1,
        })
        .unwrap();
        for value in [recovery, approval] {
            assert!(value.get("vault_key").is_none());
            assert!(value.get("workspace_key").is_none());
            assert!(value.get("recovery_key").is_none());
        }
    }

    #[test]
    fn document_projection_requests_are_opaque_and_distinct() {
        let path = UpdateDocPathProjectionRequest {
            encrypted_path_b64: "opaque-path".into(),
            path_nonce_b64: "path-nonce".into(),
            projection_version: 3,
        };
        let decoded: UpdateDocPathProjectionRequest =
            serde_json::from_str(&serde_json::to_string(&path).unwrap()).unwrap();
        assert_eq!(decoded.encrypted_path_b64, path.encrypted_path_b64);

        let title = UpdateDocTitleProjectionRequest {
            encrypted_title_b64: "opaque-title".into(),
            title_nonce_b64: "title-nonce".into(),
            base_seq: 7,
        };
        let decoded: UpdateDocTitleProjectionRequest =
            serde_json::from_str(&serde_json::to_string(&title).unwrap()).unwrap();
        assert_eq!(decoded.encrypted_title_b64, title.encrypted_title_b64);
        assert_eq!(decoded.base_seq, 7);
    }

    #[test]
    fn registration_credentials_are_flattened_into_endpoint_payloads() {
        let registration = RegisterRequest {
            email: "user@example.com".into(),
            password: "correct horse battery staple".into(),
            device_id: Uuid::new_v4(),
            device_name: None,
        };
        let bootstrap = serde_json::to_value(BootstrapRequest {
            registration: registration.clone(),
            bootstrap_token: Some("bootstrap-secret".into()),
        })
        .unwrap();
        assert_eq!(bootstrap["email"], registration.email);
        assert_eq!(bootstrap["bootstrap_token"], "bootstrap-secret");

        let invited = serde_json::to_value(InvitationRegisterRequest {
            registration,
            invitation_token: "invitation-secret".into(),
        })
        .unwrap();
        assert_eq!(invited["email"], "user@example.com");
        assert_eq!(invited["invitation_token"], "invitation-secret");
    }

    #[test]
    fn ink_presence_keeps_preview_opaque_and_epoch_scoped() {
        let payload = InkPresencePayload {
            version: INK_PRESENCE_VERSION,
            block_id: Uuid::new_v4(),
            stream_epoch: Uuid::new_v4(),
            stonepen_preview: vec![0, 255, 17],
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["stonepen_preview"], "AP8R");
        let roundtrip: InkPresencePayload = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, payload);
    }

    #[test]
    fn owner_setup_request_email_is_a_confirmation_and_nothing_else_is_accepted() {
        let mut request = serde_json::json!({
            "token": "secret",
            "email": "owner@example.com",
            "password": "correct horse battery staple",
            "device_id": Uuid::new_v4(),
            "device_name": null
        });
        let parsed = serde_json::from_value::<OwnerSetupRequest>(request.clone()).unwrap();
        assert_eq!(parsed.email.as_deref(), Some("owner@example.com"));
        request["role"] = serde_json::json!("admin");
        assert!(serde_json::from_value::<OwnerSetupRequest>(request).is_err());
    }
}
