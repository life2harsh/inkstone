use inkstone_client::ConfirmationPayload;
use inkstone_core::CollaborativeTextSnapshot;
use inkstone_core::doc_ops::DocumentUpdate;
use inkstone_core::document::{BlockContent, InkstoneDocument};
use inkstone_core::ids::DocId;
use serde::{Deserialize, Serialize};

pub const LOCAL_DOCUMENT_RECORD_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncedDocumentSnapshot {
    pub version: u32,
    pub document: InkstoneDocument,
    pub text_snapshot: CollaborativeTextSnapshot,
}

pub const SYNCED_DOCUMENT_SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalDocumentRecord {
    #[serde(default = "legacy_record_version")]
    pub version: u32,
    pub doc_id: DocId,
    pub confirmed_seq: i64,
    pub confirmed_doc: InkstoneDocument,
    pub pending_updates: Vec<DocumentUpdate>,
    #[serde(default)]
    pub text_snapshot: Option<CollaborativeTextSnapshot>,
    #[serde(default)]
    pub text_projection: Option<InkstoneDocument>,
}

const fn legacy_record_version() -> u32 {
    1
}

pub fn new_record(doc_id: DocId) -> LocalDocumentRecord {
    LocalDocumentRecord {
        version: LOCAL_DOCUMENT_RECORD_VERSION,
        doc_id,
        confirmed_seq: 0,
        confirmed_doc: InkstoneDocument::new(doc_id),
        pending_updates: Vec::new(),
        text_snapshot: None,
        text_projection: None,
    }
}

pub fn apply_text_checkpoint(
    record: &mut LocalDocumentRecord,
    document: InkstoneDocument,
    snapshot: CollaborativeTextSnapshot,
    pending_updates: Vec<DocumentUpdate>,
) {
    if document.id != record.doc_id {
        return;
    }
    record.version = LOCAL_DOCUMENT_RECORD_VERSION;
    preserve_collaborative_text(&mut record.confirmed_doc, &document);
    record.text_projection = Some(document);
    record.text_snapshot = Some(snapshot);
    let mut merged = record
        .pending_updates
        .iter()
        .filter(|update| {
            !matches!(
                update.op,
                inkstone_core::DocumentOp::ApplyTextUpdateV2 { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(pending_updates);
    record.pending_updates = merged;
}

pub fn append_pending_update(record: &mut LocalDocumentRecord, update: DocumentUpdate) {
    record.version = LOCAL_DOCUMENT_RECORD_VERSION;
    if !record
        .pending_updates
        .iter()
        .any(|u| u.update_id == update.update_id)
    {
        record.pending_updates.push(update);
    }
}

pub fn apply_confirmation(record: &mut LocalDocumentRecord, checkpoint: ConfirmationPayload) {
    if checkpoint.doc_id != record.doc_id || checkpoint.confirmed_seq < record.confirmed_seq {
        return;
    }
    let mut pending = checkpoint.pending_updates_after_rebase;
    for update in &record.pending_updates {
        if Some(update.update_id) != checkpoint.remove_outbox_update_id
            && !pending
                .iter()
                .any(|candidate| candidate.update_id == update.update_id)
        {
            pending.push(update.clone());
        }
    }
    let record_before = record.confirmed_doc.clone();
    record.confirmed_doc = checkpoint.confirmed_doc;
    if record.text_snapshot.is_some() {
        preserve_collaborative_text(&mut record.confirmed_doc, &record_before);
    }
    record.confirmed_seq = checkpoint.confirmed_seq;
    record.pending_updates = pending;
}

pub fn preserve_collaborative_text(target: &mut InkstoneDocument, source: &InkstoneDocument) {
    target.title.clone_from(&source.title);
    for block in &mut target.blocks {
        let Some(source_block) = source.get_block(block.id) else {
            continue;
        };
        match (&mut block.content, &source_block.content) {
            (BlockContent::Markdown { text }, BlockContent::Markdown { text: source }) => {
                text.clone_from(source)
            }
            (BlockContent::Math { source }, BlockContent::Math { source: current }) => {
                source.clone_from(current)
            }
            _ => {}
        }
    }
}

pub fn deserialize_record(json: &str) -> Result<LocalDocumentRecord, serde_json::Error> {
    let mut record: LocalDocumentRecord = serde_json::from_str(json)?;
    if record.version < LOCAL_DOCUMENT_RECORD_VERSION {
        record.version = LOCAL_DOCUMENT_RECORD_VERSION;
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::doc_ops::DocumentOp;
    use inkstone_core::document::{BlockContent, DocumentBlock};
    use inkstone_core::ids::{BlockId, UpdateId};

    fn create_dummy_update(doc_id: DocId, update_id: UpdateId) -> DocumentUpdate {
        DocumentUpdate {
            version: 1,
            update_id,
            doc_id,
            applied_at_ms: 1000,
            op: DocumentOp::SetTitle {
                title: "Test".to_string(),
            },
        }
    }

    #[test]
    fn test_first_persisted_update_creates_pending_state() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let update_id = UpdateId::new_v4();
        let update = create_dummy_update(doc_id, update_id);

        append_pending_update(&mut record, update.clone());

        assert_eq!(record.pending_updates.len(), 1);
        assert_eq!(record.pending_updates[0].update_id, update_id);
    }

    #[test]
    fn test_second_persisted_update_preserves_durable_order() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let u1 = UpdateId::new_v4();
        let update1 = create_dummy_update(doc_id, u1);
        let u2 = UpdateId::new_v4();
        let update2 = create_dummy_update(doc_id, u2);

        append_pending_update(&mut record, update1);
        append_pending_update(&mut record, update2);

        assert_eq!(record.pending_updates.len(), 2);
        assert_eq!(record.pending_updates[0].update_id, u1);
        assert_eq!(record.pending_updates[1].update_id, u2);
    }

    #[test]
    fn test_duplicate_update_id_is_not_appended_twice() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let u1 = UpdateId::new_v4();
        let update = create_dummy_update(doc_id, u1);

        append_pending_update(&mut record, update.clone());
        append_pending_update(&mut record, update);

        assert_eq!(record.pending_updates.len(), 1);
    }

    #[test]
    fn test_confirmation_replaces_confirmed_checkpoint() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let mut confirmed_doc = InkstoneDocument::new(doc_id);
        confirmed_doc.title = "Confirmed Title".to_string();

        let checkpoint = ConfirmationPayload {
            doc_id,
            confirmed_seq: 42,
            confirmed_doc: confirmed_doc.clone(),
            remove_outbox_update_id: None,
            pending_updates_after_rebase: Vec::new(),
        };

        apply_confirmation(&mut record, checkpoint);

        assert_eq!(record.confirmed_seq, 42);
        assert_eq!(record.confirmed_doc.title, "Confirmed Title");
        assert_eq!(record.pending_updates.len(), 0);
    }

    #[test]
    fn test_confirmation_removes_confirmed_own_update_from_pending_state() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let u1 = UpdateId::new_v4();
        let update1 = create_dummy_update(doc_id, u1);
        let u2 = UpdateId::new_v4();
        let update2 = create_dummy_update(doc_id, u2);

        append_pending_update(&mut record, update1);
        append_pending_update(&mut record, update2.clone());

        // Confirmation acknowledges update1, so rebase only contains update2
        let checkpoint = ConfirmationPayload {
            doc_id,
            confirmed_seq: 1,
            confirmed_doc: InkstoneDocument::new(doc_id),
            remove_outbox_update_id: Some(u1),
            pending_updates_after_rebase: vec![update2],
        };

        apply_confirmation(&mut record, checkpoint);

        assert_eq!(record.pending_updates.len(), 1);
        assert_eq!(record.pending_updates[0].update_id, u2);
    }

    #[test]
    fn test_remaining_pending_update_order_survives_confirmation() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let u1 = UpdateId::new_v4();
        let update1 = create_dummy_update(doc_id, u1);
        let u2 = UpdateId::new_v4();
        let update2 = create_dummy_update(doc_id, u2);
        let u3 = UpdateId::new_v4();
        let update3 = create_dummy_update(doc_id, u3);

        append_pending_update(&mut record, update1);
        append_pending_update(&mut record, update2.clone());
        append_pending_update(&mut record, update3.clone());

        let checkpoint = ConfirmationPayload {
            doc_id,
            confirmed_seq: 1,
            confirmed_doc: InkstoneDocument::new(doc_id),
            remove_outbox_update_id: Some(u1),
            pending_updates_after_rebase: vec![update2, update3],
        };

        apply_confirmation(&mut record, checkpoint);

        assert_eq!(record.pending_updates.len(), 2);
        assert_eq!(record.pending_updates[0].update_id, u2);
        assert_eq!(record.pending_updates[1].update_id, u3);
    }

    #[test]
    fn test_hydration_reconstruct_exact_confirmed_seq_and_pending_order() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);

        let u1 = UpdateId::new_v4();
        let update1 = create_dummy_update(doc_id, u1);
        let u2 = UpdateId::new_v4();
        let update2 = create_dummy_update(doc_id, u2);

        append_pending_update(&mut record, update1);
        append_pending_update(&mut record, update2);
        record.confirmed_seq = 100;

        let serialized = serde_json::to_string(&record).unwrap();
        let hydrated: LocalDocumentRecord = serde_json::from_str(&serialized).unwrap();

        assert_eq!(hydrated.confirmed_seq, 100);
        assert_eq!(hydrated.pending_updates.len(), 2);
        assert_eq!(hydrated.pending_updates[0].update_id, u1);
        assert_eq!(hydrated.pending_updates[1].update_id, u2);
    }

    #[test]
    fn test_deserialize_corrupt_data_returns_error() {
        let corrupt_json = "{invalid json}";
        let res = deserialize_record(corrupt_json);
        assert!(res.is_err());
    }

    #[test]
    fn legacy_record_migrates_without_losing_pending_updates() {
        let doc_id = DocId::new_v4();
        let pending_id = UpdateId::new_v4();
        let mut value = serde_json::to_value(new_record(doc_id)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("version");
        object.remove("text_snapshot");
        object.insert(
            "pending_updates".into(),
            serde_json::to_value(vec![create_dummy_update(doc_id, pending_id)]).unwrap(),
        );

        let migrated = deserialize_record(&serde_json::to_string(&value).unwrap()).unwrap();

        assert_eq!(migrated.version, LOCAL_DOCUMENT_RECORD_VERSION);
        assert!(migrated.text_snapshot.is_none());
        assert_eq!(migrated.pending_updates[0].update_id, pending_id);
    }

    #[test]
    fn stale_confirmation_cannot_roll_back_durable_state() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);
        record.confirmed_seq = 8;
        record.confirmed_doc.title = "newer".into();

        apply_confirmation(
            &mut record,
            ConfirmationPayload {
                doc_id,
                confirmed_seq: 7,
                confirmed_doc: InkstoneDocument::new(doc_id),
                remove_outbox_update_id: None,
                pending_updates_after_rebase: Vec::new(),
            },
        );

        assert_eq!(record.confirmed_seq, 8);
        assert_eq!(record.confirmed_doc.title, "newer");
    }

    #[test]
    fn confirmation_preserves_pending_update_added_by_another_tab() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);
        let confirmed_id = UpdateId::new_v4();
        let concurrent_id = UpdateId::new_v4();
        append_pending_update(&mut record, create_dummy_update(doc_id, confirmed_id));
        append_pending_update(&mut record, create_dummy_update(doc_id, concurrent_id));

        apply_confirmation(
            &mut record,
            ConfirmationPayload {
                doc_id,
                confirmed_seq: 1,
                confirmed_doc: InkstoneDocument::new(doc_id),
                remove_outbox_update_id: Some(confirmed_id),
                pending_updates_after_rebase: Vec::new(),
            },
        );

        assert_eq!(record.pending_updates.len(), 1);
        assert_eq!(record.pending_updates[0].update_id, concurrent_id);
    }

    #[test]
    fn test_mixed_blocks_survive_record_serialization() {
        let doc_id = DocId::new_v4();
        let mut record = new_record(doc_id);
        record.confirmed_doc.blocks = vec![
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Math {
                    source: "x^2".into(),
                },
            ),
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Ink {
                    stonepen_json: "opaque-stonepen-json".into(),
                    collaboration: None,
                },
            ),
            DocumentBlock::new(BlockId::new_v4(), BlockContent::Divider),
        ];

        let json = serde_json::to_string(&record).unwrap();
        let hydrated = deserialize_record(&json).unwrap();
        assert!(matches!(
            hydrated.confirmed_doc.blocks[0].content,
            BlockContent::Math { .. }
        ));
        assert!(matches!(
            hydrated.confirmed_doc.blocks[1].content,
            BlockContent::Ink { .. }
        ));
        assert!(matches!(
            hydrated.confirmed_doc.blocks[2].content,
            BlockContent::Divider
        ));
    }

    #[test]
    fn synchronized_snapshot_round_trips_document_and_text_crdt_state() {
        let doc_id = DocId::new_v4();
        let document = InkstoneDocument::new(doc_id);
        let text = inkstone_core::CollaborativeTextState::from_legacy(
            inkstone_core::TextReplicaId::new(7).unwrap(),
            &document,
        )
        .unwrap();
        let snapshot = SyncedDocumentSnapshot {
            version: SYNCED_DOCUMENT_SNAPSHOT_VERSION,
            document,
            text_snapshot: text.snapshot(),
        };

        let decoded: SyncedDocumentSnapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();

        assert_eq!(decoded.version, SYNCED_DOCUMENT_SNAPSHOT_VERSION);
        assert_eq!(decoded.document.id, doc_id);
        assert_eq!(
            decoded.text_snapshot.version,
            snapshot.text_snapshot.version
        );
    }
}
