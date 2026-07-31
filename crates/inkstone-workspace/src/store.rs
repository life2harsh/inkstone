use crate::path::WorkspacePath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEntryKind {
    Folder,
    Markdown,
    Asset,
    Internal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceEntry {
    pub path: WorkspacePath,
    pub kind: WorkspaceEntryKind,
    pub size_bytes: u64,
}

impl WorkspaceEntry {
    pub fn folder(path: &WorkspacePath) -> Self {
        Self {
            path: path.clone(),
            kind: WorkspaceEntryKind::Folder,
            size_bytes: 0,
        }
    }
    pub fn classify(path: &WorkspacePath, size_bytes: u64) -> Self {
        let kind = if path.is_internal() {
            WorkspaceEntryKind::Internal
        } else if path.is_markdown() {
            WorkspaceEntryKind::Markdown
        } else {
            WorkspaceEntryKind::Asset
        };
        WorkspaceEntry {
            path: path.clone(),
            kind,
            size_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WorkspaceStoreError {
    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("path not found: {0}")]
    NotFound(String),

    #[error("path already exists: {0}")]
    AlreadyExists(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("unsupported workspace format version: {0}")]
    UnsupportedFormatVersion(u32),

    #[error("backend error: {0}")]
    Backend(String),
}

#[async_trait::async_trait(?Send)]
pub trait WorkspaceStore {
    async fn load_manifest(&self) -> Result<Option<crate::WorkspaceManifest>, WorkspaceStoreError>;
    async fn save_manifest(
        &self,
        manifest: &crate::WorkspaceManifest,
    ) -> Result<(), WorkspaceStoreError>;

    async fn list_entries(&self) -> Result<Vec<WorkspaceEntry>, WorkspaceStoreError>;

    async fn read_file(&self, path: &WorkspacePath)
    -> Result<Option<Vec<u8>>, WorkspaceStoreError>;

    async fn write_file(
        &self,
        path: &WorkspacePath,
        data: &[u8],
    ) -> Result<(), WorkspaceStoreError>;

    async fn delete_file(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError>;

    async fn rename_file(
        &self,
        src: &WorkspacePath,
        dst: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError>;

    async fn create_folder(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        Err(WorkspaceStoreError::Backend(format!(
            "folders are not supported by this backend: {}",
            path.as_str()
        )))
    }
    async fn rename_tree(
        &self,
        src: &WorkspacePath,
        dst: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError> {
        let _ = dst;
        Err(WorkspaceStoreError::Backend(format!(
            "folder moves are not supported by this backend: {}",
            src.as_str()
        )))
    }
    async fn delete_tree(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        Err(WorkspaceStoreError::Backend(format!(
            "folder deletion is not supported by this backend: {}",
            path.as_str()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_markdown() {
        let p = WorkspacePath::new("Notes.md").unwrap();
        let entry = WorkspaceEntry::classify(&p, 42);
        assert_eq!(entry.kind, WorkspaceEntryKind::Markdown);
        assert_eq!(entry.size_bytes, 42);
    }

    #[test]
    fn classify_asset() {
        let p = WorkspacePath::new("image.png").unwrap();
        let entry = WorkspaceEntry::classify(&p, 1024);
        assert_eq!(entry.kind, WorkspaceEntryKind::Asset);
    }

    #[test]
    fn classify_internal() {
        let p = WorkspacePath::new(".inkstone/workspace.toml").unwrap();
        let entry = WorkspaceEntry::classify(&p, 128);
        assert_eq!(entry.kind, WorkspaceEntryKind::Internal);
    }

    #[test]
    fn classify_md_inside_dot_inkstone_is_internal_not_markdown() {
        let p = WorkspacePath::new(".inkstone/config.md").unwrap();
        let entry = WorkspaceEntry::classify(&p, 64);
        assert_eq!(entry.kind, WorkspaceEntryKind::Internal);
    }
}
