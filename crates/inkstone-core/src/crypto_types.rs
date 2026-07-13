use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedBlob {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub aad_version: i32,
}

impl EncryptedBlob {
    pub fn new(ciphertext: Vec<u8>, nonce: Vec<u8>, aad_version: i32) -> Self {
        Self {
            ciphertext,
            nonce,
            aad_version,
        }
    }
}
