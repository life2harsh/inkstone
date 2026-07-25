use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{BlockId, DocId};

pub const DOCUMENT_SCHEMA_VERSION: u32 = 1;
pub const INK_COLLABORATION_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkstoneDocument {
    pub id: DocId,
    pub schema_version: u32,
    pub title: String,
    pub blocks: Vec<DocumentBlock>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentBlock {
    pub id: BlockId,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub content: BlockContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkCollaborationState {
    pub version: u32,
    pub stream_epoch: uuid::Uuid,
    /// Stonepen-owned opaque collaboration snapshot.
    pub snapshot: Vec<u8>,
}

impl DocumentBlock {
    pub fn new(id: BlockId, content: BlockContent) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id,
            created_at_ms: now,
            updated_at_ms: now,
            content,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BlockContent {
    #[serde(rename = "markdown")]
    Markdown { text: String },
    #[serde(rename = "math")]
    Math { source: String },
    #[serde(rename = "ink")]
    Ink {
        /// Inkstone owns block ID.
        /// Stonepen owns all ink content inside stonepen_json.
        /// Inkstone loads the payload through StonepenHandle::load_json
        /// and receives current payload through StonepenHandle::export_json.
        /// No conversion to Inkstone strokes. No duplicated InkStroke structs.
        stonepen_json: String,
        /// Absent only for legacy JSON-only Ink blocks awaiting migration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collaboration: Option<InkCollaborationState>,
    },
    #[serde(rename = "divider")]
    Divider,
}

pub fn new_opaque_ink_content(
    width: f32,
    height: f32,
    stream_epoch: uuid::Uuid,
) -> Result<BlockContent, String> {
    let (snapshot, stonepen_json) =
        stonepen_core::new_opaque_document(width, height).map_err(|error| error.to_string())?;
    Ok(BlockContent::Ink {
        stonepen_json,
        collaboration: Some(InkCollaborationState {
            version: INK_COLLABORATION_VERSION,
            stream_epoch,
            snapshot,
        }),
    })
}

impl InkstoneDocument {
    pub fn new(id: DocId) -> Self {
        Self {
            id,
            schema_version: DOCUMENT_SCHEMA_VERSION,
            title: String::new(),
            blocks: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn get_block(&self, id: BlockId) -> Option<&DocumentBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }

    pub fn get_block_mut(&mut self, id: BlockId) -> Option<&mut DocumentBlock> {
        self.blocks.iter_mut().find(|b| b.id == id)
    }

    pub fn block_index(&self, id: BlockId) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == id)
    }
}

/// Create a new empty document with the given id.
pub fn new_document(id: DocId) -> InkstoneDocument {
    InkstoneDocument::new(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::BlockId;

    #[test]
    fn test_empty_document() {
        let id = DocId::new_v4();
        let doc = InkstoneDocument::new(id);
        assert_eq!(doc.id, id);
        assert_eq!(doc.schema_version, DOCUMENT_SCHEMA_VERSION);
        assert!(doc.title.is_empty());
        assert!(doc.blocks.is_empty());
    }

    #[test]
    fn test_doc_id_serializes_as_uuid_string() {
        let id = DocId::new_v4();
        let json = serde_json::to_string(&id).unwrap();
        let uuid_str = id.0.to_string();
        assert_eq!(json, format!("\"{}\"", uuid_str));
    }

    #[test]
    fn test_block_id_serializes_as_uuid_string() {
        let id = BlockId::new_v4();
        let json = serde_json::to_string(&id).unwrap();
        let uuid_str = id.0.to_string();
        assert_eq!(json, format!("\"{}\"", uuid_str));
    }

    #[test]
    fn test_doc_id_deserialize_from_uuid_string() {
        let uuid = uuid::Uuid::new_v4();
        let json = format!("\"{}\"", uuid);
        let id: DocId = serde_json::from_str(&json).unwrap();
        assert_eq!(id.0, uuid);
    }

    #[test]
    fn test_block_id_deserialize_from_uuid_string() {
        let uuid = uuid::Uuid::new_v4();
        let json = format!("\"{}\"", uuid);
        let id: BlockId = serde_json::from_str(&json).unwrap();
        assert_eq!(id.0, uuid);
    }

    #[test]
    fn test_document_block_roundtrip_preserves_block_id() {
        let id = BlockId::new_v4();
        let block = DocumentBlock::new(id, BlockContent::Markdown { text: "x".into() });
        let json = serde_json::to_string(&block).unwrap();
        let back: DocumentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, id);
    }

    #[test]
    fn test_block_new_sets_timestamps() {
        let id = BlockId::new_v4();
        let content = BlockContent::Markdown {
            text: "hello".into(),
        };
        let block = DocumentBlock::new(id, content);
        assert_eq!(block.id, id);
        assert!(block.created_at_ms > 0);
        assert_eq!(block.created_at_ms, block.updated_at_ms);
    }

    #[test]
    fn test_get_block_lookup() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let b1 = DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown { text: "a".into() },
        );
        let b2 = DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown { text: "b".into() },
        );
        let b1_id = b1.id;
        doc.blocks.push(b1);
        doc.blocks.push(b2);

        assert!(doc.get_block(b1_id).is_some());
        assert!(doc.get_block(BlockId::new_v4()).is_none());
        assert_eq!(doc.block_index(b1_id), Some(0));
    }

    #[test]
    fn test_markdown_content() {
        let content = BlockContent::Markdown {
            text: "# Title".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        let back: BlockContent = serde_json::from_str(&json).unwrap();
        match back {
            BlockContent::Markdown { text } => assert_eq!(text, "# Title"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_ink_content_opaque() {
        let content = BlockContent::Ink {
            stonepen_json: "{\"strokes\":[]}".into(),
            collaboration: None,
        };
        let json = serde_json::to_string(&content).unwrap();
        let back: BlockContent = serde_json::from_str(&json).unwrap();
        match back {
            BlockContent::Ink { stonepen_json, .. } => {
                assert_eq!(stonepen_json, "{\"strokes\":[]}");
                assert!(stonepen_json.contains("strokes"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_math_and_divider_content_roundtrip() {
        let contents = [
            BlockContent::Math {
                source: r"P(A|B) = P(B|A)P(A)/P(B)".into(),
            },
            BlockContent::Divider,
        ];
        for content in contents {
            let json = serde_json::to_string(&content).unwrap();
            let back: BlockContent = serde_json::from_str(&json).unwrap();
            match back {
                BlockContent::Math { source } => assert!(source.starts_with("P(A|B)")),
                BlockContent::Divider => {}
                _ => panic!("wrong variant"),
            }
        }
    }

    #[test]
    fn test_mixed_block_roundtrip_preserves_order_and_types() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks = vec![
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Markdown {
                    text: "first".into(),
                },
            ),
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Math {
                    source: "x^2".into(),
                },
            ),
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Ink {
                    stonepen_json: "opaque".into(),
                    collaboration: None,
                },
            ),
            DocumentBlock::new(BlockId::new_v4(), BlockContent::Divider),
        ];

        let json = serde_json::to_string(&doc).unwrap();
        let back: InkstoneDocument = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back.blocks[0].content,
            BlockContent::Markdown { .. }
        ));
        assert!(matches!(back.blocks[1].content, BlockContent::Math { .. }));
        assert!(matches!(back.blocks[2].content, BlockContent::Ink { .. }));
        assert!(matches!(back.blocks[3].content, BlockContent::Divider));
    }

    #[test]
    fn test_document_serde_roundtrip() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.title = "Test".into();
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "content".into(),
            },
        ));
        let json = serde_json::to_string(&doc).unwrap();
        let back: InkstoneDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, doc.id);
        assert_eq!(back.title, "Test");
        assert_eq!(back.blocks.len(), 1);
    }

    #[test]
    fn test_metadata_stable_serialization_order() {
        let mut doc1 = InkstoneDocument::new(DocId::new_v4());
        doc1.metadata.insert("z_key".into(), "last".into());
        doc1.metadata.insert("a_key".into(), "first".into());

        let mut doc2 = InkstoneDocument::new(doc1.id);
        doc2.metadata.insert("a_key".into(), "first".into());
        doc2.metadata.insert("z_key".into(), "last".into());

        let json1 = serde_json::to_string(&doc1).unwrap();
        let json2 = serde_json::to_string(&doc2).unwrap();
        assert_eq!(json1, json2);
    }
}
