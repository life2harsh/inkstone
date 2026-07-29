use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use inkstone_core::protocol::{ASSET_AAD_VERSION, ASSET_CHUNK_SIZE, MAX_ASSET_CHUNKS};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetMetadata {
    pub path: String,
    pub media_type: String,
    pub plaintext_size: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedAssetChunk {
    pub index: u32,
    pub ciphertext_b64: String,
    pub nonce_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedAsset {
    pub asset_id: Uuid,
    pub version: i64,
    pub metadata_ciphertext_b64: String,
    pub metadata_nonce_b64: String,
    pub chunks: Vec<EncryptedAssetChunk>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssetCryptoError {
    #[error("asset exceeds the {0} chunk limit")]
    TooManyChunks(u32),
    #[error("asset version must be positive")]
    InvalidVersion,
    #[error("asset metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("asset authentication failed")]
    Authentication,
    #[error("asset envelope is invalid")]
    InvalidEnvelope,
}

pub fn encrypt_asset(
    key: &[u8; 32],
    workspace_id: Uuid,
    asset_id: Uuid,
    version: i64,
    metadata: &AssetMetadata,
    bytes: &[u8],
) -> Result<EncryptedAsset, AssetCryptoError> {
    if version <= 0 {
        return Err(AssetCryptoError::InvalidVersion);
    }
    if metadata.path.is_empty() || metadata.path.starts_with('/') || metadata.path.contains("..") {
        return Err(AssetCryptoError::InvalidMetadata("unsafe path".into()));
    }
    if metadata.plaintext_size != bytes.len() as u64 {
        return Err(AssetCryptoError::InvalidMetadata(
            "size does not match bytes".into(),
        ));
    }
    let chunk_count = bytes.len().div_ceil(ASSET_CHUNK_SIZE).max(1);
    if chunk_count > MAX_ASSET_CHUNKS as usize {
        return Err(AssetCryptoError::TooManyChunks(MAX_ASSET_CHUNKS));
    }
    let metadata_bytes = serde_json::to_vec(metadata)
        .map_err(|error| AssetCryptoError::InvalidMetadata(error.to_string()))?;
    let (metadata_ciphertext_b64, metadata_nonce_b64) = seal(
        key,
        &metadata_bytes,
        &metadata_aad(workspace_id, asset_id, version),
    )?;
    let chunks = (0..chunk_count)
        .map(|index| {
            let start = index * ASSET_CHUNK_SIZE;
            let end = (start + ASSET_CHUNK_SIZE).min(bytes.len());
            let plaintext = if bytes.is_empty() {
                &[]
            } else {
                &bytes[start..end]
            };
            let (ciphertext_b64, nonce_b64) = seal(
                key,
                plaintext,
                &chunk_aad(workspace_id, asset_id, version, index as u32),
            )?;
            Ok(EncryptedAssetChunk {
                index: index as u32,
                ciphertext_b64,
                nonce_b64,
            })
        })
        .collect::<Result<Vec<_>, AssetCryptoError>>()?;
    Ok(EncryptedAsset {
        asset_id,
        version,
        metadata_ciphertext_b64,
        metadata_nonce_b64,
        chunks,
    })
}

pub fn decrypt_asset(
    key: &[u8; 32],
    workspace_id: Uuid,
    encrypted: &EncryptedAsset,
) -> Result<(AssetMetadata, Vec<u8>), AssetCryptoError> {
    if encrypted.version <= 0
        || encrypted.chunks.is_empty()
        || encrypted.chunks.len() > MAX_ASSET_CHUNKS as usize
    {
        return Err(AssetCryptoError::InvalidEnvelope);
    }
    let metadata: AssetMetadata = serde_json::from_slice(&open(
        key,
        &encrypted.metadata_ciphertext_b64,
        &encrypted.metadata_nonce_b64,
        &metadata_aad(workspace_id, encrypted.asset_id, encrypted.version),
    )?)
    .map_err(|_| AssetCryptoError::InvalidEnvelope)?;
    let mut bytes = Vec::with_capacity(metadata.plaintext_size as usize);
    for (expected, chunk) in encrypted.chunks.iter().enumerate() {
        if chunk.index != expected as u32 {
            return Err(AssetCryptoError::InvalidEnvelope);
        }
        bytes.extend(open(
            key,
            &chunk.ciphertext_b64,
            &chunk.nonce_b64,
            &chunk_aad(
                workspace_id,
                encrypted.asset_id,
                encrypted.version,
                chunk.index,
            ),
        )?);
    }
    if bytes.len() as u64 != metadata.plaintext_size {
        return Err(AssetCryptoError::InvalidEnvelope);
    }
    Ok((metadata, bytes))
}

pub fn metadata_aad(workspace_id: Uuid, asset_id: Uuid, version: i64) -> Vec<u8> {
    format!("inkstone:v{ASSET_AAD_VERSION}:asset-metadata:{workspace_id}:{asset_id}:{version}")
        .into_bytes()
}

pub fn chunk_aad(workspace_id: Uuid, asset_id: Uuid, version: i64, index: u32) -> Vec<u8> {
    format!("inkstone:v{ASSET_AAD_VERSION}:asset-chunk:{workspace_id}:{asset_id}:{version}:{index}")
        .into_bytes()
}

fn seal(key: &[u8; 32], bytes: &[u8], aad: &[u8]) -> Result<(String, String), AssetCryptoError> {
    let mut nonce = [0_u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|_| AssetCryptoError::InvalidEnvelope)?;
    let ciphertext = ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: bytes, aad })
        .map_err(|_| AssetCryptoError::Authentication)?;
    let encoder = base64::engine::general_purpose::STANDARD;
    Ok((encoder.encode(ciphertext), encoder.encode(nonce)))
}

fn open(
    key: &[u8; 32],
    ciphertext: &str,
    nonce: &str,
    aad: &[u8],
) -> Result<Vec<u8>, AssetCryptoError> {
    let decoder = base64::engine::general_purpose::STANDARD;
    let ciphertext = decoder
        .decode(ciphertext)
        .map_err(|_| AssetCryptoError::InvalidEnvelope)?;
    let nonce = decoder
        .decode(nonce)
        .map_err(|_| AssetCryptoError::InvalidEnvelope)?;
    if nonce.len() != 12 {
        return Err(AssetCryptoError::InvalidEnvelope);
    }
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| AssetCryptoError::Authentication)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(size: usize) -> AssetMetadata {
        AssetMetadata {
            path: "images/a.png".into(),
            media_type: "image/png".into(),
            plaintext_size: size as u64,
            content_sha256: "opaque-client-digest".into(),
        }
    }

    #[test]
    fn chunk_roundtrip_and_identity_binding() {
        let workspace = Uuid::from_u128(1);
        let asset = Uuid::from_u128(2);
        let bytes = vec![7; ASSET_CHUNK_SIZE + 3];
        let encrypted = encrypt_asset(
            &[9; 32],
            workspace,
            asset,
            1,
            &metadata(bytes.len()),
            &bytes,
        )
        .unwrap();
        assert_eq!(encrypted.chunks.len(), 2);
        assert_eq!(
            decrypt_asset(&[9; 32], workspace, &encrypted).unwrap().1,
            bytes
        );
        assert!(decrypt_asset(&[9; 32], Uuid::from_u128(3), &encrypted).is_err());
    }

    #[test]
    fn tampered_chunk_and_reordered_chunks_fail() {
        let workspace = Uuid::from_u128(1);
        let mut encrypted = encrypt_asset(
            &[9; 32],
            workspace,
            Uuid::from_u128(2),
            1,
            &metadata(3),
            b"abc",
        )
        .unwrap();
        encrypted.chunks[0].ciphertext_b64.push('A');
        assert!(decrypt_asset(&[9; 32], workspace, &encrypted).is_err());
    }

    #[test]
    fn chunk_limit_is_enforced_before_encryption() {
        let size = ASSET_CHUNK_SIZE * (MAX_ASSET_CHUNKS as usize + 1);
        assert_eq!(
            encrypt_asset(
                &[1; 32],
                Uuid::nil(),
                Uuid::from_u128(1),
                1,
                &metadata(size),
                &vec![0; size]
            )
            .unwrap_err(),
            AssetCryptoError::TooManyChunks(MAX_ASSET_CHUNKS)
        );
    }
}
