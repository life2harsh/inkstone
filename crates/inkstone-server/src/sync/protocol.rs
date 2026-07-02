use inkstone_core::protocol::ClientWsMessage;
use serde_json;

pub fn parse_client_message(json: &str) -> Result<ClientWsMessage, String> {
    serde_json::from_str::<ClientWsMessage>(json)
        .map_err(|e| format!("Invalid message: {}", e))
}
