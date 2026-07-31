use crate::WorkspaceStoreError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspacePath(String);

impl WorkspacePath {
    pub fn new<S: AsRef<str>>(raw: S) -> Result<Self, WorkspaceStoreError> {
        let s = raw.as_ref();

        if s.is_empty() {
            return Err(WorkspaceStoreError::InvalidPath(
                "path must not be empty".into(),
            ));
        }

        if s.len() > 1024 {
            return Err(WorkspaceStoreError::InvalidPath(
                "path exceeds 1024 bytes".into(),
            ));
        }

        if s.starts_with('/') {
            return Err(WorkspaceStoreError::InvalidPath(
                "path must not be absolute (no leading slash)".into(),
            ));
        }

        if s.contains('\\') {
            return Err(WorkspaceStoreError::InvalidPath(
                "path must use forward slashes only (no backslash)".into(),
            ));
        }

        if s.contains('\0') {
            return Err(WorkspaceStoreError::InvalidPath(
                "path must not contain NUL byte".into(),
            ));
        }

        for c in s.chars() {
            if c.is_control() {
                return Err(WorkspaceStoreError::InvalidPath(
                    "path must not contain control characters".into(),
                ));
            }
        }

        for segment in s.split('/') {
            if segment.len() > 255 {
                return Err(WorkspaceStoreError::InvalidPath(
                    "path segment exceeds 255 bytes".into(),
                ));
            }
            if segment.is_empty() {
                return Err(WorkspaceStoreError::InvalidPath(
                    "path must not contain empty segments (e.g. trailing slash or //)".into(),
                ));
            }
            if segment == "." {
                return Err(WorkspaceStoreError::InvalidPath(
                    "path must not contain '.' segment".into(),
                ));
            }
            if segment == ".." {
                return Err(WorkspaceStoreError::InvalidPath(
                    "path must not contain '..' segment".into(),
                ));
            }
        }

        Ok(WorkspacePath(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn segments(&self) -> Vec<&str> {
        self.0.split('/').collect()
    }

    pub fn file_name(&self) -> Option<&str> {
        self.0.rsplit('/').next().filter(|s| !s.is_empty())
    }

    pub fn extension(&self) -> Option<&str> {
        self.file_name()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext)
    }

    pub fn is_internal(&self) -> bool {
        self.0 == ".inkstone" || self.0.starts_with(".inkstone/")
    }

    pub fn is_markdown(&self) -> bool {
        !self.is_internal()
            && self
                .extension()
                .map_or(false, |ext| ext.eq_ignore_ascii_case("md"))
    }

    pub fn is_user_content(&self) -> bool {
        !self.is_internal()
    }
}

impl serde::Serialize for WorkspacePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for WorkspacePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        WorkspacePath::new(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_paths_accepted() {
        assert!(WorkspacePath::new("Notes.md").is_ok());
        assert!(WorkspacePath::new("Projects/Inkstone.md").is_ok());
        assert!(WorkspacePath::new("images/diagram.png").is_ok());
        assert!(WorkspacePath::new(".inkstone/workspace.toml").is_ok());
        assert!(WorkspacePath::new(".inkstone").is_ok());
        assert!(WorkspacePath::new("a/b/c/d/e.md").is_ok());
        assert!(WorkspacePath::new("a single file.md").is_ok());
        assert!(WorkspacePath::new("sub/readme.md").is_ok());
    }

    #[test]
    fn empty_rejected() {
        assert!(WorkspacePath::new("").is_err());
    }

    #[test]
    fn absolute_rejected() {
        assert!(WorkspacePath::new("/absolute.md").is_err());
    }

    #[test]
    fn backslash_rejected() {
        assert!(WorkspacePath::new("folder\\file.md").is_err());
        assert!(WorkspacePath::new("folder\\sub\\file.md").is_err());
    }

    #[test]
    fn dot_segment_rejected() {
        assert!(WorkspacePath::new("./note.md").is_err());
        assert!(WorkspacePath::new("folder/./note.md").is_err());
    }

    #[test]
    fn dotdot_segment_rejected() {
        assert!(WorkspacePath::new("../escape.md").is_err());
        assert!(WorkspacePath::new("folder/../escape.md").is_err());
        assert!(WorkspacePath::new("../../escape.md").is_err());
    }

    #[test]
    fn empty_segment_rejected() {
        assert!(WorkspacePath::new("folder//note.md").is_err());
        assert!(WorkspacePath::new("trailing/").is_err());
    }

    #[test]
    fn null_byte_rejected() {
        assert!(WorkspacePath::new("bad\0file.md").is_err());
    }

    #[test]
    fn control_character_rejected() {
        assert!(WorkspacePath::new("bad\x01file.md").is_err());
        assert!(WorkspacePath::new("bad\nfile.md").is_err());
        assert!(WorkspacePath::new("bad\rfile.md").is_err());
        assert!(WorkspacePath::new("bad\tfile.md").is_err());
    }

    #[test]
    fn as_str_returns_raw() {
        let p = WorkspacePath::new("Notes.md").unwrap();
        assert_eq!(p.as_str(), "Notes.md");
    }

    #[test]
    fn segments_split_correctly() {
        let p = WorkspacePath::new("Projects/Inkstone.md").unwrap();
        assert_eq!(p.segments(), vec!["Projects", "Inkstone.md"]);

        let p = WorkspacePath::new("a/b/c.md").unwrap();
        assert_eq!(p.segments(), vec!["a", "b", "c.md"]);
    }

    #[test]
    fn file_name_extracted() {
        let p = WorkspacePath::new("Projects/Inkstone.md").unwrap();
        assert_eq!(p.file_name(), Some("Inkstone.md"));

        let p = WorkspacePath::new("Notes.md").unwrap();
        assert_eq!(p.file_name(), Some("Notes.md"));

        let p = WorkspacePath::new(".inkstone").unwrap();
        assert_eq!(p.file_name(), Some(".inkstone"));
    }

    #[test]
    fn extension_extracted() {
        let p = WorkspacePath::new("note.md").unwrap();
        assert_eq!(p.extension(), Some("md"));

        let p = WorkspacePath::new("image.png").unwrap();
        assert_eq!(p.extension(), Some("png"));

        let p = WorkspacePath::new("noext").unwrap();
        assert_eq!(p.extension(), None);

        let p = WorkspacePath::new(".inkstone/workspace.toml").unwrap();
        assert_eq!(p.extension(), Some("toml"));
    }

    #[test]
    fn is_internal() {
        assert!(WorkspacePath::new(".inkstone").unwrap().is_internal());
        assert!(
            WorkspacePath::new(".inkstone/workspace.toml")
                .unwrap()
                .is_internal()
        );
        assert!(
            WorkspacePath::new(".inkstone/config.toml")
                .unwrap()
                .is_internal()
        );
        assert!(!WorkspacePath::new("Notes.md").unwrap().is_internal());
        assert!(!WorkspacePath::new("sub/Notes.md").unwrap().is_internal());
    }

    #[test]
    fn is_markdown_case_insensitive() {
        assert!(WorkspacePath::new("note.md").unwrap().is_markdown());
        assert!(WorkspacePath::new("note.MD").unwrap().is_markdown());
        assert!(WorkspacePath::new("note.mD").unwrap().is_markdown());
        assert!(!WorkspacePath::new("note.txt").unwrap().is_markdown());
        assert!(!WorkspacePath::new("image.png").unwrap().is_markdown());
        assert!(
            !WorkspacePath::new(".inkstone/config.md")
                .unwrap()
                .is_markdown()
        );
    }

    #[test]
    fn is_user_content() {
        assert!(WorkspacePath::new("Notes.md").unwrap().is_user_content());
        assert!(
            WorkspacePath::new("images/photo.png")
                .unwrap()
                .is_user_content()
        );
        assert!(
            !WorkspacePath::new(".inkstone/workspace.toml")
                .unwrap()
                .is_user_content()
        );
        assert!(!WorkspacePath::new(".inkstone").unwrap().is_user_content());
    }

    #[test]
    fn ordering_is_deterministic() {
        let mut paths: Vec<WorkspacePath> = vec![
            WorkspacePath::new("b.md").unwrap(),
            WorkspacePath::new("a.md").unwrap(),
            WorkspacePath::new("c.md").unwrap(),
        ];
        paths.sort();
        assert_eq!(paths[0].as_str(), "a.md");
        assert_eq!(paths[1].as_str(), "b.md");
        assert_eq!(paths[2].as_str(), "c.md");
    }

    #[test]
    fn eq_and_hash_work() {
        use std::collections::HashSet;
        let a = WorkspacePath::new("note.md").unwrap();
        let b = WorkspacePath::new("note.md").unwrap();
        let c = WorkspacePath::new("other.md").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a.clone());
        set.insert(b.clone());
        assert_eq!(set.len(), 1);
        set.insert(c.clone());
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn serde_round_trip_valid_path() {
        let p = WorkspacePath::new("a/b/c.md").unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"a/b/c.md\"");
        let back: WorkspacePath = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn serde_rejects_newline() {
        let err = serde_json::from_str::<WorkspacePath>("\"bad\\nfile.md\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn serde_rejects_carriage_return() {
        let err = serde_json::from_str::<WorkspacePath>("\"bad\\rfile.md\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn serde_rejects_tab() {
        let err = serde_json::from_str::<WorkspacePath>("\"bad\\tfile.md\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn serde_rejects_empty() {
        let err = serde_json::from_str::<WorkspacePath>("\"\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn serde_rejects_backslash() {
        let err = serde_json::from_str::<WorkspacePath>("\"a\\\\b.md\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn serde_rejects_absolute() {
        let err = serde_json::from_str::<WorkspacePath>("\"/abs.md\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn serde_rejects_dotdot() {
        let err = serde_json::from_str::<WorkspacePath>("\"../x.md\"").unwrap_err();
        assert!(err.is_data());
    }
}
