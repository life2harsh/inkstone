use inkstone_client::ConfirmationPayload;
use inkstone_core::doc_ops::DocumentUpdate;
use inkstone_core::document::InkstoneDocument;
use inkstone_core::ids::DocId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalDocumentRecord {
    pub doc_id: DocId,
    pub confirmed_seq: i64,
    pub confirmed_doc: InkstoneDocument,
    pub pending_updates: Vec<DocumentUpdate>,
}

pub fn new_record(doc_id: DocId) -> LocalDocumentRecord {
    LocalDocumentRecord {
        doc_id,
        confirmed_seq: 0,
        confirmed_doc: InkstoneDocument::new(doc_id),
        pending_updates: Vec::new(),
    }
}

pub fn append_pending_update(record: &mut LocalDocumentRecord, update: DocumentUpdate) {
    if !record
        .pending_updates
        .iter()
        .any(|u| u.update_id == update.update_id)
    {
        record.pending_updates.push(update);
    }
}

pub fn apply_confirmation(record: &mut LocalDocumentRecord, checkpoint: ConfirmationPayload) {
    record.confirmed_doc = checkpoint.confirmed_doc;
    record.confirmed_seq = checkpoint.confirmed_seq;
    record.pending_updates = checkpoint.pending_updates_after_rebase;
}

pub fn deserialize_record(json: &str) -> Result<LocalDocumentRecord, serde_json::Error> {
    serde_json::from_str(json)
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
}
