use serde::{Deserialize, Serialize};

use crate::document::{
    AnchorTarget, BlockContent, Comment, DocumentBlock, INK_COLLABORATION_VERSION, InkAnchor,
    InkCollaborationState, InkstoneDocument, PAGE_INK_LAYER_VERSION, PageInkLayer,
};
use crate::ids::{BlockId, CommentId, DocId, InkGroupId, UpdateId};
use crate::text_crdt::{CollaborativeTextUpdate, TextTarget};

pub const DOCUMENT_UPDATE_VERSION: u32 = 1;
pub const DOCUMENT_UPDATE_V2_VERSION: u32 = 2;
pub const INK_UPDATE_PAYLOAD_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborativeInkUpdate {
    pub version: u32,
    pub block_id: BlockId,
    pub stream_epoch: uuid::Uuid,
    /// Stonepen-owned opaque incremental update bytes.
    #[serde(with = "crate::ink_snapshot::opaque_bytes")]
    pub update: Vec<u8>,
    /// Present only while migrating a legacy JSON block or initializing a new stream.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::ink_snapshot::opaque_bytes_option"
    )]
    pub bootstrap_snapshot: Option<Vec<u8>>,
}

/// Incremental update to the document's page ink layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageInkUpdate {
    pub version: u32,
    pub stream_epoch: uuid::Uuid,
    /// Stonepen-owned opaque incremental update bytes.
    #[serde(with = "crate::ink_snapshot::opaque_bytes")]
    pub update: Vec<u8>,
    /// Present only when this update creates the layer.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::ink_snapshot::opaque_bytes_option"
    )]
    pub bootstrap_snapshot: Option<Vec<u8>>,
}

/// One legacy Ink block folded into the page ink layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkBlockConversion {
    pub block_id: BlockId,
    pub group: InkGroupId,
    pub min_height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentUpdate {
    pub version: u32,
    pub update_id: UpdateId,
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

    #[serde(rename = "apply_text_update_v2")]
    ApplyTextUpdateV2 { payload: CollaborativeTextUpdate },

    #[serde(rename = "apply_ink_update_v2")]
    ApplyInkUpdateV2 { payload: CollaborativeInkUpdate },

    /// Stonepen update for the page ink layer (creates it when bootstrapping).
    #[serde(rename = "apply_page_ink_update")]
    ApplyPageInkUpdate { payload: PageInkUpdate },

    /// Pins (or re-pins) an ink group in the text flow.
    #[serde(rename = "set_ink_anchor")]
    SetInkAnchor {
        group: InkGroupId,
        anchor: InkAnchor,
    },

    #[serde(rename = "remove_ink_anchors")]
    RemoveInkAnchors { groups: Vec<InkGroupId> },

    /// Replaces legacy Ink blocks with Space blocks and moves their strokes
    /// into a new page ink layer, one group per block.
    #[serde(rename = "migrate_ink_blocks")]
    MigrateInkBlocks {
        stream_epoch: uuid::Uuid,
        snapshot: Vec<u8>,
        conversions: Vec<InkBlockConversion>,
    },

    #[serde(rename = "add_comment")]
    AddComment {
        comment_id: CommentId,
        comment: Comment,
    },

    #[serde(rename = "edit_comment")]
    EditComment { comment_id: CommentId, body: String },

    #[serde(rename = "resolve_comment")]
    ResolveComment {
        comment_id: CommentId,
        resolved: bool,
    },

    #[serde(rename = "delete_comment")]
    DeleteComment { comment_id: CommentId },
}

impl DocumentOp {
    /// The update envelope version this op travels in.
    pub fn update_version(&self) -> u32 {
        match self {
            DocumentOp::ApplyTextUpdateV2 { .. }
            | DocumentOp::ApplyInkUpdateV2 { .. }
            | DocumentOp::ApplyPageInkUpdate { .. }
            | DocumentOp::SetInkAnchor { .. }
            | DocumentOp::RemoveInkAnchors { .. }
            | DocumentOp::MigrateInkBlocks { .. }
            | DocumentOp::AddComment { .. }
            | DocumentOp::EditComment { .. }
            | DocumentOp::ResolveComment { .. }
            | DocumentOp::DeleteComment { .. } => DOCUMENT_UPDATE_V2_VERSION,
            _ => DOCUMENT_UPDATE_VERSION,
        }
    }
}

// ── Strict local-intent errors ──────────────────────────────────────────────

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

    #[error("ink update payload version {0} is unsupported")]
    UnsupportedInkUpdateVersion(u32),

    #[error("block {0} is not an Ink block")]
    NotInkBlock(BlockId),

    #[error("ink stream epoch does not match block {0}")]
    InkStreamEpochMismatch(BlockId),

    #[error("ink stream for block {0} requires a bootstrap snapshot")]
    InkBootstrapRequired(BlockId),

    #[error("Stonepen collaboration update failed: {0}")]
    StonepenCollaboration(String),

    #[error("document has no page ink layer")]
    InkLayerMissing,

    #[error("document already has a page ink layer")]
    InkLayerExists,

    #[error("page ink stream epoch does not match")]
    PageInkStreamEpochMismatch,

    #[error("invalid ink anchor: {0}")]
    InvalidInkAnchor(String),

    #[error("comment not found: {0}")]
    CommentNotFound(CommentId),

    #[error("invalid comment: {0}")]
    InvalidComment(&'static str),
}

pub const MAX_COMMENT_LEN: usize = 10_000;

/// Strict local-intent application.
/// Rejects any operation that cannot be applied exactly as intended.
pub fn apply_update(doc: &mut InkstoneDocument, update: DocumentUpdate) -> Result<(), ApplyError> {
    validate_update(doc, &update)?;

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
            reanchor_removed_block(doc, pos);
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
            demote_stale_text_anchors(doc, block_id);
        }

        DocumentOp::SetTitle { title } => {
            doc.title = title;
        }

        DocumentOp::ApplyTextUpdateV2 { .. } => {
            return Err(ApplyError::UnsupportedUpdateVersion(update.version));
        }
        DocumentOp::ApplyInkUpdateV2 { payload } => {
            apply_ink_update(doc, payload, update.applied_at_ms)?;
        }
        DocumentOp::ApplyPageInkUpdate { payload } => {
            apply_page_ink_update(doc, payload)?;
        }
        DocumentOp::SetInkAnchor { group, anchor } => {
            validate_anchor(doc, &anchor)?;
            doc.ink
                .as_mut()
                .ok_or(ApplyError::InkLayerMissing)?
                .anchors
                .insert(group, anchor);
        }
        DocumentOp::RemoveInkAnchors { groups } => {
            let layer = doc.ink.as_mut().ok_or(ApplyError::InkLayerMissing)?;
            for group in groups {
                layer.anchors.remove(&group);
            }
        }
        DocumentOp::MigrateInkBlocks {
            stream_epoch,
            snapshot,
            conversions,
        } => {
            if doc.ink.is_some() {
                return Err(ApplyError::InkLayerExists);
            }
            for conversion in &conversions {
                match doc.get_block(conversion.block_id).map(|b| &b.content) {
                    Some(BlockContent::Ink { .. }) => {}
                    Some(_) => return Err(ApplyError::NotInkBlock(conversion.block_id)),
                    None => return Err(ApplyError::BlockNotFound(conversion.block_id)),
                }
            }
            migrate_ink_blocks(
                doc,
                stream_epoch,
                snapshot,
                &conversions,
                update.applied_at_ms,
            )?;
        }
        op @ (DocumentOp::AddComment { .. }
        | DocumentOp::EditComment { .. }
        | DocumentOp::ResolveComment { .. }
        | DocumentOp::DeleteComment { .. }) => {
            apply_comment(doc, op, update.applied_at_ms, true)?;
        }
    }

    Ok(())
}

fn comment_mut(doc: &mut InkstoneDocument, id: CommentId) -> Result<&mut Comment, ApplyError> {
    doc.comments
        .get_mut(&id)
        .ok_or(ApplyError::CommentNotFound(id))
}

fn check_body(body: &str) -> Result<(), ApplyError> {
    match body.trim().len() {
        0 => Err(ApplyError::InvalidComment("empty comment")),
        len if len > MAX_COMMENT_LEN => Err(ApplyError::InvalidComment("comment too long")),
        _ => Ok(()),
    }
}

fn apply_comment(
    doc: &mut InkstoneDocument,
    op: DocumentOp,
    at_ms: i64,
    strict: bool,
) -> Result<(), ApplyError> {
    match op {
        DocumentOp::AddComment {
            comment_id,
            comment,
        } => {
            if doc.comments.contains_key(&comment_id) {
                return Err(ApplyError::InvalidComment("duplicate comment id"));
            }
            check_body(&comment.body)?;
            match (&comment.anchor, comment.parent) {
                (Some(anchor), None) if strict => validate_anchor(doc, anchor)?,
                (Some(anchor), None) => {
                    if !anchor.dx.is_finite() || !anchor.dy.is_finite() {
                        return Err(ApplyError::InvalidInkAnchor("non-finite offset".into()));
                    }
                }
                (None, Some(parent)) => match doc.comments.get(&parent) {
                    Some(thread) if thread.parent.is_none() => {}
                    Some(_) => return Err(ApplyError::InvalidComment("replies cannot nest")),
                    None => return Err(ApplyError::CommentNotFound(parent)),
                },
                _ => {
                    return Err(ApplyError::InvalidComment(
                        "a comment needs an anchor or a parent",
                    ));
                }
            }
            doc.comments.insert(comment_id, comment);
        }
        DocumentOp::EditComment { comment_id, body } => {
            check_body(&body)?;
            let comment = comment_mut(doc, comment_id)?;
            comment.body = body;
            comment.edited_at_ms = Some(at_ms);
        }
        DocumentOp::ResolveComment {
            comment_id,
            resolved,
        } => {
            let comment = comment_mut(doc, comment_id)?;
            if comment.parent.is_some() {
                return Err(ApplyError::InvalidComment("only threads can be resolved"));
            }
            comment.resolved = resolved;
        }
        DocumentOp::DeleteComment { comment_id } => {
            if doc.comments.remove(&comment_id).is_none() && strict {
                return Err(ApplyError::CommentNotFound(comment_id));
            }
            doc.comments
                .retain(|_, comment| comment.parent != Some(comment_id));
        }
        _ => unreachable!("not a comment op"),
    }
    Ok(())
}

/// Before removing the block at `pos`, moves every anchor inside it to the
/// next block (or the previous one, or the page). Offsets are kept.
fn reanchor_removed_block(doc: &mut InkstoneDocument, pos: usize) {
    let removed = doc.blocks[pos].id;
    let fallback = doc
        .blocks
        .get(pos + 1)
        .or_else(|| pos.checked_sub(1).and_then(|prev| doc.blocks.get(prev)))
        .map(|block| AnchorTarget::Block { block_id: block.id })
        .unwrap_or(AnchorTarget::Page);
    for anchor in anchors_mut(doc) {
        if anchor.target.block_id() == Some(removed) {
            anchor.target = fallback.clone();
        }
    }
}

fn anchors_mut(doc: &mut InkstoneDocument) -> impl Iterator<Item = &mut InkAnchor> {
    doc.ink
        .iter_mut()
        .flat_map(|layer| layer.anchors.values_mut())
        .chain(
            doc.comments
                .values_mut()
                .filter_map(|comment| comment.anchor.as_mut()),
        )
}

/// Text anchors into a block that is no longer Markdown fall back to the
/// block itself.
fn demote_stale_text_anchors(doc: &mut InkstoneDocument, block_id: BlockId) {
    let is_markdown = matches!(
        doc.get_block(block_id).map(|b| &b.content),
        Some(BlockContent::Markdown { .. })
    );
    if is_markdown {
        return;
    }
    for anchor in anchors_mut(doc) {
        if matches!(&anchor.target, AnchorTarget::Text { .. })
            && anchor.target.block_id() == Some(block_id)
        {
            anchor.target = AnchorTarget::Block { block_id };
        }
    }
}

fn validate_anchor(doc: &InkstoneDocument, anchor: &InkAnchor) -> Result<(), ApplyError> {
    if !anchor.dx.is_finite() || !anchor.dy.is_finite() {
        return Err(ApplyError::InvalidInkAnchor("non-finite offset".into()));
    }
    match &anchor.target {
        AnchorTarget::Page => Ok(()),
        AnchorTarget::Block { block_id } => doc
            .get_block(*block_id)
            .map(|_| ())
            .ok_or(ApplyError::BlockNotFound(*block_id)),
        AnchorTarget::Text { position } => match position.target {
            TextTarget::Markdown(block_id) => match doc.get_block(block_id).map(|b| &b.content) {
                Some(BlockContent::Markdown { .. }) => Ok(()),
                Some(_) => Err(ApplyError::InvalidInkAnchor(format!(
                    "block {block_id} is not Markdown"
                ))),
                None => Err(ApplyError::BlockNotFound(block_id)),
            },
            _ => Err(ApplyError::InvalidInkAnchor(
                "text anchors must target a Markdown block".into(),
            )),
        },
    }
}

fn apply_page_ink_update(
    doc: &mut InkstoneDocument,
    payload: PageInkUpdate,
) -> Result<(), ApplyError> {
    if payload.version != INK_UPDATE_PAYLOAD_VERSION {
        return Err(ApplyError::UnsupportedInkUpdateVersion(payload.version));
    }
    match doc.ink.as_mut() {
        Some(layer) if layer.collaboration.stream_epoch != payload.stream_epoch => {
            Err(ApplyError::PageInkStreamEpochMismatch)
        }
        Some(layer) => layer
            .collaboration
            .snapshot
            .apply(&payload.update)
            .map_err(ApplyError::StonepenCollaboration),
        None => {
            let mut snapshot = crate::InkSnapshot::from_bytes(
                payload
                    .bootstrap_snapshot
                    .ok_or(ApplyError::InkLayerMissing)?,
            );
            snapshot
                .apply(&payload.update)
                .map_err(ApplyError::StonepenCollaboration)?;
            doc.ink = Some(PageInkLayer {
                version: PAGE_INK_LAYER_VERSION,
                collaboration: InkCollaborationState {
                    version: INK_COLLABORATION_VERSION,
                    stream_epoch: payload.stream_epoch,
                    snapshot,
                },
                anchors: Default::default(),
            });
            Ok(())
        }
    }
}

/// Converts the listed blocks that are still Ink. Callers decide how strict
/// to be about blocks that are not.
fn migrate_ink_blocks(
    doc: &mut InkstoneDocument,
    stream_epoch: uuid::Uuid,
    snapshot: Vec<u8>,
    conversions: &[InkBlockConversion],
    applied_at_ms: i64,
) -> Result<usize, ApplyError> {
    let snapshot = crate::InkSnapshot::from_bytes(snapshot);
    snapshot.doc().map_err(ApplyError::StonepenCollaboration)?;
    let mut anchors = std::collections::BTreeMap::new();
    for conversion in conversions {
        let Some(block) = doc.get_block_mut(conversion.block_id) else {
            continue;
        };
        if !matches!(block.content, BlockContent::Ink { .. }) {
            continue;
        }
        block.content = BlockContent::Space {
            min_height: conversion.min_height,
        };
        block.updated_at_ms = applied_at_ms;
        anchors.insert(
            conversion.group,
            InkAnchor {
                target: AnchorTarget::Block {
                    block_id: conversion.block_id,
                },
                dx: 0.0,
                dy: 0.0,
            },
        );
    }
    let converted = anchors.len();
    doc.ink = Some(PageInkLayer {
        version: PAGE_INK_LAYER_VERSION,
        collaboration: InkCollaborationState {
            version: INK_COLLABORATION_VERSION,
            stream_epoch,
            snapshot,
        },
        anchors,
    });
    Ok(converted)
}

fn validate_update(doc: &InkstoneDocument, update: &DocumentUpdate) -> Result<(), ApplyError> {
    if doc.schema_version != crate::document::DOCUMENT_SCHEMA_VERSION {
        return Err(ApplyError::UnsupportedDocumentSchemaVersion(
            doc.schema_version,
        ));
    }
    let expected_version = update.op.update_version();
    if update.version != expected_version {
        return Err(ApplyError::UnsupportedUpdateVersion(update.version));
    }
    if update.doc_id != doc.id {
        return Err(ApplyError::WrongDocumentId);
    }
    Ok(())
}

fn apply_ink_update(
    doc: &mut InkstoneDocument,
    payload: CollaborativeInkUpdate,
    applied_at_ms: i64,
) -> Result<(), ApplyError> {
    if payload.version != INK_UPDATE_PAYLOAD_VERSION {
        return Err(ApplyError::UnsupportedInkUpdateVersion(payload.version));
    }
    let block = doc
        .get_block_mut(payload.block_id)
        .ok_or(ApplyError::BlockNotFound(payload.block_id))?;
    let BlockContent::Ink {
        stonepen_json,
        collaboration,
    } = &mut block.content
    else {
        return Err(ApplyError::NotInkBlock(payload.block_id));
    };

    let snapshot = match collaboration.as_ref() {
        Some(state) if state.version != INK_COLLABORATION_VERSION => {
            return Err(ApplyError::UnsupportedInkUpdateVersion(state.version));
        }
        Some(state) if state.stream_epoch != payload.stream_epoch => {
            return Err(ApplyError::InkStreamEpochMismatch(payload.block_id));
        }
        Some(state) => state.snapshot.clone(),
        None => crate::InkSnapshot::from_bytes(
            payload
                .bootstrap_snapshot
                .ok_or(ApplyError::InkBootstrapRequired(payload.block_id))?,
        ),
    };
    let mut snapshot = snapshot;
    snapshot
        .apply(&payload.update)
        .map_err(ApplyError::StonepenCollaboration)?;
    *stonepen_json = snapshot
        .projection()
        .map_err(ApplyError::StonepenCollaboration)?
        .to_owned();
    *collaboration = Some(InkCollaborationState {
        version: INK_COLLABORATION_VERSION,
        stream_epoch: payload.stream_epoch,
        snapshot,
    });
    block.updated_at_ms = applied_at_ms;
    Ok(())
}

// ── Sequenced replay outcomes ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayOutcome {
    Applied,
    Adjusted { reason: String },
    NoOp { conflict: String },
    Fatal { reason: String },
}

/// Deterministic sequenced replay.
/// Every valid-version server-sequenced update must produce a deterministic
/// outcome.  Never rejects an operation that passes schema/version/doc_id
/// validation — instead it produces an explicit outcome.
pub fn sequenced_replay(doc: &mut InkstoneDocument, update: DocumentUpdate) -> ReplayOutcome {
    match validate_update(doc, &update) {
        Err(ApplyError::WrongDocumentId) => {
            return ReplayOutcome::Fatal {
                reason: "update doc_id does not match document id".into(),
            };
        }
        Err(ApplyError::UnsupportedDocumentSchemaVersion(v)) => {
            return ReplayOutcome::Fatal {
                reason: format!("unsupported document schema version {}", v),
            };
        }
        Err(ApplyError::UnsupportedUpdateVersion(v)) => {
            return ReplayOutcome::Fatal {
                reason: format!("unsupported update version {}", v),
            };
        }
        Err(_) => {
            return ReplayOutcome::Fatal {
                reason: "protocol invariant violation during replay".into(),
            };
        }
        Ok(()) => {}
    }

    match update.op {
        DocumentOp::InsertBlock { idx, block } => {
            if doc.block_index(block.id).is_some() {
                return ReplayOutcome::NoOp {
                    conflict: format!("duplicate block id {}", block.id),
                };
            }
            let clamped_idx = idx.min(doc.blocks.len());
            let adjusted = clamped_idx != idx;
            doc.blocks.insert(clamped_idx, block);
            if adjusted {
                ReplayOutcome::Adjusted {
                    reason: format!("insert index {} clamped to {}", idx, clamped_idx),
                }
            } else {
                ReplayOutcome::Applied
            }
        }

        DocumentOp::DeleteBlock { block_id } => {
            let pos = match doc.block_index(block_id) {
                Some(p) => p,
                None => {
                    return ReplayOutcome::NoOp {
                        conflict: format!("delete target block {} not found", block_id),
                    };
                }
            };
            reanchor_removed_block(doc, pos);
            doc.blocks.remove(pos);
            ReplayOutcome::Applied
        }

        DocumentOp::MoveBlock { block_id, new_idx } => {
            let pos = match doc.block_index(block_id) {
                Some(p) => p,
                None => {
                    return ReplayOutcome::NoOp {
                        conflict: format!("move target block {} not found", block_id),
                    };
                }
            };
            let len = doc.blocks.len();
            let clamped_idx = if new_idx >= len { len - 1 } else { new_idx };
            let adjusted = clamped_idx != new_idx;
            let block = doc.blocks.remove(pos);
            doc.blocks.insert(clamped_idx, block);
            if adjusted {
                ReplayOutcome::Adjusted {
                    reason: format!("move index {} clamped to {}", new_idx, clamped_idx),
                }
            } else {
                ReplayOutcome::Applied
            }
        }

        DocumentOp::ReplaceBlockContent { block_id, content } => {
            match doc.get_block_mut(block_id) {
                Some(block) => {
                    block.content = content;
                    block.updated_at_ms = update.applied_at_ms;
                    demote_stale_text_anchors(doc, block_id);
                    ReplayOutcome::Applied
                }
                None => ReplayOutcome::NoOp {
                    conflict: format!("replace target block {} not found", block_id),
                },
            }
        }

        DocumentOp::SetTitle { title } => {
            doc.title = title;
            ReplayOutcome::Applied
        }
        DocumentOp::ApplyTextUpdateV2 { .. } => ReplayOutcome::Fatal {
            reason: "text v2 updates require CollaborativeTextState".into(),
        },
        DocumentOp::ApplyInkUpdateV2 { payload } => {
            match apply_ink_update(doc, payload, update.applied_at_ms) {
                Ok(()) => ReplayOutcome::Applied,
                Err(ApplyError::BlockNotFound(block_id)) => ReplayOutcome::NoOp {
                    conflict: format!("ink target block {} not found", block_id),
                },
                Err(ApplyError::NotInkBlock(block_id)) => ReplayOutcome::NoOp {
                    conflict: format!("ink target block {} has another type", block_id),
                },
                Err(ApplyError::InkStreamEpochMismatch(block_id)) => ReplayOutcome::NoOp {
                    conflict: format!("ink stream epoch for block {} is stale", block_id),
                },
                Err(error) => ReplayOutcome::Fatal {
                    reason: error.to_string(),
                },
            }
        }
        DocumentOp::ApplyPageInkUpdate { payload } => match apply_page_ink_update(doc, payload) {
            Ok(()) => ReplayOutcome::Applied,
            Err(ApplyError::PageInkStreamEpochMismatch) => ReplayOutcome::NoOp {
                conflict: "page ink stream epoch is stale".into(),
            },
            Err(ApplyError::InkLayerMissing) => ReplayOutcome::NoOp {
                conflict: "page ink update without a layer or bootstrap".into(),
            },
            Err(error) => ReplayOutcome::Fatal {
                reason: error.to_string(),
            },
        },
        DocumentOp::SetInkAnchor { group, anchor } => {
            if !anchor.dx.is_finite() || !anchor.dy.is_finite() {
                return ReplayOutcome::NoOp {
                    conflict: "non-finite ink anchor offset".into(),
                };
            }
            let Some(layer) = doc.ink.as_mut() else {
                return ReplayOutcome::NoOp {
                    conflict: "ink anchor without a page ink layer".into(),
                };
            };
            // A concurrently deleted target is kept; layout falls back to
            // the page origin for targets it cannot find.
            layer.anchors.insert(group, anchor);
            ReplayOutcome::Applied
        }
        DocumentOp::RemoveInkAnchors { groups } => {
            if let Some(layer) = doc.ink.as_mut() {
                for group in groups {
                    layer.anchors.remove(&group);
                }
            }
            ReplayOutcome::Applied
        }
        DocumentOp::MigrateInkBlocks {
            stream_epoch,
            snapshot,
            conversions,
        } => {
            if doc.ink.is_some() {
                return ReplayOutcome::NoOp {
                    conflict: "page ink layer already exists".into(),
                };
            }
            match migrate_ink_blocks(
                doc,
                stream_epoch,
                snapshot,
                &conversions,
                update.applied_at_ms,
            ) {
                Ok(converted) if converted == conversions.len() => ReplayOutcome::Applied,
                Ok(converted) => ReplayOutcome::Adjusted {
                    reason: format!("migrated {converted} of {} Ink blocks", conversions.len()),
                },
                Err(error) => ReplayOutcome::Fatal {
                    reason: error.to_string(),
                },
            }
        }
        op @ (DocumentOp::AddComment { .. }
        | DocumentOp::EditComment { .. }
        | DocumentOp::ResolveComment { .. }
        | DocumentOp::DeleteComment { .. }) => {
            match apply_comment(doc, op, update.applied_at_ms, false) {
                Ok(()) => ReplayOutcome::Applied,
                Err(error) => ReplayOutcome::NoOp {
                    conflict: error.to_string(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::BlockContent;
    use crate::ids::UpdateId;

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
            update_id: UpdateId::new_v4(),
            doc_id,
            applied_at_ms,
            op,
        }
    }

    fn legacy_ink_block(block_id: BlockId) -> (DocumentBlock, Vec<u8>) {
        let (snapshot, stonepen_json) = stonepen_core::new_opaque_document(640.0, 480.0).unwrap();
        (
            DocumentBlock::new(
                block_id,
                BlockContent::Ink {
                    stonepen_json,
                    collaboration: None,
                },
            ),
            snapshot,
        )
    }

    fn ink_update(
        doc_id: DocId,
        block_id: BlockId,
        stream_epoch: uuid::Uuid,
        bootstrap_snapshot: Option<Vec<u8>>,
    ) -> DocumentUpdate {
        DocumentUpdate {
            version: DOCUMENT_UPDATE_V2_VERSION,
            update_id: UpdateId::new_v4(),
            doc_id,
            applied_at_ms: 4_000,
            op: DocumentOp::ApplyInkUpdateV2 {
                payload: CollaborativeInkUpdate {
                    version: INK_UPDATE_PAYLOAD_VERSION,
                    block_id,
                    stream_epoch,
                    update: Vec::new(),
                    bootstrap_snapshot,
                },
            },
        }
    }

    #[test]
    fn legacy_ink_migrates_to_an_opaque_snapshot_and_projection() {
        let (mut doc, doc_id) = make_doc();
        let block_id = BlockId::new_v4();
        let (block, snapshot) = legacy_ink_block(block_id);
        doc.blocks.push(block);
        let epoch = uuid::Uuid::new_v4();

        apply_update(
            &mut doc,
            ink_update(doc_id, block_id, epoch, Some(snapshot)),
        )
        .unwrap();

        let BlockContent::Ink {
            stonepen_json,
            collaboration: Some(state),
        } = &doc.blocks[0].content
        else {
            panic!("expected migrated Ink block");
        };
        assert_eq!(state.stream_epoch, epoch);
        assert!(!state.snapshot.is_empty());
        assert_eq!(
            stonepen_core::project_opaque_snapshot(state.snapshot.bytes().unwrap()).unwrap(),
            *stonepen_json
        );
    }

    #[test]
    fn duplicate_ink_updates_are_idempotent() {
        let (mut doc, doc_id) = make_doc();
        let block_id = BlockId::new_v4();
        let (block, snapshot) = legacy_ink_block(block_id);
        doc.blocks.push(block);
        let epoch = uuid::Uuid::new_v4();
        apply_update(
            &mut doc,
            ink_update(doc_id, block_id, epoch, Some(snapshot)),
        )
        .unwrap();
        let update = ink_update(doc_id, block_id, epoch, None);
        apply_update(&mut doc, update.clone()).unwrap();
        let once = serde_json::to_vec(&doc).unwrap();
        apply_update(&mut doc, update).unwrap();
        assert_eq!(serde_json::to_vec(&doc).unwrap(), once);
    }

    #[test]
    fn deleted_and_recreated_block_rejects_the_old_stream_epoch() {
        let (mut doc, doc_id) = make_doc();
        let block_id = BlockId::new_v4();
        let (block, snapshot) = legacy_ink_block(block_id);
        doc.blocks.push(block);
        let old_epoch = uuid::Uuid::new_v4();
        apply_update(
            &mut doc,
            ink_update(doc_id, block_id, old_epoch, Some(snapshot.clone())),
        )
        .unwrap();
        doc.blocks.clear();
        let (replacement, _) = legacy_ink_block(block_id);
        doc.blocks.push(replacement);
        let new_epoch = uuid::Uuid::new_v4();
        apply_update(
            &mut doc,
            ink_update(doc_id, block_id, new_epoch, Some(snapshot)),
        )
        .unwrap();

        assert!(matches!(
            sequenced_replay(&mut doc, ink_update(doc_id, block_id, old_epoch, None)),
            ReplayOutcome::NoOp { .. }
        ));
        let BlockContent::Ink {
            collaboration: Some(state),
            ..
        } = &doc.blocks[0].content
        else {
            panic!("expected collaborative Ink block");
        };
        assert_eq!(state.stream_epoch, new_epoch);
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
                collaboration: None,
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
                        collaboration: None,
                    },
                },
            ),
        )
        .unwrap();
        match &doc.blocks[0].content {
            BlockContent::Ink { stonepen_json, .. } => {
                assert_eq!(stonepen_json, "{\"strokes\":[]}");
                assert!(stonepen_json.contains("strokes"));
            }
            _ => panic!("wrong content"),
        }
    }

    #[test]
    fn test_insert_all_block_types_preserves_order() {
        let (mut doc, doc_id) = make_doc();
        let contents = vec![
            markdown("markdown"),
            BlockContent::Math {
                source: "x+y".into(),
            },
            BlockContent::Ink {
                stonepen_json: "opaque".into(),
                collaboration: None,
            },
            BlockContent::Divider,
        ];
        for (idx, content) in contents.into_iter().enumerate() {
            apply_update(
                &mut doc,
                upd(
                    doc_id,
                    1_000 + idx as i64,
                    DocumentOp::InsertBlock {
                        idx,
                        block: DocumentBlock::new(BlockId::new_v4(), content),
                    },
                ),
            )
            .unwrap();
        }

        assert!(matches!(
            doc.blocks[0].content,
            BlockContent::Markdown { .. }
        ));
        assert!(matches!(doc.blocks[1].content, BlockContent::Math { .. }));
        assert!(matches!(doc.blocks[2].content, BlockContent::Ink { .. }));
        assert!(matches!(doc.blocks[3].content, BlockContent::Divider));
    }

    #[test]
    fn test_replace_math_source() {
        let (mut doc, doc_id) = make_doc();
        let block =
            DocumentBlock::new(BlockId::new_v4(), BlockContent::Math { source: "x".into() });
        let block_id = block.id;
        doc.blocks.push(block);
        apply_update(
            &mut doc,
            upd(
                doc_id,
                2_000,
                DocumentOp::ReplaceBlockContent {
                    block_id,
                    content: BlockContent::Math {
                        source: "x^2 + y^2".into(),
                    },
                },
            ),
        )
        .unwrap();

        assert!(matches!(
            &doc.blocks[0].content,
            BlockContent::Math { source } if source == "x^2 + y^2"
        ));
        assert_eq!(doc.blocks[0].updated_at_ms, 2_000);
    }

    #[test]
    fn test_replace_missing_block_is_rejected() {
        let (mut doc, doc_id) = make_doc();
        let missing = BlockId::new_v4();
        let error = apply_update(
            &mut doc,
            upd(
                doc_id,
                2_000,
                DocumentOp::ReplaceBlockContent {
                    block_id: missing,
                    content: BlockContent::Divider,
                },
            ),
        )
        .unwrap_err();
        assert_eq!(error, ApplyError::BlockNotFound(missing));
    }

    #[test]
    fn test_wrong_document_id_rejected() {
        let (mut doc, _) = make_doc();
        let err = apply_update(
            &mut doc,
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
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
            update_id: UpdateId::new_v4(),
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
            update_id: UpdateId::new_v4(),
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
            update_id: UpdateId::new_v4(),
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
                update_id: UpdateId::new_v4(),
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
                update_id: UpdateId::new_v4(),
                doc_id,
                applied_at_ms: 1000,
                op: DocumentOp::SetTitle { title: "x".into() },
            },
        )
        .unwrap_err();
        assert_eq!(err, ApplyError::UnsupportedUpdateVersion(99));
    }

    #[test]
    fn test_v2_text_payload_is_explicit_and_not_legacy_replay() {
        use crate::text_crdt::{CollaborativeTextUpdate, TextTarget};

        let (mut doc, doc_id) = make_doc();
        let update = DocumentUpdate {
            version: DOCUMENT_UPDATE_V2_VERSION,
            update_id: UpdateId::new_v4(),
            doc_id,
            applied_at_ms: 1000,
            op: DocumentOp::ApplyTextUpdateV2 {
                payload: CollaborativeTextUpdate {
                    version: 2,
                    target: TextTarget::Title,
                    update_v1: vec![0, 0],
                },
            },
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["op"]["type"], "apply_text_update_v2");
        assert_eq!(
            apply_update(&mut doc, update).unwrap_err(),
            ApplyError::UnsupportedUpdateVersion(2)
        );
    }

    // ── Sequenced replay tests ───────────────────────────────────────────────

    fn replay_nop(doc: &mut InkstoneDocument, doc_id: DocId, op: DocumentOp) -> ReplayOutcome {
        sequenced_replay(doc, upd(doc_id, 1000, op))
    }

    #[test]
    fn test_sequenced_delete_missing_block_is_nop() {
        let (mut doc, doc_id) = make_doc();
        let outcome = replay_nop(
            &mut doc,
            doc_id,
            DocumentOp::DeleteBlock {
                block_id: BlockId::new_v4(),
            },
        );
        assert!(matches!(outcome, ReplayOutcome::NoOp { .. }));
    }

    #[test]
    fn test_sequenced_replace_missing_block_is_nop() {
        let (mut doc, doc_id) = make_doc();
        let outcome = replay_nop(
            &mut doc,
            doc_id,
            DocumentOp::ReplaceBlockContent {
                block_id: BlockId::new_v4(),
                content: markdown("x"),
            },
        );
        assert!(matches!(outcome, ReplayOutcome::NoOp { .. }));
    }

    #[test]
    fn test_sequenced_move_missing_block_is_nop() {
        let (mut doc, doc_id) = make_doc();
        let outcome = replay_nop(
            &mut doc,
            doc_id,
            DocumentOp::MoveBlock {
                block_id: BlockId::new_v4(),
                new_idx: 0,
            },
        );
        assert!(matches!(outcome, ReplayOutcome::NoOp { .. }));
    }

    #[test]
    fn test_sequenced_duplicate_block_id_is_nop() {
        let (mut doc, doc_id) = make_doc();
        let id = BlockId::new_v4();
        doc.blocks
            .push(DocumentBlock::new(id, markdown("existing")));
        let outcome = replay_nop(
            &mut doc,
            doc_id,
            DocumentOp::InsertBlock {
                idx: 0,
                block: DocumentBlock::new(id, markdown("dup")),
            },
        );
        assert!(matches!(outcome, ReplayOutcome::NoOp { .. }));
    }

    #[test]
    fn test_sequenced_out_of_range_insert_clamps() {
        let (mut doc, doc_id) = make_doc();
        doc.blocks
            .push(DocumentBlock::new(BlockId::new_v4(), markdown("a")));
        let outcome = replay_nop(
            &mut doc,
            doc_id,
            DocumentOp::InsertBlock {
                idx: 100,
                block: DocumentBlock::new(BlockId::new_v4(), markdown("late")),
            },
        );
        assert!(matches!(outcome, ReplayOutcome::Adjusted { .. }));
        assert_eq!(doc.blocks.len(), 2);
    }

    #[test]
    fn test_sequenced_wrong_document_id_is_fatal() {
        let (mut doc, _) = make_doc();
        let outcome = sequenced_replay(
            &mut doc,
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
                doc_id: DocId::new_v4(),
                applied_at_ms: 1000,
                op: DocumentOp::SetTitle {
                    title: "test".into(),
                },
            },
        );
        assert!(matches!(outcome, ReplayOutcome::Fatal { .. }));
    }

    #[test]
    fn test_sequenced_set_title_is_applied() {
        let (mut doc, doc_id) = make_doc();
        let outcome = sequenced_replay(
            &mut doc,
            upd(
                doc_id,
                1000,
                DocumentOp::SetTitle {
                    title: "hello".into(),
                },
            ),
        );
        assert_eq!(outcome, ReplayOutcome::Applied);
        assert_eq!(doc.title, "hello");
    }

    #[test]
    fn test_same_ordered_log_identical_final_document() {
        let id = DocId::new_v4();
        let b_a = DocumentBlock::new(BlockId::new_v4(), markdown("a"));
        let b_b = DocumentBlock::new(BlockId::new_v4(), markdown("b"));
        let b_c = DocumentBlock::new(BlockId::new_v4(), markdown("c"));
        let b_a_id = b_a.id;
        let b_c_id = b_c.id;

        let updates = vec![
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
                doc_id: id,
                applied_at_ms: 1000,
                op: DocumentOp::InsertBlock { idx: 0, block: b_a },
            },
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
                doc_id: id,
                applied_at_ms: 1001,
                op: DocumentOp::InsertBlock { idx: 1, block: b_b },
            },
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
                doc_id: id,
                applied_at_ms: 1002,
                op: DocumentOp::InsertBlock { idx: 0, block: b_c },
            },
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
                doc_id: id,
                applied_at_ms: 1003,
                op: DocumentOp::ReplaceBlockContent {
                    block_id: b_a_id,
                    content: markdown("A"),
                },
            },
            DocumentUpdate {
                version: DOCUMENT_UPDATE_VERSION,
                update_id: UpdateId::new_v4(),
                doc_id: id,
                applied_at_ms: 1004,
                op: DocumentOp::DeleteBlock { block_id: b_c_id },
            },
        ];

        // Two docs replaying the same ordered log
        let mut doc_a = InkstoneDocument::new(id);
        let mut doc_b = InkstoneDocument::new(id);
        for u in &updates {
            assert_eq!(
                sequenced_replay(&mut doc_a, u.clone()),
                ReplayOutcome::Applied
            );
        }
        for u in &updates {
            assert_eq!(
                sequenced_replay(&mut doc_b, u.clone()),
                ReplayOutcome::Applied
            );
        }

        assert_eq!(
            serde_json::to_string(&doc_a).unwrap(),
            serde_json::to_string(&doc_b).unwrap()
        );
        assert_eq!(doc_a.blocks.len(), 2);
        // After delete of c, blocks: [c, a, b] -> delete c -> [a, b], then a updated to "A"
        assert_eq!(doc_a.blocks[0].id, b_a_id);
    }
}

#[cfg(test)]
mod page_ink_tests {
    use super::*;
    use crate::document::{DEFAULT_SPACE_HEIGHT, new_page_ink_layer};
    use crate::markdown::materialize_document_markdown;
    use crate::page_ink::plan_ink_block_migration;
    use crate::text_crdt::{CollaborativeTextState, TextAssociation, TextReplicaId};
    use stonepen_core::{
        ActorId, Brush, CollaborationDoc, InkPoint, InkStroke, ItemId, PointerKind,
    };

    fn update(doc_id: DocId, op: DocumentOp) -> DocumentUpdate {
        DocumentUpdate {
            version: op.update_version(),
            update_id: UpdateId::new_v4(),
            doc_id,
            applied_at_ms: 1_000,
            op,
        }
    }

    fn apply(doc: &mut InkstoneDocument, op: DocumentOp) -> Result<(), ApplyError> {
        let update = update(doc.id, op);
        apply_update(doc, update)
    }

    fn replay(doc: &mut InkstoneDocument, op: DocumentOp) -> ReplayOutcome {
        let update = update(doc.id, op);
        sequenced_replay(doc, update)
    }

    fn markdown_block(text: &str) -> DocumentBlock {
        DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown { text: text.into() },
        )
    }

    fn framed_stroke(frame: InkGroupId, y: f32) -> InkStroke {
        let brush = Brush::default_pen();
        let pts = [0.0_f32, 30.0]
            .into_iter()
            .map(|x| InkPoint {
                x,
                y,
                t_ms: 0.0,
                press: 1.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
                twist: 0.0,
                pointer_type: PointerKind::Pen,
            })
            .collect::<Vec<_>>();
        let bbox = stonepen_core::compute_conservative_stroke_bbox(&pts, &brush).unwrap();
        InkStroke {
            id: ItemId::new(),
            parent_id: Some(ItemId(frame.0)),
            brush,
            raw_pts: pts.clone(),
            pts,
            local_bbox: bbox,
            world_bbox: bbox,
            xform: stonepen_core::Xform2D::identity(),
            created_at_ms: 0,
            updated_at_ms: 0,
            geom_rev: 0,
        }
    }

    /// A doc with a page layer created through a bootstrap update.
    fn doc_with_layer(blocks: Vec<DocumentBlock>) -> (InkstoneDocument, uuid::Uuid) {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks = blocks;
        let epoch = uuid::Uuid::new_v4();
        let layer = new_page_ink_layer(epoch).unwrap();
        apply(
            &mut doc,
            DocumentOp::ApplyPageInkUpdate {
                payload: PageInkUpdate {
                    version: INK_UPDATE_PAYLOAD_VERSION,
                    stream_epoch: epoch,
                    update: Vec::new(),
                    bootstrap_snapshot: Some(
                        layer.collaboration.snapshot.bytes().unwrap().to_vec(),
                    ),
                },
            },
        )
        .unwrap();
        (doc, epoch)
    }

    fn block_anchor(block_id: BlockId) -> InkAnchor {
        InkAnchor {
            target: AnchorTarget::Block { block_id },
            dx: 12.0,
            dy: 34.0,
        }
    }

    #[test]
    fn page_ink_update_bootstraps_and_applies_framed_strokes() {
        let (mut doc, epoch) = doc_with_layer(vec![markdown_block("hello")]);
        let layer = doc.ink.as_ref().unwrap();
        let mut replica = CollaborationDoc::from_snapshot(
            ActorId::from_bytes([7; 16]),
            layer.collaboration.snapshot.bytes().unwrap(),
        )
        .unwrap();
        let group = InkGroupId::new_v4();
        let layer_id = replica.document().active_layer_id;
        replica
            .add_stroke(layer_id, None, framed_stroke(group, 5.0))
            .unwrap();
        let bytes = replica.take_local_update().unwrap();

        apply(
            &mut doc,
            DocumentOp::ApplyPageInkUpdate {
                payload: PageInkUpdate {
                    version: INK_UPDATE_PAYLOAD_VERSION,
                    stream_epoch: epoch,
                    update: bytes,
                    bootstrap_snapshot: None,
                },
            },
        )
        .unwrap();

        let projected = CollaborationDoc::from_snapshot(
            ActorId::from_bytes([8; 16]),
            doc.ink
                .as_ref()
                .unwrap()
                .collaboration
                .snapshot
                .bytes()
                .unwrap(),
        )
        .unwrap();
        let bounds = projected.document().frame_bounds();
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0].0.0, group.0);
    }

    #[test]
    fn two_devices_starting_page_ink_at_once_both_keep_their_strokes() {
        let doc_id = DocId::new_v4();
        let first_stroke = |actor: u8, y: f32| {
            let layer = crate::document::first_page_ink_layer(doc_id).unwrap();
            let mut replica = CollaborationDoc::from_snapshot(
                ActorId::from_bytes([actor; 16]),
                layer.collaboration.snapshot.bytes().unwrap(),
            )
            .unwrap();
            let layer_id = replica.document().active_layer_id;
            replica
                .add_stroke(layer_id, None, framed_stroke(InkGroupId::new_v4(), y))
                .unwrap();
            DocumentOp::ApplyPageInkUpdate {
                payload: PageInkUpdate {
                    version: INK_UPDATE_PAYLOAD_VERSION,
                    stream_epoch: layer.collaboration.stream_epoch,
                    update: replica.take_local_update().unwrap(),
                    bootstrap_snapshot: Some(
                        layer.collaboration.snapshot.bytes().unwrap().to_vec(),
                    ),
                },
            }
        };
        let laptop = first_stroke(1, 5.0);
        let phone = first_stroke(2, 50.0);
        let mut doc = InkstoneDocument::new(doc_id);
        assert!(matches!(replay(&mut doc, laptop), ReplayOutcome::Applied));
        assert!(matches!(replay(&mut doc, phone), ReplayOutcome::Applied));
        let projected = CollaborationDoc::from_snapshot(
            ActorId::from_bytes([9; 16]),
            doc.ink
                .as_ref()
                .unwrap()
                .collaboration
                .snapshot
                .bytes()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(projected.document().frame_bounds().len(), 2);
    }

    #[test]
    fn page_ink_update_requires_layer_or_bootstrap_and_matching_epoch() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let payload = PageInkUpdate {
            version: INK_UPDATE_PAYLOAD_VERSION,
            stream_epoch: uuid::Uuid::new_v4(),
            update: Vec::new(),
            bootstrap_snapshot: None,
        };
        let op = DocumentOp::ApplyPageInkUpdate {
            payload: payload.clone(),
        };
        assert!(matches!(
            apply(&mut doc, op.clone()),
            Err(ApplyError::InkLayerMissing)
        ));
        assert!(matches!(replay(&mut doc, op), ReplayOutcome::NoOp { .. }));

        let (mut doc, _) = doc_with_layer(Vec::new());
        let stale = DocumentOp::ApplyPageInkUpdate { payload };
        assert!(matches!(
            apply(&mut doc, stale.clone()),
            Err(ApplyError::PageInkStreamEpochMismatch)
        ));
        assert!(matches!(
            replay(&mut doc, stale),
            ReplayOutcome::NoOp { .. }
        ));
    }

    #[test]
    fn anchors_are_validated_set_and_removed() {
        let block = markdown_block("text");
        let block_id = block.id;
        let (mut doc, _) = doc_with_layer(vec![block]);
        let group = InkGroupId::new_v4();

        assert!(matches!(
            apply(
                &mut doc,
                DocumentOp::SetInkAnchor {
                    group,
                    anchor: block_anchor(BlockId::new_v4()),
                },
            ),
            Err(ApplyError::BlockNotFound(_))
        ));
        let nan = InkAnchor {
            dx: f32::NAN,
            ..block_anchor(block_id)
        };
        assert!(matches!(
            apply(&mut doc, DocumentOp::SetInkAnchor { group, anchor: nan },),
            Err(ApplyError::InvalidInkAnchor(_))
        ));

        apply(
            &mut doc,
            DocumentOp::SetInkAnchor {
                group,
                anchor: block_anchor(block_id),
            },
        )
        .unwrap();
        assert_eq!(
            doc.ink.as_ref().unwrap().anchors.get(&group),
            Some(&block_anchor(block_id))
        );

        apply(
            &mut doc,
            DocumentOp::RemoveInkAnchors {
                groups: vec![group],
            },
        )
        .unwrap();
        assert!(doc.ink.as_ref().unwrap().anchors.is_empty());
    }

    #[test]
    fn anchor_without_layer_is_rejected_locally_and_ignored_in_replay() {
        let block = markdown_block("text");
        let block_id = block.id;
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(block);
        let op = DocumentOp::SetInkAnchor {
            group: InkGroupId::new_v4(),
            anchor: block_anchor(block_id),
        };
        assert!(matches!(
            apply(&mut doc, op.clone()),
            Err(ApplyError::InkLayerMissing)
        ));
        assert!(matches!(replay(&mut doc, op), ReplayOutcome::NoOp { .. }));
    }

    #[test]
    fn text_anchor_follows_concurrent_prefix_insert() {
        let block = markdown_block("first paragraph\n\nsecond paragraph");
        let block_id = block.id;
        let (mut doc, _) = doc_with_layer(vec![block]);
        let mut text =
            CollaborativeTextState::from_legacy(TextReplicaId::new(1).unwrap(), &doc).unwrap();
        let position = text
            .relative_position(
                &doc,
                TextTarget::Markdown(block_id),
                17,
                TextAssociation::After,
            )
            .unwrap();
        let group = InkGroupId::new_v4();
        apply(
            &mut doc,
            DocumentOp::SetInkAnchor {
                group,
                anchor: InkAnchor {
                    target: AnchorTarget::Text {
                        position: position.clone(),
                    },
                    dx: 0.0,
                    dy: 0.0,
                },
            },
        )
        .unwrap();

        text.local_edit(
            &mut doc,
            TextTarget::Markdown(block_id),
            crate::text_crdt::TextEdit {
                start_utf16: 0,
                end_utf16: 0,
                replacement: "intro\n\n".into(),
            },
        )
        .unwrap();

        let AnchorTarget::Text { position } = &doc.ink.as_ref().unwrap().anchors[&group].target
        else {
            panic!("expected text anchor");
        };
        assert_eq!(text.resolve_relative_position(position).unwrap(), 24);
    }

    #[test]
    fn deleting_a_block_moves_its_anchors_to_a_neighbour_or_the_page() {
        let first = markdown_block("a");
        let second = markdown_block("b");
        let (first_id, second_id) = (first.id, second.id);
        let (mut doc, _) = doc_with_layer(vec![first, second]);
        let group = InkGroupId::new_v4();
        apply(
            &mut doc,
            DocumentOp::SetInkAnchor {
                group,
                anchor: block_anchor(first_id),
            },
        )
        .unwrap();

        apply(&mut doc, DocumentOp::DeleteBlock { block_id: first_id }).unwrap();
        let anchor = &doc.ink.as_ref().unwrap().anchors[&group];
        assert_eq!(
            anchor.target,
            AnchorTarget::Block {
                block_id: second_id
            }
        );
        assert_eq!((anchor.dx, anchor.dy), (12.0, 34.0));

        assert_eq!(
            replay(
                &mut doc,
                DocumentOp::DeleteBlock {
                    block_id: second_id
                },
            ),
            ReplayOutcome::Applied
        );
        assert_eq!(
            doc.ink.as_ref().unwrap().anchors[&group].target,
            AnchorTarget::Page
        );
    }

    #[test]
    fn replacing_markdown_demotes_text_anchors_to_the_block() {
        let block = markdown_block("text");
        let block_id = block.id;
        let (mut doc, _) = doc_with_layer(vec![block]);
        let mut text =
            CollaborativeTextState::from_legacy(TextReplicaId::new(1).unwrap(), &doc).unwrap();
        let position = text
            .relative_position(
                &doc,
                TextTarget::Markdown(block_id),
                2,
                TextAssociation::After,
            )
            .unwrap();
        let group = InkGroupId::new_v4();
        doc.ink.as_mut().unwrap().anchors.insert(
            group,
            InkAnchor {
                target: AnchorTarget::Text { position },
                dx: 1.0,
                dy: 2.0,
            },
        );

        apply(
            &mut doc,
            DocumentOp::ReplaceBlockContent {
                block_id,
                content: BlockContent::Divider,
            },
        )
        .unwrap();
        assert_eq!(
            doc.ink.as_ref().unwrap().anchors[&group].target,
            AnchorTarget::Block { block_id }
        );
    }

    fn doc_with_legacy_ink() -> (InkstoneDocument, BlockId) {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(markdown_block("before"));
        let mut legacy = CollaborationDoc::new(
            ActorId::from_bytes([3; 16]),
            stonepen_core::InkDoc::new(640.0, 480.0),
        )
        .unwrap();
        let layer_id = legacy.document().active_layer_id;
        let mut stroke = framed_stroke(InkGroupId::new_v4(), 300.0);
        stroke.parent_id = None;
        legacy.add_stroke(layer_id, None, stroke).unwrap();
        let snapshot = legacy.snapshot().unwrap();
        let ink = DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Ink {
                stonepen_json: stonepen_core::project_opaque_snapshot(&snapshot).unwrap(),
                collaboration: Some(InkCollaborationState {
                    version: INK_COLLABORATION_VERSION,
                    stream_epoch: uuid::Uuid::new_v4(),
                    snapshot: snapshot.into(),
                }),
            },
        );
        let ink_id = ink.id;
        doc.blocks.push(ink);
        doc.blocks.push(markdown_block("after"));
        (doc, ink_id)
    }

    #[test]
    fn migration_replaces_ink_blocks_with_anchored_spaces() {
        let (mut doc, ink_id) = doc_with_legacy_ink();
        let op = plan_ink_block_migration(&doc).unwrap().unwrap();
        apply(&mut doc, op).unwrap();

        let Some(BlockContent::Space { min_height }) =
            doc.get_block(ink_id).map(|block| block.content.clone())
        else {
            panic!("expected Space block");
        };
        assert!(min_height >= 300.0 && min_height > DEFAULT_SPACE_HEIGHT);
        let layer = doc.ink.as_ref().unwrap();
        assert_eq!(layer.anchors.len(), 1);
        let (group, anchor) = layer.anchors.iter().next().unwrap();
        assert_eq!(anchor.target, AnchorTarget::Block { block_id: ink_id });
        let projected = CollaborationDoc::from_snapshot(
            ActorId::from_bytes([9; 16]),
            layer.collaboration.snapshot.bytes().unwrap(),
        )
        .unwrap();
        let bounds = projected.document().frame_bounds();
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0].0.0, group.0);
        assert!(plan_ink_block_migration(&doc).unwrap().is_none());
    }

    #[test]
    fn second_migration_is_rejected_locally_and_ignored_in_replay() {
        let (mut doc, _) = doc_with_legacy_ink();
        let first = plan_ink_block_migration(&doc).unwrap().unwrap();
        let second = plan_ink_block_migration(&doc).unwrap().unwrap();
        assert_eq!(replay(&mut doc, first), ReplayOutcome::Applied);
        let before = serde_json::to_vec(&doc).unwrap();
        assert!(matches!(
            apply(&mut doc, second.clone()),
            Err(ApplyError::InkLayerExists)
        ));
        assert!(matches!(
            replay(&mut doc, second),
            ReplayOutcome::NoOp { .. }
        ));
        assert_eq!(serde_json::to_vec(&doc).unwrap(), before);
    }

    #[test]
    fn document_with_layer_roundtrips_and_exports() {
        let (mut doc, ink_id) = doc_with_legacy_ink();
        let op = plan_ink_block_migration(&doc).unwrap().unwrap();
        apply(&mut doc, op).unwrap();

        let json = serde_json::to_string(&doc).unwrap();
        let back: InkstoneDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.ink.as_ref().unwrap().anchors,
            doc.ink.as_ref().unwrap().anchors
        );

        let markdown = materialize_document_markdown(&doc);
        assert!(markdown.contains(&format!("<!-- inkstone-space block={ink_id}")));
        assert!(markdown.contains("```inkstone-page-ink\n"));
        assert!(markdown.starts_with("before"));
    }

    #[test]
    fn documents_without_layer_keep_their_serialized_shape() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(markdown_block("x"));
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("\"ink\""));
    }

    fn thread(anchor: Option<InkAnchor>, parent: Option<CommentId>, body: &str) -> Comment {
        Comment {
            anchor,
            parent,
            author_id: None,
            author: "a@example.test".into(),
            created_at_ms: 1,
            edited_at_ms: None,
            body: body.into(),
            resolved: false,
        }
    }

    fn add(comment_id: CommentId, comment: Comment) -> DocumentOp {
        DocumentOp::AddComment {
            comment_id,
            comment,
        }
    }

    #[test]
    fn comments_thread_edit_resolve_and_delete_with_replies() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let block = markdown_block("hello");
        let block_id = block.id;
        doc.blocks.push(block);
        let (root, reply) = (CommentId::new_v4(), CommentId::new_v4());
        apply(
            &mut doc,
            add(root, thread(Some(block_anchor(block_id)), None, "Why?")),
        )
        .unwrap();
        apply(&mut doc, add(reply, thread(None, Some(root), "Because"))).unwrap();
        apply(
            &mut doc,
            DocumentOp::EditComment {
                comment_id: reply,
                body: "Because.".into(),
            },
        )
        .unwrap();
        apply(
            &mut doc,
            DocumentOp::ResolveComment {
                comment_id: root,
                resolved: true,
            },
        )
        .unwrap();
        assert_eq!(doc.comments[&reply].body, "Because.");
        assert_eq!(doc.comments[&reply].edited_at_ms, Some(1_000));
        assert!(doc.comments[&root].resolved);
        apply(&mut doc, DocumentOp::DeleteComment { comment_id: root }).unwrap();
        assert!(doc.comments.is_empty());
    }

    #[test]
    fn invalid_comments_are_rejected_locally() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let root = CommentId::new_v4();
        let page = InkAnchor {
            target: AnchorTarget::Page,
            dx: 1.0,
            dy: 2.0,
        };
        assert!(apply(&mut doc, add(root, thread(Some(page.clone()), None, "  "))).is_err());
        assert!(apply(&mut doc, add(root, thread(None, None, "x"))).is_err());
        assert!(
            apply(
                &mut doc,
                add(root, thread(Some(page.clone()), Some(root), "x"))
            )
            .is_err()
        );
        let missing = InkAnchor {
            target: AnchorTarget::Block {
                block_id: BlockId::new_v4(),
            },
            dx: 0.0,
            dy: 0.0,
        };
        assert!(apply(&mut doc, add(root, thread(Some(missing), None, "x"))).is_err());
        apply(&mut doc, add(root, thread(Some(page), None, "x"))).unwrap();
        let reply = CommentId::new_v4();
        apply(&mut doc, add(reply, thread(None, Some(root), "y"))).unwrap();
        assert!(
            apply(
                &mut doc,
                add(CommentId::new_v4(), thread(None, Some(reply), "z"))
            )
            .is_err()
        );
        assert!(
            apply(
                &mut doc,
                DocumentOp::ResolveComment {
                    comment_id: reply,
                    resolved: true,
                },
            )
            .is_err()
        );
        assert!(
            apply(
                &mut doc,
                DocumentOp::DeleteComment {
                    comment_id: CommentId::new_v4()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn comment_replay_tolerates_concurrent_deletes() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let root = CommentId::new_v4();
        let gone = InkAnchor {
            target: AnchorTarget::Block {
                block_id: BlockId::new_v4(),
            },
            dx: 0.0,
            dy: 0.0,
        };
        assert_eq!(
            replay(&mut doc, add(root, thread(Some(gone.clone()), None, "x"))),
            ReplayOutcome::Applied
        );
        assert!(matches!(
            replay(&mut doc, add(root, thread(Some(gone), None, "x"))),
            ReplayOutcome::NoOp { .. }
        ));
        assert_eq!(
            replay(&mut doc, DocumentOp::DeleteComment { comment_id: root }),
            ReplayOutcome::Applied
        );
        assert_eq!(
            replay(&mut doc, DocumentOp::DeleteComment { comment_id: root }),
            ReplayOutcome::Applied
        );
        assert!(matches!(
            replay(
                &mut doc,
                add(CommentId::new_v4(), thread(None, Some(root), "late reply"))
            ),
            ReplayOutcome::NoOp { .. }
        ));
        assert!(matches!(
            replay(
                &mut doc,
                DocumentOp::EditComment {
                    comment_id: root,
                    body: "x".into()
                }
            ),
            ReplayOutcome::NoOp { .. }
        ));
        assert!(doc.comments.is_empty());
    }

    #[test]
    fn deleting_a_block_moves_comment_pins_and_old_documents_still_load() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let (first, second) = (markdown_block("a"), markdown_block("b"));
        let (first_id, second_id) = (first.id, second.id);
        doc.blocks.extend([first, second]);
        let root = CommentId::new_v4();
        apply(
            &mut doc,
            add(root, thread(Some(block_anchor(first_id)), None, "x")),
        )
        .unwrap();
        apply(&mut doc, DocumentOp::DeleteBlock { block_id: first_id }).unwrap();
        assert_eq!(
            doc.comments[&root].anchor.as_ref().unwrap().target,
            AnchorTarget::Block {
                block_id: second_id
            }
        );
        let json = serde_json::to_string(&doc).unwrap();
        let loaded: InkstoneDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.comments, doc.comments);
        let mut bare = InkstoneDocument::new(DocId::new_v4());
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("comments"));
        bare = serde_json::from_str(&json).unwrap();
        assert!(bare.comments.is_empty());
    }
}
