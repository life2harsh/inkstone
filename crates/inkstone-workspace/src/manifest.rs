use crate::WorkspaceStoreError;

pub const WORKSPACE_FORMAT_VERSION: u32 = 1;
pub const WORKSPACE_MANIFEST_PATH: &str = ".inkstone/workspace.toml";

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceManifest {
    pub format_version: u32,
    pub workspace_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ManifestData {
    format_version: u32,
    workspace_id: uuid::Uuid,
}

pub fn serialize_manifest_toml(
    manifest: &WorkspaceManifest,
) -> Result<String, WorkspaceStoreError> {
    if manifest.format_version != WORKSPACE_FORMAT_VERSION {
        return Err(WorkspaceStoreError::UnsupportedFormatVersion(
            manifest.format_version,
        ));
    }
    let data = ManifestData {
        format_version: manifest.format_version,
        workspace_id: manifest.workspace_id,
    };
    toml::to_string(&data)
        .map_err(|e| WorkspaceStoreError::Serialization(format!("TOML serialize: {}", e)))
}

pub fn deserialize_manifest_toml(input: &str) -> Result<WorkspaceManifest, WorkspaceStoreError> {
    let raw: toml::Value = toml::from_str(input)
        .map_err(|e| WorkspaceStoreError::Serialization(format!("TOML deserialize: {}", e)))?;

    let format_version = raw
        .get("format_version")
        .and_then(|v| v.as_integer())
        .map(|n| n as u32)
        .ok_or_else(|| {
            WorkspaceStoreError::Serialization("missing or non-integer format_version".into())
        })?;

    if format_version != WORKSPACE_FORMAT_VERSION {
        return Err(WorkspaceStoreError::UnsupportedFormatVersion(
            format_version,
        ));
    }

    let data: ManifestData = toml::from_str(input)
        .map_err(|e| WorkspaceStoreError::Serialization(format!("TOML deserialize: {}", e)))?;

    Ok(WorkspaceManifest {
        format_version: data.format_version,
        workspace_id: data.workspace_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_v1_round_trips() {
        let id = uuid::Uuid::new_v4();
        let m = WorkspaceManifest {
            format_version: 1,
            workspace_id: id,
        };
        let toml = serialize_manifest_toml(&m).unwrap();
        let back = deserialize_manifest_toml(&toml).unwrap();
        assert_eq!(back.format_version, 1);
        assert_eq!(back.workspace_id, id);
    }

    #[test]
    fn uuid_survives_round_trip_exactly() {
        let id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let m = WorkspaceManifest {
            format_version: 1,
            workspace_id: id,
        };
        let toml = serialize_manifest_toml(&m).unwrap();
        assert!(toml.contains("550e8400-e29b-41d4-a716-446655440000"));
        let back = deserialize_manifest_toml(&toml).unwrap();
        assert_eq!(back.workspace_id, id);
    }

    #[test]
    fn unsupported_future_version_returns_error() {
        let input = r#"
format_version = 999
workspace_id = "550e8400-e29b-41d4-a716-446655440000"
"#;
        let err = deserialize_manifest_toml(input).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceStoreError::UnsupportedFormatVersion(999)
        ));
    }

    #[test]
    fn version_mismatch_uses_typed_error_string() {
        let input = r#"
format_version = 2
workspace_id = "550e8400-e29b-41d4-a716-446655440000"
"#;
        let err_str = deserialize_manifest_toml(input).unwrap_err().to_string();
        assert!(err_str.contains("2"));
    }

    #[test]
    fn malformed_toml_returns_serialization_error() {
        let err = deserialize_manifest_toml("not valid toml {{{").unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::Serialization(_)));
    }

    #[test]
    fn empty_string_returns_error() {
        let err = deserialize_manifest_toml("").unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::Serialization(_)));
    }

    #[test]
    fn missing_fields_returns_error() {
        let err = deserialize_manifest_toml("format_version = 1").unwrap_err();
        assert!(matches!(err, WorkspaceStoreError::Serialization(_)));
    }

    #[test]
    fn serialize_rejects_unsupported_version() {
        let m = WorkspaceManifest {
            format_version: 2,
            workspace_id: uuid::Uuid::new_v4(),
        };
        let err = serialize_manifest_toml(&m).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceStoreError::UnsupportedFormatVersion(2)
        ));
    }

    #[test]
    fn deserialize_probes_version_before_schema() {
        let input = r#"
format_version = 999
# intentionally missing workspace_id to prove the probe runs first
"#;
        let err = deserialize_manifest_toml(input).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceStoreError::UnsupportedFormatVersion(999)
        ));
    }

    #[test]
    fn deserialize_future_version_non_v1_schema() {
        let input = r#"
format_version = 2
some_new_field = "hello"
"#;
        let err = deserialize_manifest_toml(input).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceStoreError::UnsupportedFormatVersion(2)
        ));
    }
}
