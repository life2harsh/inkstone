use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::manifest::{
    WORKSPACE_MANIFEST_PATH, WorkspaceManifest, deserialize_manifest_toml, serialize_manifest_toml,
};
use crate::path::WorkspacePath;
use crate::store::{WorkspaceEntry, WorkspaceStore, WorkspaceStoreError};

pub struct MemoryWorkspaceStore {
    files: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl MemoryWorkspaceStore {
    pub fn new() -> Self {
        MemoryWorkspaceStore {
            files: RefCell::new(BTreeMap::new()),
        }
    }
}

impl Default for MemoryWorkspaceStore {
    fn default() -> Self {
        Self::new()
    }
}

fn manifest_path() -> WorkspacePath {
    WorkspacePath::new(WORKSPACE_MANIFEST_PATH).unwrap()
}

#[async_trait::async_trait(?Send)]
impl WorkspaceStore for MemoryWorkspaceStore {
    async fn load_manifest(&self) -> Result<Option<WorkspaceManifest>, WorkspaceStoreError> {
        let path = manifest_path();
        let key = path.as_str().to_string();
        let files = self.files.borrow();
        match files.get(&key) {
            Some(data) => {
                let text = std::str::from_utf8(data).map_err(|e| {
                    WorkspaceStoreError::Serialization(format!("UTF-8 decode: {}", e))
                })?;
                deserialize_manifest_toml(text).map(Some)
            }
            None => Ok(None),
        }
    }

    async fn save_manifest(&self, manifest: &WorkspaceManifest) -> Result<(), WorkspaceStoreError> {
        let path = manifest_path();
        let toml = serialize_manifest_toml(manifest)?;
        self.files
            .borrow_mut()
            .insert(path.as_str().to_string(), toml.into_bytes());
        Ok(())
    }

    async fn list_entries(&self) -> Result<Vec<WorkspaceEntry>, WorkspaceStoreError> {
        let files = self.files.borrow();
        let mut entries: Vec<WorkspaceEntry> = files
            .keys()
            .map(|key| {
                let path = WorkspacePath::new(key).expect("stored paths are valid");
                let size = files.get(key).map(|d| d.len() as u64).unwrap_or(0);
                WorkspaceEntry::classify(&path, size)
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    async fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Result<Option<Vec<u8>>, WorkspaceStoreError> {
        Ok(self.files.borrow().get(path.as_str()).cloned())
    }

    async fn write_file(
        &self,
        path: &WorkspacePath,
        data: &[u8],
    ) -> Result<(), WorkspaceStoreError> {
        self.files
            .borrow_mut()
            .insert(path.as_str().to_string(), data.to_vec());
        Ok(())
    }

    async fn delete_file(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        match self.files.borrow_mut().remove(path.as_str()) {
            Some(_) => Ok(()),
            None => Err(WorkspaceStoreError::NotFound(path.as_str().to_string())),
        }
    }

    async fn rename_file(
        &self,
        src: &WorkspacePath,
        dst: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError> {
        let data = self
            .files
            .borrow_mut()
            .remove(src.as_str())
            .ok_or_else(|| WorkspaceStoreError::NotFound(src.as_str().to_string()))?;

        if self.files.borrow().contains_key(dst.as_str()) {
            self.files
                .borrow_mut()
                .insert(src.as_str().to_string(), data);
            return Err(WorkspaceStoreError::AlreadyExists(dst.as_str().to_string()));
        }

        self.files
            .borrow_mut()
            .insert(dst.as_str().to_string(), data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceManifest;
    use crate::store::WorkspaceEntryKind;

    fn test_manifest() -> WorkspaceManifest {
        WorkspaceManifest {
            format_version: 1,
            workspace_id: uuid::Uuid::new_v4(),
        }
    }

    #[tokio::test]
    async fn empty_store_has_no_manifest() {
        let store = MemoryWorkspaceStore::new();
        let m = store.load_manifest().await.unwrap();
        assert!(m.is_none());
    }

    #[tokio::test]
    async fn save_and_load_manifest_round_trip() {
        let store = MemoryWorkspaceStore::new();
        let m = test_manifest();
        store.save_manifest(&m).await.unwrap();
        let loaded = store.load_manifest().await.unwrap().unwrap();
        assert_eq!(loaded.format_version, m.format_version);
        assert_eq!(loaded.workspace_id, m.workspace_id);
    }

    #[tokio::test]
    async fn write_and_read_markdown_file() {
        let store = MemoryWorkspaceStore::new();
        let path = WorkspacePath::new("Notes.md").unwrap();
        let content = b"# Hello World";
        store.write_file(&path, content).await.unwrap();
        let read_back = store.read_file(&path).await.unwrap().unwrap();
        assert_eq!(read_back, content);
    }

    #[tokio::test]
    async fn write_and_read_binary_asset() {
        let store = MemoryWorkspaceStore::new();
        let path = WorkspacePath::new("images/diagram.png").unwrap();
        let data: Vec<u8> = (0..255).collect();
        store.write_file(&path, &data).await.unwrap();
        let read_back = store.read_file(&path).await.unwrap().unwrap();
        assert_eq!(read_back, data);
    }

    #[tokio::test]
    async fn overwrite_file() {
        let store = MemoryWorkspaceStore::new();
        let path = WorkspacePath::new("note.md").unwrap();
        store.write_file(&path, b"v1").await.unwrap();
        store.write_file(&path, b"v2").await.unwrap();
        let read_back = store.read_file(&path).await.unwrap().unwrap();
        assert_eq!(read_back, b"v2");
    }

    #[tokio::test]
    async fn delete_file() {
        let store = MemoryWorkspaceStore::new();
        let path = WorkspacePath::new("note.md").unwrap();
        store.write_file(&path, b"data").await.unwrap();
        store.delete_file(&path).await.unwrap();
        let read_back = store.read_file(&path).await.unwrap();
        assert!(read_back.is_none());
    }

    #[tokio::test]
    async fn delete_missing_file_fails() {
        let store = MemoryWorkspaceStore::new();
        let path = WorkspacePath::new("nonexistent.md").unwrap();
        let err = store.delete_file(&path).await.unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn rename_file() {
        let store = MemoryWorkspaceStore::new();
        let src = WorkspacePath::new("old.md").unwrap();
        let dst = WorkspacePath::new("new.md").unwrap();
        store.write_file(&src, b"data").await.unwrap();
        store.rename_file(&src, &dst).await.unwrap();
        assert!(store.read_file(&src).await.unwrap().is_none());
        assert_eq!(store.read_file(&dst).await.unwrap().unwrap(), b"data");
    }

    #[tokio::test]
    async fn rename_missing_source_fails() {
        let store = MemoryWorkspaceStore::new();
        let src = WorkspacePath::new("missing.md").unwrap();
        let dst = WorkspacePath::new("dest.md").unwrap();
        let err = store.rename_file(&src, &dst).await.unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn rename_over_existing_destination_fails() {
        let store = MemoryWorkspaceStore::new();
        let src = WorkspacePath::new("src.md").unwrap();
        let dst = WorkspacePath::new("dst.md").unwrap();
        store.write_file(&src, b"src data").await.unwrap();
        store.write_file(&dst, b"dst data").await.unwrap();
        let err = store.rename_file(&src, &dst).await.unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::AlreadyExists(_)));
        // Source must still exist
        assert!(store.read_file(&src).await.unwrap().is_some());
        // Destination unchanged
        assert_eq!(store.read_file(&dst).await.unwrap().unwrap(), b"dst data");
    }

    #[tokio::test]
    async fn list_entries_deterministic_ordering() {
        let store = MemoryWorkspaceStore::new();
        store
            .write_file(&WorkspacePath::new("z.md").unwrap(), b"z")
            .await
            .unwrap();
        store
            .write_file(&WorkspacePath::new("a.md").unwrap(), b"a")
            .await
            .unwrap();
        store
            .write_file(&WorkspacePath::new("m.md").unwrap(), b"m")
            .await
            .unwrap();
        let entries = store.list_entries().await.unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a.md", "m.md", "z.md"]);
    }

    #[tokio::test]
    async fn entry_kind_classification() {
        let store = MemoryWorkspaceStore::new();
        store
            .write_file(&WorkspacePath::new("doc.md").unwrap(), b"md")
            .await
            .unwrap();
        store
            .write_file(&WorkspacePath::new("img.png").unwrap(), b"png")
            .await
            .unwrap();
        store
            .write_file(
                &WorkspacePath::new(".inkstone/workspace.toml").unwrap(),
                b"toml",
            )
            .await
            .unwrap();
        let entries = store.list_entries().await.unwrap();
        // Lexicographic order: .inkstone/..., doc.md, img.png
        assert_eq!(entries[0].kind, WorkspaceEntryKind::Internal);
        assert_eq!(entries[1].kind, WorkspaceEntryKind::Markdown);
        assert_eq!(entries[2].kind, WorkspaceEntryKind::Asset);
    }

    #[tokio::test]
    async fn internal_path_classification() {
        let store = MemoryWorkspaceStore::new();
        store
            .write_file(
                &WorkspacePath::new(".inkstone/workspace.toml").unwrap(),
                b"data",
            )
            .await
            .unwrap();
        let entries = store.list_entries().await.unwrap();
        assert_eq!(entries[0].kind, WorkspaceEntryKind::Internal);
    }

    #[tokio::test]
    async fn invalid_traversal_path_rejected_before_store_access() {
        let err = WorkspacePath::new("../escape.md").unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::InvalidPath(_)));
    }

    #[tokio::test]
    async fn binary_bytes_round_trip_exactly() {
        let store = MemoryWorkspaceStore::new();
        let path = WorkspacePath::new("data.bin").unwrap();
        let data: Vec<u8> = (0..255).chain((0..255).rev()).collect();
        store.write_file(&path, &data).await.unwrap();
        let read_back = store.read_file(&path).await.unwrap().unwrap();
        assert_eq!(read_back, data);
    }
}
