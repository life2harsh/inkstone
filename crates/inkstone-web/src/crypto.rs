use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use x25519_dalek::{PublicKey, StaticSecret};

pub const AAD_VERSION: i32 = 1;
pub const WRAPPING_VERSION: i32 = 1;
const NONCE_LEN: usize = 12;
const PUBLIC_ENVELOPE_MAGIC: &[u8; 4] = b"IKX1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ciphertext {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
}

pub fn workspace_key_aad(workspace_id: Uuid, user_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:workspace-key:{workspace_id}:{user_id}").into_bytes()
}

pub fn workspace_metadata_aad(workspace_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:workspace-metadata:{workspace_id}").into_bytes()
}

pub fn account_vault_aad(user_id: Uuid, kind: &str) -> Vec<u8> {
    format!("inkstone:v1:account-vault:{user_id}:{kind}").into_bytes()
}

pub fn device_private_key_aad(user_id: Uuid, device_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:device-private-key:{user_id}:{device_id}").into_bytes()
}

pub fn presence_aad(workspace_id: Uuid, doc_id: Uuid, device_id: Uuid, counter: u64) -> Vec<u8> {
    format!("inkstone:v1:presence:{workspace_id}:{doc_id}:{device_id}:{counter}").into_bytes()
}

pub fn path_aad(workspace_id: Uuid, doc_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:path:{workspace_id}:{doc_id}").into_bytes()
}

pub fn folder_path_aad(workspace_id: Uuid, folder_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:folder-path:{workspace_id}:{folder_id}").into_bytes()
}

pub fn title_aad(workspace_id: Uuid, doc_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:title:{workspace_id}:{doc_id}").into_bytes()
}

pub fn update_aad(workspace_id: Uuid, doc_id: Uuid, update_id: Uuid) -> Vec<u8> {
    format!("inkstone:v1:update:{workspace_id}:{doc_id}:{update_id}").into_bytes()
}

pub fn snapshot_aad(workspace_id: Uuid, doc_id: Uuid, base_seq: i64) -> Vec<u8> {
    format!("inkstone:v1:snapshot:{workspace_id}:{doc_id}:{base_seq}").into_bytes()
}

pub fn derive_account_kek(passphrase: &str, user_id: Uuid) -> Result<[u8; 32], String> {
    if passphrase.is_empty() || passphrase.len() > 1024 {
        return Err("Vault passphrase must be between 1 and 1024 bytes".into());
    }
    let mut salt_hash = Sha256::new();
    salt_hash.update(b"inkstone:v1:account-kek-salt:");
    salt_hash.update(user_id.as_bytes());
    let salt = salt_hash.finalize();
    let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(|e| e.to_string())?;
    let mut key = [0_u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), &salt[..16], &mut key)
        .map_err(|e| e.to_string())?;
    Ok(key)
}

pub fn random_key() -> Result<[u8; 32], String> {
    let mut key = [0_u8; 32];
    getrandom::getrandom(&mut key).map_err(|error| error.to_string())?;
    Ok(key)
}

pub fn recovery_key_to_string(key: &[u8; 32]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
}

pub fn recovery_key_from_string(value: &str) -> Result<[u8; 32], String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value.trim())
        .map_err(|_| "Invalid recovery key".to_string())?;
    bytes
        .try_into()
        .map_err(|_| "Invalid recovery key length".to_string())
}

pub fn generate_device_keypair() -> Result<([u8; 32], [u8; 32]), String> {
    let private = random_key()?;
    let public = PublicKey::from(&StaticSecret::from(private)).to_bytes();
    Ok((private, public))
}

pub fn seal_workspace_key_for_device(
    workspace_key: &[u8; 32],
    recipient_public_key: &[u8; 32],
    workspace_id: Uuid,
    recipient_user_id: Uuid,
) -> Result<Ciphertext, String> {
    let ephemeral_private = StaticSecret::from(random_key()?);
    let ephemeral_public = PublicKey::from(&ephemeral_private).to_bytes();
    let shared = ephemeral_private.diffie_hellman(&PublicKey::from(*recipient_public_key));
    let mut key = [0_u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(b"inkstone:v1:x25519-workspace-key"), shared.as_bytes())
        .expand(
            &workspace_key_aad(workspace_id, recipient_user_id),
            &mut key,
        )
        .map_err(|_| "Failed to derive invitation key".to_string())?;
    let sealed = encrypt(
        &key,
        workspace_key,
        &workspace_key_aad(workspace_id, recipient_user_id),
    )?;
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(&sealed.ciphertext_b64)
        .map_err(|_| "Invalid generated envelope".to_string())?;
    let mut framed = Vec::with_capacity(4 + 32 + ciphertext.len());
    framed.extend_from_slice(PUBLIC_ENVELOPE_MAGIC);
    framed.extend_from_slice(&ephemeral_public);
    framed.extend_from_slice(&ciphertext);
    Ok(Ciphertext {
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(framed),
        nonce_b64: sealed.nonce_b64,
    })
}

pub fn open_workspace_key_for_device(
    envelope: &Ciphertext,
    private_key: &[u8; 32],
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<[u8; 32], String> {
    let framed = base64::engine::general_purpose::STANDARD
        .decode(&envelope.ciphertext_b64)
        .map_err(|_| "Invalid invitation envelope".to_string())?;
    if framed.len() < 4 + 32 + 16 || &framed[..4] != PUBLIC_ENVELOPE_MAGIC {
        return Err("Unsupported invitation envelope".into());
    }
    let ephemeral_public: [u8; 32] = framed[4..36]
        .try_into()
        .map_err(|_| "Invalid invitation public key".to_string())?;
    let shared =
        StaticSecret::from(*private_key).diffie_hellman(&PublicKey::from(ephemeral_public));
    let mut key = [0_u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(b"inkstone:v1:x25519-workspace-key"), shared.as_bytes())
        .expand(&workspace_key_aad(workspace_id, user_id), &mut key)
        .map_err(|_| "Failed to derive invitation key".to_string())?;
    let plaintext = decrypt(
        &key,
        &base64::engine::general_purpose::STANDARD.encode(&framed[36..]),
        &envelope.nonce_b64,
        &workspace_key_aad(workspace_id, user_id),
    )?;
    plaintext
        .try_into()
        .map_err(|_| "Invitation envelope has an invalid key length".to_string())
}

pub fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Ciphertext, String> {
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| "encryption failed".to_string())?;
    let encoder = base64::engine::general_purpose::STANDARD;
    Ok(Ciphertext {
        ciphertext_b64: encoder.encode(ciphertext),
        nonce_b64: encoder.encode(nonce),
    })
}

pub fn decrypt(
    key: &[u8; 32],
    ciphertext_b64: &str,
    nonce_b64: &str,
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    let decoder = base64::engine::general_purpose::STANDARD;
    let ciphertext = decoder
        .decode(ciphertext_b64)
        .map_err(|_| "invalid ciphertext base64".to_string())?;
    let nonce = decoder
        .decode(nonce_b64)
        .map_err(|_| "invalid nonce base64".to_string())?;
    if nonce.len() != NONCE_LEN {
        return Err("nonce must be 12 bytes".into());
    }
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| "authentication failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::{
        CollaborativeTextUpdate, DOCUMENT_UPDATE_V2_VERSION, DocId, DocumentOp, DocumentUpdate,
        TextTarget, UpdateId,
    };

    #[test]
    fn aad_is_deterministic_and_identity_bound() {
        let workspace = Uuid::from_u128(1);
        let doc = Uuid::from_u128(2);
        let update = Uuid::from_u128(3);
        assert_eq!(
            update_aad(workspace, doc, update),
            b"inkstone:v1:update:00000000-0000-0000-0000-000000000001:00000000-0000-0000-0000-000000000002:00000000-0000-0000-0000-000000000003"
        );
        assert_ne!(
            update_aad(workspace, doc, update),
            update_aad(workspace, Uuid::from_u128(4), update)
        );
        assert_ne!(
            snapshot_aad(workspace, doc, 8),
            snapshot_aad(workspace, doc, 9)
        );
        assert_ne!(path_aad(workspace, doc), title_aad(workspace, doc));
    }

    #[test]
    fn ciphertext_rejects_wrong_aad() {
        let key = [7_u8; 32];
        let encrypted = encrypt(&key, b"private", b"identity-a").unwrap();
        assert_eq!(
            decrypt(
                &key,
                &encrypted.ciphertext_b64,
                &encrypted.nonce_b64,
                b"identity-a"
            )
            .unwrap(),
            b"private"
        );
        assert!(
            decrypt(
                &key,
                &encrypted.ciphertext_b64,
                &encrypted.nonce_b64,
                b"identity-b"
            )
            .is_err()
        );
    }

    #[test]
    fn ciphertext_rejects_wrong_key_and_tampering() {
        let encrypted = encrypt(&[1; 32], b"vault", b"aad").unwrap();
        assert!(
            decrypt(
                &[2; 32],
                &encrypted.ciphertext_b64,
                &encrypted.nonce_b64,
                b"aad"
            )
            .is_err()
        );
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&encrypted.ciphertext_b64)
            .unwrap();
        bytes[0] ^= 1;
        assert!(
            decrypt(
                &[1; 32],
                &base64::engine::general_purpose::STANDARD.encode(bytes),
                &encrypted.nonce_b64,
                b"aad"
            )
            .is_err()
        );
    }

    #[test]
    fn recovery_key_and_account_vault_roundtrip() {
        let user = Uuid::from_u128(8);
        let recovery = random_key().unwrap();
        let displayed = recovery_key_to_string(&recovery);
        assert_eq!(recovery_key_from_string(&displayed).unwrap(), recovery);
        let vault = random_key().unwrap();
        let envelope =
            encrypt(&recovery, &vault, &account_vault_aad(user, "recovery_key")).unwrap();
        assert_eq!(
            decrypt(
                &recovery,
                &envelope.ciphertext_b64,
                &envelope.nonce_b64,
                &account_vault_aad(user, "recovery_key")
            )
            .unwrap(),
            vault
        );
    }

    #[test]
    fn x25519_workspace_envelope_rejects_wrong_device_and_tampering() {
        let workspace = Uuid::from_u128(4);
        let user = Uuid::from_u128(5);
        let (private, public) = generate_device_keypair().unwrap();
        let (wrong_private, _) = generate_device_keypair().unwrap();
        let envelope = seal_workspace_key_for_device(&[7; 32], &public, workspace, user).unwrap();
        assert_eq!(
            open_workspace_key_for_device(&envelope, &private, workspace, user).unwrap(),
            [7; 32]
        );
        assert!(open_workspace_key_for_device(&envelope, &wrong_private, workspace, user).is_err());
        let mut tampered = envelope.clone();
        tampered.ciphertext_b64.push('A');
        assert!(open_workspace_key_for_device(&tampered, &private, workspace, user).is_err());
    }

    #[test]
    fn document_update_v2_roundtrips_only_inside_ciphertext() {
        let workspace = Uuid::from_u128(1);
        let update = DocumentUpdate {
            version: DOCUMENT_UPDATE_V2_VERSION,
            update_id: UpdateId::new_v4(),
            doc_id: DocId::new_v4(),
            applied_at_ms: 7,
            op: DocumentOp::ApplyTextUpdateV2 {
                payload: CollaborativeTextUpdate {
                    version: 2,
                    target: TextTarget::Title,
                    update_v1: vec![1, 2, 3],
                },
            },
        };
        let plaintext = serde_json::to_vec(&update).unwrap();
        let aad = update_aad(workspace, update.doc_id.0, update.update_id.0);
        let encrypted = encrypt(&[9; 32], &plaintext, &aad).unwrap();
        let decoded: DocumentUpdate = serde_json::from_slice(
            &decrypt(
                &[9; 32],
                &encrypted.ciphertext_b64,
                &encrypted.nonce_b64,
                &aad,
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(decoded.version, DOCUMENT_UPDATE_V2_VERSION);
        assert!(matches!(decoded.op, DocumentOp::ApplyTextUpdateV2 { .. }));
        assert!(!encrypted.ciphertext_b64.contains("apply_text_update_v2"));
    }

    #[test]
    fn encrypted_ink_update_exposes_no_stonepen_fields() {
        let workspace = Uuid::from_u128(1);
        let update = DocumentUpdate {
            version: DOCUMENT_UPDATE_V2_VERSION,
            update_id: UpdateId::new_v4(),
            doc_id: DocId::new_v4(),
            applied_at_ms: 9,
            op: DocumentOp::ApplyInkUpdateV2 {
                payload: inkstone_core::CollaborativeInkUpdate {
                    version: inkstone_core::INK_UPDATE_PAYLOAD_VERSION,
                    block_id: inkstone_core::BlockId::new_v4(),
                    stream_epoch: Uuid::new_v4(),
                    update: b"raw_pts stroke brush layers".to_vec(),
                    bootstrap_snapshot: None,
                },
            },
        };
        let plaintext = serde_json::to_vec(&update).unwrap();
        let aad = update_aad(workspace, update.doc_id.0, update.update_id.0);
        let encrypted = encrypt(&[11; 32], &plaintext, &aad).unwrap();

        for recognizable in ["raw_pts", "stroke", "brush", "layers"] {
            assert!(!encrypted.ciphertext_b64.contains(recognizable));
        }
        let decoded: DocumentUpdate = serde_json::from_slice(
            &decrypt(
                &[11; 32],
                &encrypted.ciphertext_b64,
                &encrypted.nonce_b64,
                &aad,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(decoded.op, DocumentOp::ApplyInkUpdateV2 { .. }));
    }
}
