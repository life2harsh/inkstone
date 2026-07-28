pub mod protocol;
pub mod rooms;

use inkstone_core::protocol::ServerWsMessage;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OutboundMessage {
    Core(ServerWsMessage),
    EncryptedPresence(EncryptedPresenceFrame),
}

#[derive(Debug, Clone, Serialize)]
pub struct EncryptedPresenceFrame {
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub doc_id: Uuid,
    pub sender_device_id: Uuid,
    pub encrypted_presence_b64: String,
    pub nonce_b64: String,
    pub aad_version: i32,
    pub counter: u64,
}

impl From<ServerWsMessage> for OutboundMessage {
    fn from(value: ServerWsMessage) -> Self {
        Self::Core(value)
    }
}
