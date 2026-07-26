use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{
    Assoc, ClientID, Doc, GetString, IndexedSequence, OffsetKind, Options, ReadTxn, StateVector,
    StickyIndex, Text, Transact, Update,
};

use crate::doc_ops::{DOCUMENT_UPDATE_V2_VERSION, DocumentOp, DocumentUpdate};
use crate::document::{BlockContent, InkstoneDocument};
use crate::ids::{BlockId, UpdateId};

pub const COLLABORATIVE_TEXT_VERSION: u32 = 2;
pub const COLLABORATIVE_TEXT_SNAPSHOT_VERSION: u32 = 1;
const LEGACY_CLIENT_ID: u64 = 0;
const TEXT_ROOT: &str = "text";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "block_id", rename_all = "snake_case")]
pub enum TextTarget {
    Title,
    Markdown(BlockId),
    Math(BlockId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextReplicaId(pub u64);

impl TextReplicaId {
    pub fn new(value: u64) -> Result<Self, TextCrdtError> {
        if value == LEGACY_CLIENT_ID || value > ((1_u64 << 53) - 1) {
            Err(TextCrdtError::InvalidReplicaId(value))
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborativeTextUpdate {
    pub version: u32,
    pub target: TextTarget,
    pub update_v1: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextStateVector {
    pub version: u32,
    pub target: TextTarget,
    pub state_vector_v1: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborativeTextSnapshot {
    pub version: u32,
    pub entries: Vec<TextSnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSnapshotEntry {
    pub target: TextTarget,
    pub full_update_v1: Vec<u8>,
    pub state_vector_v1: Vec<u8>,
    pub legacy_initialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAssociation {
    Before,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelativePosition {
    pub version: u32,
    pub target: TextTarget,
    pub sticky_index_v1: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelativeSelection {
    pub anchor: RelativePosition,
    pub focus: RelativePosition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextUpdateOutcome {
    Applied { changed: bool },
    IgnoredDeleted,
    IgnoredNonText,
}

#[derive(Debug, thiserror::Error)]
pub enum TextCrdtError {
    #[error("invalid text replica id {0}")]
    InvalidReplicaId(u64),
    #[error("unsupported collaborative text version {0}")]
    UnsupportedVersion(u32),
    #[error("text target does not exist")]
    TargetNotFound,
    #[error("block target is not the declared text type")]
    TargetIsNotText,
    #[error("invalid UTF-16 edit range {start}..{end} for length {len}")]
    InvalidUtf16Range { start: u32, end: u32, len: u32 },
    #[error("malformed CRDT update: {0}")]
    MalformedUpdate(String),
    #[error("snapshot state vector does not match its full update")]
    SnapshotStateVectorMismatch,
    #[error("relative position does not resolve in its text target")]
    RelativePositionUnavailable,
    #[error("snapshot serialization failed: {0}")]
    SnapshotSerialization(String),
    #[error("update belongs to a different document")]
    WrongDocument,
    #[error("document update v2 does not contain a text v2 payload")]
    InvalidDocumentUpdatePayload,
}

struct TextDoc {
    doc: Doc,
    legacy_initialized: bool,
}

pub struct CollaborativeTextState {
    replica_id: TextReplicaId,
    docs: BTreeMap<TextTarget, TextDoc>,
}

impl std::fmt::Debug for CollaborativeTextState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollaborativeTextState")
            .field("replica_id", &self.replica_id)
            .field("targets", &self.docs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Clone for CollaborativeTextState {
    fn clone(&self) -> Self {
        Self::from_snapshot(self.replica_id, &self.snapshot()).expect("self snapshot is valid")
    }
}

impl CollaborativeTextState {
    pub fn local_document_edit(
        &mut self,
        document: &mut InkstoneDocument,
        update_id: UpdateId,
        applied_at_ms: i64,
        target: TextTarget,
        edit: TextEdit,
    ) -> Result<DocumentUpdate, TextCrdtError> {
        let payload = self.local_edit(document, target, edit)?;
        Ok(DocumentUpdate {
            version: DOCUMENT_UPDATE_V2_VERSION,
            update_id,
            doc_id: document.id,
            applied_at_ms,
            op: DocumentOp::ApplyTextUpdateV2 { payload },
        })
    }

    pub fn apply_document_update(
        &mut self,
        document: &mut InkstoneDocument,
        update: &DocumentUpdate,
    ) -> Result<TextUpdateOutcome, TextCrdtError> {
        if update.doc_id != document.id {
            return Err(TextCrdtError::WrongDocument);
        }
        if update.version != DOCUMENT_UPDATE_V2_VERSION {
            return Err(TextCrdtError::UnsupportedVersion(update.version));
        }
        match &update.op {
            DocumentOp::ApplyTextUpdateV2 { payload } => self.apply_update(document, payload),
            _ => Err(TextCrdtError::InvalidDocumentUpdatePayload),
        }
    }

    pub fn from_legacy(
        replica_id: TextReplicaId,
        document: &InkstoneDocument,
    ) -> Result<Self, TextCrdtError> {
        TextReplicaId::new(replica_id.0)?;
        let mut state = Self {
            replica_id,
            docs: BTreeMap::new(),
        };
        state.ensure_target(document, TextTarget::Title)?;
        for block in &document.blocks {
            let target = match block.content {
                BlockContent::Markdown { .. } => Some(TextTarget::Markdown(block.id)),
                BlockContent::Math { .. } => Some(TextTarget::Math(block.id)),
                _ => None,
            };
            if let Some(target) = target {
                state.ensure_target(document, target)?;
            }
        }
        Ok(state)
    }

    pub fn local_edit(
        &mut self,
        document: &mut InkstoneDocument,
        target: TextTarget,
        edit: TextEdit,
    ) -> Result<CollaborativeTextUpdate, TextCrdtError> {
        self.ensure_target(document, target)?;
        let text_doc = self.docs.get_mut(&target).expect("target was initialized");
        let text = text_doc.doc.get_or_insert_text(TEXT_ROOT);
        let read_txn = text_doc.doc.transact();
        let before_text = text.get_string(&read_txn);
        let before = read_txn.state_vector();
        drop(read_txn);
        {
            let mut txn = text_doc.doc.transact_mut();
            let len = text.len(&txn);
            if edit.start_utf16 > edit.end_utf16
                || edit.end_utf16 > len
                || !is_utf16_boundary(&before_text, edit.start_utf16)
                || !is_utf16_boundary(&before_text, edit.end_utf16)
            {
                return Err(TextCrdtError::InvalidUtf16Range {
                    start: edit.start_utf16,
                    end: edit.end_utf16,
                    len,
                });
            }
            if edit.end_utf16 > edit.start_utf16 {
                text.remove_range(
                    &mut txn,
                    edit.start_utf16,
                    edit.end_utf16 - edit.start_utf16,
                );
            }
            if !edit.replacement.is_empty() {
                text.insert(&mut txn, edit.start_utf16, &edit.replacement);
            }
        }
        let update_v1 = text_doc.doc.transact().encode_diff_v1(&before);
        self.materialize_target(document, target)?;
        Ok(CollaborativeTextUpdate {
            version: COLLABORATIVE_TEXT_VERSION,
            target,
            update_v1,
        })
    }

    pub fn apply_update(
        &mut self,
        document: &mut InkstoneDocument,
        update: &CollaborativeTextUpdate,
    ) -> Result<TextUpdateOutcome, TextCrdtError> {
        if update.version != COLLABORATIVE_TEXT_VERSION {
            return Err(TextCrdtError::UnsupportedVersion(update.version));
        }
        match target_text(document, update.target) {
            Ok(_) => {}
            Err(TextCrdtError::TargetNotFound) => return Ok(TextUpdateOutcome::IgnoredDeleted),
            Err(TextCrdtError::TargetIsNotText) => return Ok(TextUpdateOutcome::IgnoredNonText),
            Err(error) => return Err(error),
        }
        self.ensure_target(document, update.target)?;
        let before = self.text(update.target).expect("target was initialized");
        let decoded = Update::decode_v1(&update.update_v1)
            .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
        self.docs
            .get_mut(&update.target)
            .expect("target was initialized")
            .doc
            .transact_mut()
            .apply_update(decoded)
            .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
        self.materialize_target(document, update.target)?;
        Ok(TextUpdateOutcome::Applied {
            changed: self.text(update.target).as_deref() != Some(before.as_str()),
        })
    }

    pub fn text(&self, target: TextTarget) -> Option<String> {
        let text_doc = self.docs.get(&target)?;
        let text = text_doc.doc.get_or_insert_text(TEXT_ROOT);
        Some(text.get_string(&text_doc.doc.transact()))
    }

    pub fn state_vector(&self, target: TextTarget) -> Option<TextStateVector> {
        let text_doc = self.docs.get(&target)?;
        Some(TextStateVector {
            version: COLLABORATIVE_TEXT_VERSION,
            target,
            state_vector_v1: text_doc.doc.transact().state_vector().encode_v1(),
        })
    }

    pub fn diff(&self, vector: &TextStateVector) -> Result<CollaborativeTextUpdate, TextCrdtError> {
        if vector.version != COLLABORATIVE_TEXT_VERSION {
            return Err(TextCrdtError::UnsupportedVersion(vector.version));
        }
        let text_doc = self
            .docs
            .get(&vector.target)
            .ok_or(TextCrdtError::TargetNotFound)?;
        let state_vector = StateVector::decode_v1(&vector.state_vector_v1)
            .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
        Ok(CollaborativeTextUpdate {
            version: COLLABORATIVE_TEXT_VERSION,
            target: vector.target,
            update_v1: text_doc.doc.transact().encode_diff_v1(&state_vector),
        })
    }

    pub fn snapshot(&self) -> CollaborativeTextSnapshot {
        let entries = self
            .docs
            .iter()
            .map(|(target, text_doc)| {
                let txn = text_doc.doc.transact();
                TextSnapshotEntry {
                    target: *target,
                    full_update_v1: txn.encode_state_as_update_v1(&StateVector::default()),
                    state_vector_v1: txn.state_vector().encode_v1(),
                    legacy_initialized: text_doc.legacy_initialized,
                }
            })
            .collect();
        CollaborativeTextSnapshot {
            version: COLLABORATIVE_TEXT_SNAPSHOT_VERSION,
            entries,
        }
    }

    pub fn encode_snapshot(&self) -> Result<Vec<u8>, TextCrdtError> {
        serde_json::to_vec(&self.snapshot())
            .map_err(|error| TextCrdtError::SnapshotSerialization(error.to_string()))
    }

    pub fn decode_snapshot(replica_id: TextReplicaId, bytes: &[u8]) -> Result<Self, TextCrdtError> {
        let snapshot = serde_json::from_slice(bytes)
            .map_err(|error| TextCrdtError::SnapshotSerialization(error.to_string()))?;
        Self::from_snapshot(replica_id, &snapshot)
    }

    pub fn from_snapshot(
        replica_id: TextReplicaId,
        snapshot: &CollaborativeTextSnapshot,
    ) -> Result<Self, TextCrdtError> {
        TextReplicaId::new(replica_id.0)?;
        if snapshot.version != COLLABORATIVE_TEXT_SNAPSHOT_VERSION {
            return Err(TextCrdtError::UnsupportedVersion(snapshot.version));
        }
        let mut docs = BTreeMap::new();
        for entry in &snapshot.entries {
            if docs.contains_key(&entry.target) {
                return Err(TextCrdtError::MalformedUpdate(
                    "duplicate snapshot target".into(),
                ));
            }
            let doc = new_doc(replica_id.0);
            let update = Update::decode_v1(&entry.full_update_v1)
                .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
            doc.transact_mut()
                .apply_update(update)
                .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
            StateVector::decode_v1(&entry.state_vector_v1)
                .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
            docs.insert(
                entry.target,
                TextDoc {
                    doc,
                    legacy_initialized: entry.legacy_initialized,
                },
            );
        }
        Ok(Self { replica_id, docs })
    }

    pub fn relative_position(
        &mut self,
        document: &InkstoneDocument,
        target: TextTarget,
        offset_utf16: u32,
        association: TextAssociation,
    ) -> Result<RelativePosition, TextCrdtError> {
        self.ensure_target(document, target)?;
        let text_doc = self.docs.get(&target).expect("target was initialized");
        let text = text_doc.doc.get_or_insert_text(TEXT_ROOT);
        let mut txn = text_doc.doc.transact_mut();
        let len = text.len(&txn);
        if offset_utf16 > len {
            return Err(TextCrdtError::InvalidUtf16Range {
                start: offset_utf16,
                end: offset_utf16,
                len,
            });
        }
        let assoc = match association {
            TextAssociation::Before => Assoc::Before,
            TextAssociation::After => Assoc::After,
        };
        let sticky = text
            .sticky_index(&mut txn, offset_utf16, assoc)
            .ok_or(TextCrdtError::RelativePositionUnavailable)?;
        Ok(RelativePosition {
            version: COLLABORATIVE_TEXT_VERSION,
            target,
            sticky_index_v1: sticky.encode_v1(),
        })
    }

    pub fn resolve_relative_position(
        &self,
        position: &RelativePosition,
    ) -> Result<u32, TextCrdtError> {
        if position.version != COLLABORATIVE_TEXT_VERSION {
            return Err(TextCrdtError::UnsupportedVersion(position.version));
        }
        let text_doc = self
            .docs
            .get(&position.target)
            .ok_or(TextCrdtError::TargetNotFound)?;
        let sticky = StickyIndex::decode_v1(&position.sticky_index_v1)
            .map_err(|error| TextCrdtError::MalformedUpdate(error.to_string()))?;
        sticky
            .get_offset(&text_doc.doc.transact())
            .map(|offset| offset.index)
            .ok_or(TextCrdtError::RelativePositionUnavailable)
    }

    fn ensure_target(
        &mut self,
        document: &InkstoneDocument,
        target: TextTarget,
    ) -> Result<(), TextCrdtError> {
        if self.docs.contains_key(&target) {
            return match target_text(document, target) {
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            };
        }
        let initial = target_text(document, target)?;
        let seed = new_doc(LEGACY_CLIENT_ID);
        if !initial.is_empty() {
            seed.get_or_insert_text(TEXT_ROOT)
                .insert(&mut seed.transact_mut(), 0, initial);
        } else {
            seed.get_or_insert_text(TEXT_ROOT);
        }
        let full = seed
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let doc = new_doc(self.replica_id.0);
        doc.transact_mut()
            .apply_update(Update::decode_v1(&full).expect("locally encoded update must decode"))
            .expect("locally encoded legacy update must apply");
        self.docs.insert(
            target,
            TextDoc {
                doc,
                legacy_initialized: true,
            },
        );
        Ok(())
    }

    fn materialize_target(
        &self,
        document: &mut InkstoneDocument,
        target: TextTarget,
    ) -> Result<(), TextCrdtError> {
        let value = self.text(target).ok_or(TextCrdtError::TargetNotFound)?;
        match target {
            TextTarget::Title => document.title = value,
            TextTarget::Markdown(block_id) => match document.get_block_mut(block_id) {
                Some(block) => match &mut block.content {
                    BlockContent::Markdown { text } => *text = value,
                    _ => return Err(TextCrdtError::TargetIsNotText),
                },
                None => return Err(TextCrdtError::TargetNotFound),
            },
            TextTarget::Math(block_id) => match document.get_block_mut(block_id) {
                Some(block) => match &mut block.content {
                    BlockContent::Math { source } => *source = value,
                    _ => return Err(TextCrdtError::TargetIsNotText),
                },
                None => return Err(TextCrdtError::TargetNotFound),
            },
        }
        Ok(())
    }
}

fn new_doc(client_id: u64) -> Doc {
    let mut options = Options::with_client_id(ClientID::new(client_id));
    options.offset_kind = OffsetKind::Utf16;
    Doc::with_options(options)
}

fn target_text(document: &InkstoneDocument, target: TextTarget) -> Result<&str, TextCrdtError> {
    match target {
        TextTarget::Title => Ok(&document.title),
        TextTarget::Markdown(block_id) => match document.get_block(block_id) {
            Some(block) => match &block.content {
                BlockContent::Markdown { text } => Ok(text),
                _ => Err(TextCrdtError::TargetIsNotText),
            },
            None => Err(TextCrdtError::TargetNotFound),
        },
        TextTarget::Math(block_id) => match document.get_block(block_id) {
            Some(block) => match &block.content {
                BlockContent::Math { source } => Ok(source),
                _ => Err(TextCrdtError::TargetIsNotText),
            },
            None => Err(TextCrdtError::TargetNotFound),
        },
    }
}

fn is_utf16_boundary(value: &str, offset: u32) -> bool {
    let mut current = 0_u32;
    if offset == 0 {
        return true;
    }
    for character in value.chars() {
        current += character.len_utf16() as u32;
        if current == offset {
            return true;
        }
        if current > offset {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DocumentBlock, InkstoneDocument};
    use crate::ids::DocId;

    fn replica(id: u64, document: &InkstoneDocument) -> CollaborativeTextState {
        CollaborativeTextState::from_legacy(TextReplicaId::new(id).unwrap(), document).unwrap()
    }

    fn edit(
        state: &mut CollaborativeTextState,
        document: &mut InkstoneDocument,
        target: TextTarget,
        start: u32,
        end: u32,
        replacement: &str,
    ) -> CollaborativeTextUpdate {
        state
            .local_edit(
                document,
                target,
                TextEdit {
                    start_utf16: start,
                    end_utf16: end,
                    replacement: replacement.into(),
                },
            )
            .unwrap()
    }

    fn title_doc(title: &str) -> InkstoneDocument {
        let mut document = InkstoneDocument::new(DocId::new_v4());
        document.title = title.into();
        document
    }

    #[test]
    fn two_replicas_converge_for_same_position_inserts() {
        let base = title_doc("ab");
        let (mut a_doc, mut b_doc) = (base.clone(), base.clone());
        let (mut a, mut b) = (replica(1, &base), replica(2, &base));
        let a_update = edit(&mut a, &mut a_doc, TextTarget::Title, 1, 1, "A");
        let b_update = edit(&mut b, &mut b_doc, TextTarget::Title, 1, 1, "B");

        b.apply_update(&mut b_doc, &a_update).unwrap();
        a.apply_update(&mut a_doc, &b_update).unwrap();

        assert_eq!(a_doc.title, b_doc.title);
        assert!(a_doc.title == "aABb" || a_doc.title == "aBAb");
    }

    #[test]
    fn overlapping_deletes_and_offline_inserts_converge() {
        let base = title_doc("abcdef");
        let (mut a_doc, mut b_doc) = (base.clone(), base.clone());
        let (mut a, mut b) = (replica(11, &base), replica(12, &base));
        let a_update = edit(&mut a, &mut a_doc, TextTarget::Title, 1, 4, "X");
        let b_update = edit(&mut b, &mut b_doc, TextTarget::Title, 2, 5, "Y");

        a.apply_update(&mut a_doc, &b_update).unwrap();
        b.apply_update(&mut b_doc, &a_update).unwrap();

        assert_eq!(a_doc.title, b_doc.title);
    }

    #[test]
    fn three_replicas_converge_with_duplicates_and_reordered_delivery() {
        let base = title_doc("");
        let mut docs = [base.clone(), base.clone(), base.clone()];
        let mut states = [replica(21, &base), replica(22, &base), replica(23, &base)];
        let u1 = edit(&mut states[0], &mut docs[0], TextTarget::Title, 0, 0, "a");
        let u2 = edit(&mut states[1], &mut docs[1], TextTarget::Title, 0, 0, "b");
        let u3 = edit(&mut states[2], &mut docs[2], TextTarget::Title, 0, 0, "c");
        let updates = [u1, u2, u3];
        let orders = [[2, 1, 0, 2], [0, 2, 1, 0], [1, 0, 2, 1]];

        for replica_index in 0..3 {
            for update_index in orders[replica_index] {
                states[replica_index]
                    .apply_update(&mut docs[replica_index], &updates[update_index])
                    .unwrap();
            }
        }

        assert_eq!(docs[0].title, docs[1].title);
        assert_eq!(docs[1].title, docs[2].title);
        assert_eq!(docs[0].title.encode_utf16().count(), 3);
    }

    #[test]
    fn dependent_updates_can_arrive_in_reverse_order() {
        let base = title_doc("");
        let mut a_doc = base.clone();
        let mut a = replica(31, &base);
        let first = edit(&mut a, &mut a_doc, TextTarget::Title, 0, 0, "a");
        let second = edit(&mut a, &mut a_doc, TextTarget::Title, 1, 1, "b");
        let mut b_doc = base.clone();
        let mut b = replica(32, &base);

        b.apply_update(&mut b_doc, &second).unwrap();
        assert_eq!(b_doc.title, "");
        b.apply_update(&mut b_doc, &first).unwrap();
        assert_eq!(b_doc.title, "ab");
        b.apply_update(&mut b_doc, &first).unwrap();
        assert_eq!(b_doc.title, "ab");
    }

    #[test]
    fn utf16_ranges_do_not_split_emoji() {
        let base = title_doc("A😀B");
        let mut document = base.clone();
        let mut state = replica(41, &base);
        edit(&mut state, &mut document, TextTarget::Title, 1, 3, "🦀");
        assert_eq!(document.title, "A🦀B");
        let error = state
            .local_edit(
                &mut document,
                TextTarget::Title,
                TextEdit {
                    start_utf16: 2,
                    end_utf16: 2,
                    replacement: "x".into(),
                },
            )
            .unwrap_err();
        assert!(matches!(error, TextCrdtError::InvalidUtf16Range { .. }));
    }

    #[test]
    fn snapshot_roundtrip_then_tail_update_converges() {
        let base = title_doc("legacy");
        let mut a_doc = base.clone();
        let mut a = replica(51, &base);
        edit(&mut a, &mut a_doc, TextTarget::Title, 6, 6, " one");
        let encoded = a.encode_snapshot().unwrap();
        let mut b =
            CollaborativeTextState::decode_snapshot(TextReplicaId::new(52).unwrap(), &encoded)
                .unwrap();
        let mut b_doc = a_doc.clone();
        let tail = edit(&mut a, &mut a_doc, TextTarget::Title, 10, 10, " two");
        b.apply_update(&mut b_doc, &tail).unwrap();

        assert_eq!(a_doc.title, b_doc.title);
        assert_eq!(a.snapshot().entries[0].legacy_initialized, true);
    }

    #[test]
    fn local_edit_update_is_incremental_not_a_full_text_snapshot() {
        let base = title_doc(&"x".repeat(10_000));
        let mut document = base.clone();
        let mut state = replica(53, &base);
        let update = edit(
            &mut state,
            &mut document,
            TextTarget::Title,
            5_000,
            5_000,
            "y",
        );
        assert!(update.update_v1.len() < 100);
        assert!(update.update_v1.len() * 10 < state.snapshot().entries[0].full_update_v1.len());
    }

    #[test]
    fn legacy_markdown_and_math_are_initialized_deterministically() {
        let mut base = title_doc("title");
        let markdown_id = BlockId::new_v4();
        let math_id = BlockId::new_v4();
        base.blocks.push(DocumentBlock::new(
            markdown_id,
            BlockContent::Markdown {
                text: "[[Page]] #tag".into(),
            },
        ));
        base.blocks.push(DocumentBlock::new(
            math_id,
            BlockContent::Math {
                source: "x²".into(),
            },
        ));
        let a = replica(61, &base);
        let b = replica(62, &base);

        assert_eq!(
            a.text(TextTarget::Markdown(markdown_id)),
            b.text(TextTarget::Markdown(markdown_id))
        );
        assert_eq!(a.text(TextTarget::Math(math_id)).as_deref(), Some("x²"));
    }

    #[test]
    fn updates_for_deleted_and_non_text_blocks_are_deterministic_noops() {
        let mut base = title_doc("");
        let block_id = BlockId::new_v4();
        base.blocks.push(DocumentBlock::new(
            block_id,
            BlockContent::Markdown { text: "abc".into() },
        ));
        let mut source_doc = base.clone();
        let mut source = replica(71, &base);
        let update = edit(
            &mut source,
            &mut source_doc,
            TextTarget::Markdown(block_id),
            3,
            3,
            "!",
        );
        let mut deleted_doc = base.clone();
        deleted_doc.blocks.clear();
        let mut deleted = replica(72, &base);
        assert_eq!(
            deleted.apply_update(&mut deleted_doc, &update).unwrap(),
            TextUpdateOutcome::IgnoredDeleted
        );
        let mut divider_doc = base.clone();
        divider_doc.blocks[0].content = BlockContent::Divider;
        let mut divider = replica(73, &base);
        assert_eq!(
            divider.apply_update(&mut divider_doc, &update).unwrap(),
            TextUpdateOutcome::IgnoredNonText
        );
    }

    #[test]
    fn relative_position_tracks_concurrent_prefix_insert() {
        let base = title_doc("abcd");
        let mut a_doc = base.clone();
        let mut b_doc = base.clone();
        let mut a = replica(81, &base);
        let mut b = replica(82, &base);
        let position = a
            .relative_position(&a_doc, TextTarget::Title, 2, TextAssociation::After)
            .unwrap();
        let update = edit(&mut b, &mut b_doc, TextTarget::Title, 0, 0, "XX");
        a.apply_update(&mut a_doc, &update).unwrap();
        assert_eq!(a.resolve_relative_position(&position).unwrap(), 4);
    }
}
