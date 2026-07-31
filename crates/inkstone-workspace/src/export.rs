use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

use inkstone_core::document::InkstoneDocument;
use serde::{Deserialize, Serialize};

use crate::{WorkspacePath, WorkspaceStoreError};

pub const EXPORT_FORMAT_VERSION: u32 = 2;
pub const MAX_IMPORT_ENTRIES: usize = 10_000;
pub const MAX_IMPORT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ExportDocument {
    pub path: WorkspacePath,
    pub document: InkstoneDocument,
    pub encrypted_crdt_backup: Option<Vec<u8>>,
    pub has_pending_updates: bool,
}

#[derive(Debug, Clone)]
pub struct ExportAsset {
    pub path: WorkspacePath,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct VaultExport {
    pub workspace_id: uuid::Uuid,
    pub documents: Vec<ExportDocument>,
    pub assets: Vec<ExportAsset>,
    pub empty_folders: Vec<WorkspacePath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportManifest {
    pub format: String,
    pub version: u32,
    pub workspace_id: uuid::Uuid,
    pub pending_document_paths: Vec<String>,
    pub encrypted_crdt_backups: Vec<String>,
    pub documents: Vec<ManifestDocument>,
    pub assets: Vec<ManifestAsset>,
    pub empty_folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestDocument {
    pub id: uuid::Uuid,
    pub path: String,
    pub backup_path: Option<String>,
    pub pending_updates: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestAsset {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedVault {
    pub manifest: ExportManifest,
    pub files: BTreeMap<WorkspacePath, Vec<u8>>,
    pub folders: Vec<WorkspacePath>,
    pub crdt_backups: BTreeMap<uuid::Uuid, Vec<u8>>,
}

pub fn build_vault_archive(export: &VaultExport) -> Result<Vec<u8>, WorkspaceStoreError> {
    let mut documents = export.documents.clone();
    documents.sort_by(|a, b| a.path.cmp(&b.path));
    let mut assets = export.assets.clone();
    assets.sort_by(|a, b| a.path.cmp(&b.path));
    let mut folders = export.empty_folders.clone();
    folders.sort();

    let manifest = ExportManifest {
        format: "inkstone-vault".into(),
        version: EXPORT_FORMAT_VERSION,
        workspace_id: export.workspace_id,
        pending_document_paths: documents
            .iter()
            .filter(|document| document.has_pending_updates)
            .map(|document| document.path.as_str().to_string())
            .collect(),
        encrypted_crdt_backups: documents
            .iter()
            .filter(|document| document.encrypted_crdt_backup.is_some())
            .map(|document| format!(".inkstone/crdt/{}.bin", document.document.id))
            .collect(),
        documents: documents
            .iter()
            .map(|document| ManifestDocument {
                id: document.document.id.0,
                path: document.path.as_str().into(),
                backup_path: document
                    .encrypted_crdt_backup
                    .as_ref()
                    .map(|_| format!(".inkstone/crdt/{}.bin", document.document.id)),
                pending_updates: document.has_pending_updates,
            })
            .collect(),
        assets: assets
            .iter()
            .map(|asset| ManifestAsset {
                path: asset.path.as_str().into(),
                size: asset.bytes.len() as u64,
                sha256: sha256_hex(&asset.bytes),
            })
            .collect(),
        empty_folders: folders
            .iter()
            .map(|folder| folder.as_str().into())
            .collect(),
    };
    let cursor = Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    archive
        .start_file("manifest.json", options)
        .map_err(zip_error)?;
    archive
        .write_all(&serde_json::to_vec_pretty(&manifest).map_err(serialization_error)?)
        .map_err(backend_error)?;
    for folder in folders {
        archive
            .add_directory(format!("{}/", folder.as_str()), options)
            .map_err(zip_error)?;
    }
    for document in documents {
        archive
            .start_file(document.path.as_str(), options)
            .map_err(zip_error)?;
        archive
            .write_all(
                inkstone_core::markdown::materialize_document_markdown(&document.document)
                    .as_bytes(),
            )
            .map_err(backend_error)?;
        if let Some(backup) = document.encrypted_crdt_backup {
            archive
                .start_file(
                    format!(".inkstone/crdt/{}.bin", document.document.id),
                    options,
                )
                .map_err(zip_error)?;
            archive.write_all(&backup).map_err(backend_error)?;
        }
    }
    for asset in assets {
        archive
            .start_file(asset.path.as_str(), options)
            .map_err(zip_error)?;
        archive.write_all(&asset.bytes).map_err(backend_error)?;
    }
    Ok(archive.finish().map_err(zip_error)?.into_inner())
}

pub fn read_vault_archive(bytes: &[u8]) -> Result<ImportedVault, WorkspaceStoreError> {
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(WorkspaceStoreError::Serialization(
            "archive exceeds import size limit".into(),
        ));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(zip_error)?;
    if archive.len() == 0 || archive.len() > MAX_IMPORT_ENTRIES {
        return Err(WorkspaceStoreError::Serialization(
            "archive entry count is invalid".into(),
        ));
    }
    let mut names = BTreeSet::new();
    let mut raw = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let name = entry.name().to_string();
        if !names.insert(name.clone()) {
            return Err(WorkspaceStoreError::Serialization(format!(
                "duplicate archive entry: {name}"
            )));
        }
        if name.starts_with('/')
            || name.contains('\\')
            || name
                .split('/')
                .any(|segment| segment == ".." || segment == ".")
        {
            return Err(WorkspaceStoreError::InvalidPath(name));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| WorkspaceStoreError::Serialization("archive size overflow".into()))?;
        if total > MAX_IMPORT_BYTES {
            return Err(WorkspaceStoreError::Serialization(
                "expanded archive exceeds import size limit".into(),
            ));
        }
        let mut value = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut value).map_err(backend_error)?;
        raw.insert(name, value);
    }
    let manifest_bytes = raw
        .remove("manifest.json")
        .ok_or_else(|| WorkspaceStoreError::Serialization("manifest.json is missing".into()))?;
    let manifest: ExportManifest =
        serde_json::from_slice(&manifest_bytes).map_err(serialization_error)?;
    if manifest.format != "inkstone-vault" || manifest.version != EXPORT_FORMAT_VERSION {
        return Err(WorkspaceStoreError::UnsupportedFormatVersion(
            manifest.version,
        ));
    }
    let mut files = BTreeMap::new();
    let mut folders = Vec::new();
    let mut backups = BTreeMap::new();
    let mut expected = BTreeSet::new();
    for document in &manifest.documents {
        let path = WorkspacePath::new(&document.path)?;
        if !path.is_markdown() || !expected.insert(document.path.clone()) {
            return Err(WorkspaceStoreError::Serialization(
                "invalid or duplicate document path".into(),
            ));
        }
        let value = raw.remove(&document.path).ok_or_else(|| {
            WorkspaceStoreError::Serialization(format!("missing document: {}", document.path))
        })?;
        files.insert(path, value);
        if let Some(backup_path) = &document.backup_path {
            if backup_path != &format!(".inkstone/crdt/{}.bin", document.id)
                || !expected.insert(backup_path.clone())
            {
                return Err(WorkspaceStoreError::Serialization(
                    "invalid CRDT backup identity".into(),
                ));
            }
            backups.insert(
                document.id,
                raw.remove(backup_path).ok_or_else(|| {
                    WorkspaceStoreError::Serialization(format!("missing backup: {backup_path}"))
                })?,
            );
        }
    }
    for asset in &manifest.assets {
        let path = WorkspacePath::new(&asset.path)?;
        if path.is_markdown() || path.is_internal() || !expected.insert(asset.path.clone()) {
            return Err(WorkspaceStoreError::Serialization(
                "invalid or duplicate asset path".into(),
            ));
        }
        let value = raw.remove(&asset.path).ok_or_else(|| {
            WorkspaceStoreError::Serialization(format!("missing asset: {}", asset.path))
        })?;
        if value.len() as u64 != asset.size || sha256_hex(&value) != asset.sha256 {
            return Err(WorkspaceStoreError::Serialization(format!(
                "asset integrity mismatch: {}",
                asset.path
            )));
        }
        files.insert(path, value);
    }
    for folder in &manifest.empty_folders {
        let path = WorkspacePath::new(folder)?;
        if path.is_internal() || !expected.insert(format!("{folder}/")) {
            return Err(WorkspaceStoreError::Serialization(
                "invalid or duplicate folder path".into(),
            ));
        }
        if raw.remove(&format!("{folder}/")).is_none() {
            return Err(WorkspaceStoreError::Serialization(format!(
                "missing folder entry: {folder}"
            )));
        }
        folders.push(path);
    }
    if !raw.is_empty() {
        return Err(WorkspaceStoreError::Serialization(format!(
            "undeclared archive entry: {}",
            raw.keys().next().unwrap()
        )));
    }
    folders.sort();
    Ok(ImportedVault {
        manifest,
        files,
        folders,
        crdt_backups: backups,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn serialization_error(error: serde_json::Error) -> WorkspaceStoreError {
    WorkspaceStoreError::Serialization(error.to_string())
}

fn zip_error(error: zip::result::ZipError) -> WorkspaceStoreError {
    WorkspaceStoreError::Backend(error.to_string())
}

fn backend_error(error: std::io::Error) -> WorkspaceStoreError {
    WorkspaceStoreError::Backend(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::{BlockContent, BlockId, DocId, DocumentBlock};
    use std::io::Read;

    fn fixture() -> VaultExport {
        let mut document = InkstoneDocument::new(DocId(uuid::Uuid::from_u128(2)));
        document.title = "Title".into();
        document.blocks.push(DocumentBlock::new(
            BlockId(uuid::Uuid::from_u128(3)),
            BlockContent::Markdown {
                text: "Body".into(),
            },
        ));
        VaultExport {
            workspace_id: uuid::Uuid::from_u128(1),
            documents: vec![ExportDocument {
                path: WorkspacePath::new("Notes/One.md").unwrap(),
                document,
                encrypted_crdt_backup: Some(vec![9, 8, 7]),
                has_pending_updates: true,
            }],
            assets: vec![ExportAsset {
                path: WorkspacePath::new("Assets/pixel.png").unwrap(),
                bytes: vec![1, 2, 3],
            }],
            empty_folders: vec![WorkspacePath::new("Empty").unwrap()],
        }
    }

    #[test]
    fn archive_is_deterministic_and_complete() {
        let bytes = build_vault_archive(&fixture()).unwrap();
        assert_eq!(bytes, build_vault_archive(&fixture()).unwrap());
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        for name in [
            "manifest.json",
            "Notes/One.md",
            "Assets/pixel.png",
            "Empty/",
            ".inkstone/crdt/00000000-0000-0000-0000-000000000002.bin",
        ] {
            assert!(archive.by_name(name).is_ok(), "missing {name}");
        }
        let mut markdown = String::new();
        archive
            .by_name("Notes/One.md")
            .unwrap()
            .read_to_string(&mut markdown)
            .unwrap();
        assert_eq!(markdown, "# Title\n\nBody\n\n");
        let imported = read_vault_archive(&build_vault_archive(&fixture()).unwrap()).unwrap();
        assert_eq!(imported.manifest.version, 2);
        assert_eq!(
            imported.files[&WorkspacePath::new("Assets/pixel.png").unwrap()],
            vec![1, 2, 3]
        );
        assert_eq!(
            imported.crdt_backups[&uuid::Uuid::from_u128(2)],
            vec![9, 8, 7]
        );
    }

    #[test]
    fn import_rejects_tampered_asset_before_returning_state() {
        let mut export = fixture();
        export.assets[0].bytes = vec![4, 5, 6];
        let bytes = build_vault_archive(&export).unwrap();
        let imported = read_vault_archive(&bytes).unwrap();
        assert_eq!(imported.files.len(), 2);
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_reader(archive.by_name("manifest.json").unwrap()).unwrap();
        manifest["assets"][0]["sha256"] = "wrong".into();
        assert_ne!(
            manifest["assets"][0]["sha256"],
            imported.manifest.assets[0].sha256
        );
    }
}
