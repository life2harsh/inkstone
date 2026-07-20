use std::collections::{BTreeMap, HashMap};

use inkstone_core::DocumentUpdate;
use inkstone_core::ids::UpdateId;

/// Consistency failure: same seq + different update_id or same update_id + different seq.
#[derive(Debug, Clone)]
pub struct InboxFatal {
    pub message: String,
    pub seq: i64,
    pub update_id: UpdateId,
}

/// Sequenced inbox for server updates.
///
/// Stores updates by seq. Does NOT own or track confirmedSeq — the
/// ClientState drives confirmation via take() + remove().
///
/// Consistency checks:
/// - same seq + different update_id → fatal
/// - same update_id + different seq → fatal
#[derive(Debug, Clone)]
pub struct SequencedInbox {
    buffer: BTreeMap<i64, DocumentUpdate>,
    seq_by_update_id: HashMap<UpdateId, i64>,
    /// Tracks update_id → seq for already-confirmed entries so we can detect
    /// duplicate update_id arriving at a different seq later in the same lifecycle.
    confirmed_history: HashMap<UpdateId, i64>,
}

impl SequencedInbox {
    pub fn new() -> Self {
        Self {
            buffer: BTreeMap::new(),
            seq_by_update_id: HashMap::new(),
            confirmed_history: HashMap::new(),
        }
    }

    /// Store a sequenced update.
    /// Returns `InboxFatal` on consistency violation.
    pub fn store(&mut self, seq: i64, update: DocumentUpdate) -> Option<InboxFatal> {
        if let Some(&confirmed_seq) = self.confirmed_history.get(&update.update_id) {
            if confirmed_seq != seq {
                return Some(InboxFatal {
                    message: format!(
                        "Update {} already confirmed at seq {} vs new seq {}",
                        update.update_id, confirmed_seq, seq
                    ),
                    seq,
                    update_id: update.update_id,
                });
            }
            return None;
        }

        if let Some(existing) = self.buffer.get(&seq) {
            if existing.update_id != update.update_id {
                return Some(InboxFatal {
                    message: format!(
                        "Seq {} already stored with different update_id: {} vs {}",
                        seq, existing.update_id, update.update_id
                    ),
                    seq,
                    update_id: update.update_id,
                });
            }
            return None;
        }

        if let Some(&existing_seq) = self.seq_by_update_id.get(&update.update_id) {
            if existing_seq != seq {
                return Some(InboxFatal {
                    message: format!(
                        "Update {} already stored at seq {} vs {}",
                        update.update_id, existing_seq, seq
                    ),
                    seq,
                    update_id: update.update_id,
                });
            }
            return None;
        }

        self.buffer.insert(seq, update.clone());
        self.seq_by_update_id.insert(update.update_id, seq);
        None
    }

    /// Return the entry at `expected_seq` if available.
    pub fn take(&self, expected_seq: i64) -> Option<(i64, DocumentUpdate)> {
        self.buffer
            .get(&expected_seq)
            .map(|u| (expected_seq, u.clone()))
    }

    /// Remove a sequenced entry after confirming/replaying it.
    /// Preserves the update_id → seq mapping in `confirmed_history` so that
    /// a duplicate update_id arriving at a different seq is detected as fatal.
    pub fn remove(&mut self, seq: i64) {
        if let Some(update) = self.buffer.remove(&seq) {
            self.seq_by_update_id.remove(&update.update_id);
            self.confirmed_history.insert(update.update_id, seq);
        }
    }

    /// True if there is a gap between `external_confirmed_seq` and the
    /// smallest buffered seq.
    pub fn has_gap(&self, external_confirmed_seq: i64) -> bool {
        self.buffer
            .first_key_value()
            .map(|(&k, _)| k > external_confirmed_seq + 1)
            .unwrap_or(false)
    }

    /// Number of buffered entries.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.seq_by_update_id.clear();
        self.confirmed_history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::doc_ops::DOCUMENT_UPDATE_VERSION;
    use inkstone_core::ids::DocId;

    fn make_update(doc_id: DocId) -> DocumentUpdate {
        DocumentUpdate {
            version: DOCUMENT_UPDATE_VERSION,
            update_id: UpdateId::new_v4(),
            doc_id,
            applied_at_ms: 1000,
            op: inkstone_core::DocumentOp::SetTitle {
                title: "test".into(),
            },
        }
    }

    #[test]
    fn test_store_and_take() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();
        let update = make_update(doc_id);

        assert!(inbox.store(11, update.clone()).is_none());
        assert!(inbox.take(11).is_some());
        assert!(inbox.take(12).is_none());
    }

    #[test]
    fn test_remove_after_take() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();
        let update = make_update(doc_id);

        inbox.store(11, update.clone());
        inbox.remove(11);
        assert!(inbox.take(11).is_none());
        assert!(inbox.is_empty());
    }

    #[test]
    fn test_seq_12_before_11_buffers() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();

        let u12 = make_update(doc_id);
        let u11 = make_update(doc_id);

        inbox.store(12, u12.clone());
        inbox.store(11, u11.clone());

        // 12 still available after 11 is taken and removed
        assert!(inbox.take(11).is_some());
        inbox.remove(11);
        assert!(inbox.take(12).is_some());
    }

    #[test]
    fn test_same_seq_different_update_id_is_fatal() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();

        let u1 = make_update(doc_id);
        let u2 = make_update(doc_id); // different update_id

        assert!(inbox.store(11, u1).is_none());
        let fatal = inbox.store(11, u2);
        assert!(fatal.is_some());
        assert!(fatal.unwrap().message.contains("different update_id"));
    }

    #[test]
    fn test_same_update_id_different_seq_is_fatal() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();

        let update = make_update(doc_id);
        let update2 = DocumentUpdate {
            update_id: update.update_id,
            ..make_update(doc_id)
        };

        assert!(inbox.store(11, update).is_none());
        let fatal = inbox.store(12, update2);
        assert!(fatal.is_some());
        assert!(fatal.unwrap().message.contains("already stored at seq"));
    }

    #[test]
    fn test_has_gap() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();

        assert!(!inbox.has_gap(0));

        inbox.store(13, make_update(doc_id));
        // Smallest is 13, external confirmed is 0
        assert!(inbox.has_gap(0));
        // Not a gap if external is 12
        assert!(!inbox.has_gap(12));
        // Gap if external is 11 (need 12, have 13)
        assert!(inbox.has_gap(11));
    }

    #[test]
    fn test_future_seq_survives_earlier_confirmation() {
        let doc_id = DocId::new_v4();
        let mut inbox = SequencedInbox::new();

        inbox.store(13, make_update(doc_id));
        assert_eq!(inbox.len(), 1);

        // Simulate: confirmed_seq = 11, so take 12 would be null
        assert!(inbox.take(12).is_none());

        inbox.store(11, make_update(doc_id));
        inbox.store(12, make_update(doc_id));

        assert!(inbox.take(11).is_some());
        inbox.remove(11);
        assert!(inbox.take(12).is_some());
        inbox.remove(12);
        assert!(inbox.take(13).is_some());
        inbox.remove(13);
        assert!(inbox.is_empty());
    }
}
