use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
use crate::ids::{BlockId, DocId};

pub const DOCUMENT_UPDATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentUpdate {
    pub version: u32,
    pub update_id: Uuid,
    pub doc_id: DocId,
    pub applied_at_ms: i64,
    pub op: DocumentOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DocumentOp {
    #[serde(rename = "insert_block")]
    InsertBlock { idx: usize, block: DocumentBlock },

    #[serde(rename = "delete_block")]
    DeleteBlock { block_id: BlockId },

    #[serde(rename = "move_block")]
    MoveBlock { block_id: BlockId, new_idx: usize },

    #[serde(rename = "replace_block_content")]
    ReplaceBlockContent {
        block_id: BlockId,
        content: BlockContent,
    },

    #[serde(rename = "set_title")]
    SetTitle { title: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("update doc_id does not match document id")]
    WrongDocumentId,

    #[error("block with id {0} already exists")]
    DuplicateBlockId(BlockId),

    #[error("block with id {0} not found")]
    BlockNotFound(BlockId),

    #[error("insert index {0} out of bounds (len {1})")]
    InsertIndexOutOfBounds(usize, usize),

    #[error("move index {0} out of bounds, must be < {1}")]
    MoveIndexOutOfBounds(usize, usize),

    #[error("unsupported document schema version {0}")]
    UnsupportedDocumentSchemaVersion(u32),

    #[error("unsupported update version {0}")]
    UnsupportedUpdateVersion(u32),
}

pub fn apply_update(doc: &mut InkstoneDocument, update: DocumentUpdate) -> Result<(), ApplyError> {
    if doc.schema_version != crate::document::DOCUMENT_SCHEMA_VERSION {
        return Err(ApplyError::UnsupportedDocumentSchemaVersion(
            doc.schema_version,
        ));
    }
    if update.version != DOCUMENT_UPDATE_VERSION {
        return Err(ApplyError::UnsupportedUpdateVersion(update.version));
    }
    if update.doc_id != doc.id {
        return Err(ApplyError::WrongDocumentId);
    }

    match update.op {
        DocumentOp::InsertBlock { idx, block } => {
            if idx > doc.blocks.len() {
                return Err(ApplyError::InsertIndexOutOfBounds(idx, doc.blocks.len()));
            }
            if doc.block_index(block.id).is_some() {
                return Err(ApplyError::DuplicateBlockId(block.id));
            }
            doc.blocks.insert(idx, block);
        }

        DocumentOp::DeleteBlock { block_id } => {
            let pos = doc
                .block_index(block_id)
                .ok_or(ApplyError::BlockNotFound(block_id))?;
            doc.blocks.remove(pos);
        }

        DocumentOp::MoveBlock { block_id, new_idx } => {
            let pos = doc
                .block_index(block_id)
                .ok_or(ApplyError::BlockNotFound(block_id))?;
            if new_idx >= doc.blocks.len() {
                return Err(ApplyError::MoveIndexOutOfBounds(new_idx, doc.blocks.len()));
            }
            let block = doc.blocks.remove(pos);
            doc.blocks.insert(new_idx, block);
        }

        DocumentOp::ReplaceBlockContent { block_id, content } => {
            let block = doc
                .get_block_mut(block_id)
                .ok_or(ApplyError::BlockNotFound(block_id))?;
            block.content = content;
            block.updated_at_ms = update.applied_at_ms;
        }

        DocumentOp::SetTitle { title } => {
            doc.title = title;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::BlockContent;

    fn make_doc() -> (InkstoneDocument, DocId) {
        let doc = InkstoneDocument::new(DocId::new_v4());
        let id = doc.id;
        (doc, id)
    }

    fn markdown(text: &str) -> BlockContent {
        BlockContent::Markdown {
            text: text.to_string(),
        }
    }

    fn upd(doc_id: DocId, applied_at_ms: i64, op: DocumentOp) -> DocumentUpdate {
        DocumentUpdate {
            version: DOCUMENT_UPDATE_VERSION,
            update_id: Uuid::new_v4(),
            doc_id,
            applied_at_ms,
            op,
        }
    }

    #[test]
    fn test_insert_block_preserves_order() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("first"));
        let b2 = DocumentBlock::new(BlockId::new_v4(), markdown("second"));
        let b3 = DocumentBlock::new(BlockId::new_v4(), markdown("third"));

        apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::InsertBlock {
                    idx: 0,
                    block: b1.clone(),
                },
            ),
        )
        .unwrap();
        apply_update(
            &mut doc,
            upd(
                doc_id,
                1001,
                DocumentOp::InsertBlock {
                    idx: 1,
                    block: b2.clone(),
                },
            ),
        )
        .unwrap();
        apply_update(
            &mut doc,
            upd(
                doc_id,
                1002,
                DocumentOp::InsertBlock {
                    idx: 1,
                    block: b3.clone(),
                },
            ),
        )
        .unwrap();

        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(doc.blocks[0].id, b1.id);
        assert_eq!(doc.blocks[1].id, b3.id);
        assert_eq!(doc.blocks[2].id, b2.id);
    }

    #[test]
    fn test_duplicate_block_id_rejected() {
        let (mut doc, doc_id) = make_doc();
        let id = BlockId::new_v4();
        apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::InsertBlock {
                    idx: 0,
                    block: DocumentBlock::new(id, markdown("a")),
                },
            ),
        )
        .unwrap();
        let err = apply_update(
            &mut doc,
            upd(
                doc_id,
                1001,
                DocumentOp::InsertBlock {
                    idx: 0,
                    block: DocumentBlock::new(id, markdown("b")),
                },
            ),
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::DuplicateBlockId(id));
    }

    #[test]
    fn test_delete_existing_block() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("a"));
        let b2 = DocumentBlock::new(BlockId::new_v4(), markdown("b"));
        let id1 = b1.id;
        doc.blocks.push(b1);
        doc.blocks.push(b2);

        apply_update(
            &mut doc,
            upd(doc_id, 1000, DocumentOp::DeleteBlock { block_id: id1 }),
        )
        .unwrap();
        assert_eq!(doc.blocks.len(), 1);
        assert!(!doc.blocks.iter().any(|b| b.id == id1));
    }

    #[test]
    fn test_delete_missing_block_rejected() {
        let (mut doc, doc_id) = make_doc();
        let missing = BlockId::new_v4();
        let err = apply_update(
            &mut doc,
            upd(doc_id, 1000, DocumentOp::DeleteBlock { block_id: missing }),
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::BlockNotFound(missing));
    }

    #[test]
    fn test_move_block_forward() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("a"));
        let b2 = DocumentBlock::new(BlockId::new_v4(), markdown("b"));
        let b3 = DocumentBlock::new(BlockId::new_v4(), markdown("c"));
        let id1 = b1.id;
        doc.blocks.extend(vec![b1, b2, b3]);

        apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::MoveBlock {
                    block_id: id1,
                    new_idx: 2,
                },
            ),
        )
        .unwrap();
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(doc.blocks[2].id, id1);
    }

    #[test]
    fn test_move_block_backward() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("a"));
        let b2 = DocumentBlock::new(BlockId::new_v4(), markdown("b"));
        let b3 = DocumentBlock::new(BlockId::new_v4(), markdown("c"));
        let id3 = b3.id;
        doc.blocks.extend(vec![b1, b2, b3]);

        apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::MoveBlock {
                    block_id: id3,
                    new_idx: 0,
                },
            ),
        )
        .unwrap();
        assert_eq!(doc.blocks[0].id, id3);
        assert_eq!(doc.blocks.len(), 3);
    }

    #[test]
    fn test_move_preserves_id_and_content() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("a"));
        let b2 = DocumentBlock::new(BlockId::new_v4(), markdown("b"));
        let id1 = b1.id;
        doc.blocks.extend(vec![b1, b2]);

        apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::MoveBlock {
                    block_id: id1,
                    new_idx: 1,
                },
            ),
        )
        .unwrap();
        let moved = &doc.blocks[1];
        assert_eq!(moved.id, id1);
        match &moved.content {
            BlockContent::Markdown { text } => assert_eq!(text, "a"),
            _ => panic!("wrong content"),
        }
    }

    #[test]
    fn test_replace_markdown_content() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("old"));
        let id = b1.id;
        doc.blocks.push(b1);

        apply_update(
            &mut doc,
            upd(
                doc_id,
                2000,
                DocumentOp::ReplaceBlockContent {
                    block_id: id,
                    content: markdown("new"),
                },
            ),
        )
        .unwrap();
        match &doc.blocks[0].content {
            BlockContent::Markdown { text } => assert_eq!(text, "new"),
            _ => panic!("wrong content"),
        }
    }

    #[test]
    fn test_replace_ink_payload_opaque() {
        let (mut doc, doc_id) = make_doc();
        let b1 = DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Ink {
                stonepen_json: "{}".into(),
            },
        );
        let id = b1.id;
        doc.blocks.push(b1);

        apply_update(
            &mut doc,
            upd(
                doc_id,
                3000,
                DocumentOp::ReplaceBlockContent {
                    block_id: id,
                    content: BlockContent::Ink {
                        stonepen_json: "{\"strokes\":[]}".into(),
                    },
                },
            ),
        )
        .unwrap();
        match &doc.blocks[0].content {
            BlockContent::Ink { stonepen_json } => {
                assert_eq!(stonepen_json, "{\"strokes\":[]}");
                assert!(stonepen_json.contains("strokes"));
            }
            _ => panic!("wrong content"),
        }
    }

    #[test]
    fn test_wrong_document_id_rejected() {
        let (mut doc, _) = make_doc();
        let err = apply_update(
            &mut doc,
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: Uuid::new_v4(),
                doc_id: DocId::new_v4(),
                applied_at_ms: 1000,
                op: DocumentOp::SetTitle {
                    title: "test".into(),
                },
            },
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::WrongDocumentId);
    }

    #[test]
    fn test_set_title() {
        let (mut doc, doc_id) = make_doc();
        apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::SetTitle {
                    title: "My Document".into(),
                },
            ),
        )
        .unwrap();
        assert_eq!(doc.title, "My Document");
    }

    #[test]
    fn test_update_serde_roundtrip_preserves_doc_id() {
        let doc_id = DocId::new_v4();
        let update = DocumentUpdate {
            version: DOCUMENT_UPDATE_VERSION,
            update_id: Uuid::new_v4(),
            doc_id,
            applied_at_ms: 1000,
            op: DocumentOp::InsertBlock {
                idx: 0,
                block: DocumentBlock::new(BlockId::new_v4(), markdown("hello")),
            },
        };
        let json = serde_json::to_string(&update).unwrap();
        let back: DocumentUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(back.doc_id, doc_id);
        assert_eq!(back.version, DOCUMENT_UPDATE_VERSION);
        assert_eq!(back.applied_at_ms, 1000);
        match back.op {
            DocumentOp::InsertBlock { idx, block } => {
                assert_eq!(idx, 0);
                match block.content {
                    BlockContent::Markdown { text } => assert_eq!(text, "hello"),
                    _ => panic!("wrong content"),
                }
            }
            _ => panic!("wrong op"),
        }
    }

    #[test]
    fn test_insert_index_out_of_bounds() {
        let (mut doc, doc_id) = make_doc();
        let err = apply_update(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::InsertBlock {
                    idx: 5,
                    block: DocumentBlock::new(BlockId::new_v4(), markdown("a")),
                },
            ),
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::InsertIndexOutOfBounds(5, 0));
    }

    #[test]
    fn test_replace_block_content_uses_applied_at_ms() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let block = DocumentBlock::new(BlockId::new_v4(), markdown("original"));
        let block_id = block.id;
        doc.blocks.push(block);

        let update = DocumentUpdate {
            version: DOCUMENT_UPDATE_VERSION,
            update_id: Uuid::new_v4(),
            doc_id: doc.id,
            applied_at_ms: 999999,
            op: DocumentOp::ReplaceBlockContent {
                block_id,
                content: markdown("updated"),
            },
        };
        apply_update(&mut doc, update).unwrap();
        assert_eq!(doc.blocks[0].updated_at_ms, 999999);
    }

    #[test]
    fn test_deterministic_same_update_identical_result() {
        let id = DocId::new_v4();
        let mut doc_a = InkstoneDocument::new(id);
        let mut doc_b = InkstoneDocument::new(id);

        let b1 = DocumentBlock::new(BlockId::new_v4(), markdown("hello"));
        let b1_id = b1.id;
        doc_a.blocks.push(b1.clone());
        doc_b.blocks.push(b1);

        let update = DocumentUpdate {
            version: DOCUMENT_UPDATE_VERSION,
            update_id: Uuid::new_v4(),
            doc_id: id,
            applied_at_ms: 5000,
            op: DocumentOp::ReplaceBlockContent {
                block_id: b1_id,
                content: markdown("world"),
            },
        };

        apply_update(&mut doc_a, update.clone()).unwrap();
        apply_update(&mut doc_b, update).unwrap();

        let json_a = serde_json::to_string(&doc_a).unwrap();
        let json_b = serde_json::to_string(&doc_b).unwrap();
        assert_eq!(json_a, json_b);
    }

    #[test]
    fn test_unsupported_document_schema_rejected() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.schema_version = 99;
        let doc_id = doc.id;
        let err = apply_update(
            &mut doc,
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: Uuid::new_v4(),
                doc_id,
                applied_at_ms: 1000,
                op: DocumentOp::SetTitle { title: "x".into() },
            },
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::UnsupportedDocumentSchemaVersion(99));
    }

    #[test]
    fn test_unsupported_update_version_rejected() {
        let (mut doc, doc_id) = make_doc();
        let err = apply_update(
            &mut doc,
            DocumentUpdate {
                version: 99,
                update_id: Uuid::new_v4(),
                doc_id,
                applied_at_ms: 1000,
                op: DocumentOp::SetTitle { title: "x".into() },
            },
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::UnsupportedUpdateVersion(99));
    }
}
