use std::collections::VecDeque;

use inkstone_core::document::InkstoneDocument;
use inkstone_core::ids::{DocId, UpdateId};
use inkstone_core::{
    CollaborativeTextSnapshot, CollaborativeTextState, DocumentUpdate, RelativePosition,
    RelativeSelection, TextAssociation, TextCrdtError, TextEdit, TextReplicaId, TextTarget,
};

#[derive(Debug, Clone)]
pub enum TextClientEvent {
    Hydrate {
        document: InkstoneDocument,
        snapshot: Option<CollaborativeTextSnapshot>,
        pending_updates: Vec<DocumentUpdate>,
    },
    LocalEdit {
        target: TextTarget,
        edit: TextEdit,
        update_id: UpdateId,
        applied_at_ms: i64,
    },
    LocalUpdatePersisted {
        update_id: UpdateId,
    },
    LocalUpdatePersistenceFailed {
        update_id: UpdateId,
    },
    RetryLocalPersistence,
    RemoteUpdate {
        update: DocumentUpdate,
    },
    RemoteMergePersisted {
        token: TextMergeToken,
    },
    RemoteMergePersistenceFailed {
        token: TextMergeToken,
    },
    Connected,
    Disconnected,
}

#[derive(Debug, Clone)]
pub enum TextClientEffect {
    PersistLocalUpdate {
        update: DocumentUpdate,
    },
    PersistRemoteMerge {
        token: TextMergeToken,
        checkpoint: TextMergeCheckpoint,
    },
    SendUpdate(DocumentUpdate),
    NotifyError(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextMergeToken(pub u64);

#[derive(Debug, Clone)]
pub struct TextMergeCheckpoint {
    pub document: InkstoneDocument,
    pub text_snapshot: CollaborativeTextSnapshot,
    pub pending_updates: Vec<DocumentUpdate>,
}

#[derive(Debug, Clone)]
struct QueuedEdit {
    target: TextTarget,
    edit: TextEdit,
    update_id: UpdateId,
    applied_at_ms: i64,
}

#[derive(Debug, Clone)]
struct PreparedEdit {
    update: DocumentUpdate,
    document: InkstoneDocument,
    text: CollaborativeTextState,
}

#[derive(Debug, Clone)]
struct PreparedMerge {
    token: TextMergeToken,
    checkpoint: TextMergeCheckpoint,
    document: InkstoneDocument,
    text: CollaborativeTextState,
}

#[derive(Debug, Clone)]
pub struct TextClientState {
    doc_id: DocId,
    replica_id: TextReplicaId,
    confirmed_document: InkstoneDocument,
    confirmed_text: CollaborativeTextState,
    view_document: InkstoneDocument,
    view_text: CollaborativeTextState,
    pending_updates: Vec<DocumentUpdate>,
    prepared_edit: Option<PreparedEdit>,
    queued_edits: VecDeque<QueuedEdit>,
    prepared_merge: Option<PreparedMerge>,
    queued_remote: VecDeque<DocumentUpdate>,
    local_persistence_failed: bool,
    connected: bool,
    next_merge_token: u64,
    errors: Vec<String>,
}

impl TextClientState {
    pub fn local_persistence_failed(&self) -> bool {
        self.local_persistence_failed
    }

    pub fn has_pending_persistence(&self) -> bool {
        self.prepared_edit.is_some()
            || !self.queued_edits.is_empty()
            || self.local_persistence_failed
    }

    pub fn new(
        document: InkstoneDocument,
        replica_id: TextReplicaId,
    ) -> Result<Self, TextCrdtError> {
        let text = CollaborativeTextState::from_legacy(replica_id, &document)?;
        Ok(Self {
            doc_id: document.id,
            replica_id,
            confirmed_document: document.clone(),
            confirmed_text: text.clone(),
            view_document: document,
            view_text: text,
            pending_updates: Vec::new(),
            prepared_edit: None,
            queued_edits: VecDeque::new(),
            prepared_merge: None,
            queued_remote: VecDeque::new(),
            local_persistence_failed: false,
            connected: false,
            next_merge_token: 0,
            errors: Vec::new(),
        })
    }

    pub fn document(&self) -> &InkstoneDocument {
        self.prepared_edit
            .as_ref()
            .map(|prepared| &prepared.document)
            .unwrap_or(&self.view_document)
    }

    pub fn confirmed_document(&self) -> &InkstoneDocument {
        &self.confirmed_document
    }

    pub fn pending_updates(&self) -> &[DocumentUpdate] {
        &self.pending_updates
    }

    pub fn snapshot(&self) -> CollaborativeTextSnapshot {
        self.view_text.snapshot()
    }

    pub fn persistence_snapshot(&self) -> CollaborativeTextSnapshot {
        self.prepared_edit
            .as_ref()
            .map(|prepared| prepared.text.snapshot())
            .unwrap_or_else(|| self.view_text.snapshot())
    }

    pub fn confirmed_snapshot(&self) -> CollaborativeTextSnapshot {
        self.confirmed_text.snapshot()
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn relative_selection(
        &mut self,
        target: TextTarget,
        anchor_utf16: u32,
        focus_utf16: u32,
    ) -> Result<RelativeSelection, TextCrdtError> {
        let anchor = self.view_text.relative_position(
            &self.view_document,
            target,
            anchor_utf16,
            TextAssociation::After,
        )?;
        let focus = self.view_text.relative_position(
            &self.view_document,
            target,
            focus_utf16,
            TextAssociation::Before,
        )?;
        Ok(RelativeSelection { anchor, focus })
    }

    pub fn resolve_position(&self, position: &RelativePosition) -> Result<u32, TextCrdtError> {
        self.view_text.resolve_relative_position(position)
    }

    pub fn transition(&mut self, event: TextClientEvent) -> Vec<TextClientEffect> {
        match event {
            TextClientEvent::Hydrate {
                document,
                snapshot,
                pending_updates,
            } => self.hydrate(document, snapshot, pending_updates),
            TextClientEvent::LocalEdit {
                target,
                edit,
                update_id,
                applied_at_ms,
            } => {
                self.queued_edits.push_back(QueuedEdit {
                    target,
                    edit,
                    update_id,
                    applied_at_ms,
                });
                self.prepare_next_edit()
            }
            TextClientEvent::LocalUpdatePersisted { update_id } => {
                let Some(prepared) = self.prepared_edit.take() else {
                    return Vec::new();
                };
                if prepared.update.update_id != update_id {
                    self.prepared_edit = Some(prepared);
                    return Vec::new();
                }
                self.local_persistence_failed = false;
                self.pending_updates.push(prepared.update.clone());
                if let Err(error) = self.rebuild_view() {
                    return self.fail(error);
                }
                let mut effects = if self.connected {
                    vec![TextClientEffect::SendUpdate(prepared.update)]
                } else {
                    Vec::new()
                };
                effects.extend(self.prepare_next_edit());
                effects
            }
            TextClientEvent::LocalUpdatePersistenceFailed { update_id } => {
                if self
                    .prepared_edit
                    .as_ref()
                    .is_some_and(|prepared| prepared.update.update_id == update_id)
                {
                    self.local_persistence_failed = true;
                    vec![TextClientEffect::NotifyError(format!(
                        "failed to persist text update {update_id}"
                    ))]
                } else {
                    Vec::new()
                }
            }
            TextClientEvent::RetryLocalPersistence => {
                if !self.local_persistence_failed {
                    return Vec::new();
                }
                self.local_persistence_failed = false;
                self.prepared_edit
                    .as_ref()
                    .map(|prepared| {
                        vec![TextClientEffect::PersistLocalUpdate {
                            update: prepared.update.clone(),
                        }]
                    })
                    .unwrap_or_default()
            }
            TextClientEvent::RemoteUpdate { update } => {
                self.queued_remote.push_back(update);
                self.prepare_next_merge()
            }
            TextClientEvent::RemoteMergePersisted { token } => {
                let Some(prepared) = self.prepared_merge.take() else {
                    return Vec::new();
                };
                if prepared.token != token {
                    self.prepared_merge = Some(prepared);
                    return Vec::new();
                }
                self.confirmed_document = prepared.document;
                self.confirmed_text = prepared.text;
                self.pending_updates = prepared.checkpoint.pending_updates;
                if let Err(error) = self.rebuild_view() {
                    return self.fail(error);
                }
                if let Err(error) = self.rebase_prepared_edit() {
                    return self.fail(error);
                }
                self.prepare_next_merge()
            }
            TextClientEvent::RemoteMergePersistenceFailed { token } => {
                if let Some(prepared) = &self.prepared_merge
                    && prepared.token == token
                {
                    vec![TextClientEffect::PersistRemoteMerge {
                        token,
                        checkpoint: prepared.checkpoint.clone(),
                    }]
                } else {
                    Vec::new()
                }
            }
            TextClientEvent::Connected => {
                self.connected = true;
                self.pending_updates
                    .iter()
                    .cloned()
                    .map(TextClientEffect::SendUpdate)
                    .collect()
            }
            TextClientEvent::Disconnected => {
                self.connected = false;
                Vec::new()
            }
        }
    }

    fn hydrate(
        &mut self,
        document: InkstoneDocument,
        snapshot: Option<CollaborativeTextSnapshot>,
        pending_updates: Vec<DocumentUpdate>,
    ) -> Vec<TextClientEffect> {
        if document.id != self.doc_id
            || pending_updates
                .iter()
                .any(|update| update.doc_id != self.doc_id)
        {
            return self.fail(TextCrdtError::WrongDocument);
        }
        let text = match snapshot {
            Some(snapshot) => CollaborativeTextState::from_snapshot(self.replica_id, &snapshot),
            None => CollaborativeTextState::from_legacy(self.replica_id, &document),
        };
        let text = match text {
            Ok(text) => text,
            Err(error) => return self.fail(error),
        };
        self.confirmed_document = document.clone();
        self.confirmed_text = text;
        self.view_document = document;
        self.pending_updates = pending_updates;
        if let Err(error) = self.rebuild_view() {
            return self.fail(error);
        }
        Vec::new()
    }

    fn prepare_next_edit(&mut self) -> Vec<TextClientEffect> {
        if self.prepared_edit.is_some() || self.local_persistence_failed {
            return Vec::new();
        }
        let Some(queued) = self.queued_edits.pop_front() else {
            return Vec::new();
        };
        let mut document = self.view_document.clone();
        let mut text = self.view_text.clone();
        match text.local_document_edit(
            &mut document,
            queued.update_id,
            queued.applied_at_ms,
            queued.target,
            queued.edit,
        ) {
            Ok(update) => {
                self.prepared_edit = Some(PreparedEdit {
                    update: update.clone(),
                    document,
                    text,
                });
                vec![TextClientEffect::PersistLocalUpdate { update }]
            }
            Err(error) => self.fail(error),
        }
    }

    fn prepare_next_merge(&mut self) -> Vec<TextClientEffect> {
        if self.prepared_merge.is_some() {
            return Vec::new();
        }
        let Some(update) = self.queued_remote.pop_front() else {
            return Vec::new();
        };
        let mut document = self.confirmed_document.clone();
        let mut text = self.confirmed_text.clone();
        if let Err(error) = text.apply_document_update(&mut document, &update) {
            return self.fail(error);
        }
        let pending_updates = self
            .pending_updates
            .iter()
            .filter(|pending| pending.update_id != update.update_id)
            .cloned()
            .collect();
        let checkpoint = TextMergeCheckpoint {
            document: document.clone(),
            text_snapshot: text.snapshot(),
            pending_updates,
        };
        let token = TextMergeToken(self.next_merge_token);
        self.next_merge_token += 1;
        self.prepared_merge = Some(PreparedMerge {
            token,
            checkpoint: checkpoint.clone(),
            document,
            text,
        });
        vec![TextClientEffect::PersistRemoteMerge { token, checkpoint }]
    }

    fn rebuild_view(&mut self) -> Result<(), TextCrdtError> {
        let mut document = self.confirmed_document.clone();
        let mut text = self.confirmed_text.clone();
        for update in &self.pending_updates {
            text.apply_document_update(&mut document, update)?;
        }
        self.view_document = document;
        self.view_text = text;
        Ok(())
    }

    fn rebase_prepared_edit(&mut self) -> Result<(), TextCrdtError> {
        let Some(prepared) = &mut self.prepared_edit else {
            return Ok(());
        };
        let mut document = self.view_document.clone();
        let mut text = self.view_text.clone();
        text.apply_document_update(&mut document, &prepared.update)?;
        prepared.document = document;
        prepared.text = text;
        Ok(())
    }

    fn fail(&mut self, error: TextCrdtError) -> Vec<TextClientEffect> {
        let message = error.to_string();
        self.errors.push(message.clone());
        vec![TextClientEffect::NotifyError(message)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::doc_ops::DocumentOp;
    use inkstone_core::document::InkstoneDocument;

    fn state(replica: u64) -> TextClientState {
        TextClientState::new(
            InkstoneDocument::new(DocId::new_v4()),
            TextReplicaId::new(replica).unwrap(),
        )
        .unwrap()
    }

    fn local_edit(state: &mut TextClientState, text: &str) -> DocumentUpdate {
        let effects = state.transition(TextClientEvent::LocalEdit {
            target: TextTarget::Title,
            edit: TextEdit {
                start_utf16: state.document().title.encode_utf16().count() as u32,
                end_utf16: state.document().title.encode_utf16().count() as u32,
                replacement: text.into(),
            },
            update_id: UpdateId::new_v4(),
            applied_at_ms: 1,
        });
        match effects.as_slice() {
            [TextClientEffect::PersistLocalUpdate { update }] => update.clone(),
            _ => panic!("expected local persistence"),
        }
    }

    fn remote_update(replica: u64, document: &InkstoneDocument, text: &str) -> DocumentUpdate {
        let mut document = document.clone();
        let mut state =
            CollaborativeTextState::from_legacy(TextReplicaId::new(replica).unwrap(), &document)
                .unwrap();
        state
            .local_document_edit(
                &mut document,
                UpdateId::new_v4(),
                1,
                TextTarget::Title,
                TextEdit {
                    start_utf16: 0,
                    end_utf16: 0,
                    replacement: text.into(),
                },
            )
            .unwrap()
    }

    #[test]
    fn local_edit_is_visible_but_not_sendable_until_persisted() {
        let mut state = state(101);
        state.transition(TextClientEvent::Connected);
        let update = local_edit(&mut state, "local");
        assert_eq!(state.document().title, "local");
        assert!(state.pending_updates().is_empty());

        let effects = state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: update.update_id,
        });
        assert_eq!(state.pending_updates().len(), 1);
        assert!(
            matches!(effects.as_slice(), [TextClientEffect::SendUpdate(sent)] if sent.update_id == update.update_id)
        );
    }

    #[test]
    fn persistence_failure_retries_exact_update() {
        let mut state = state(102);
        let update = local_edit(&mut state, "retry");
        state.transition(TextClientEvent::LocalUpdatePersistenceFailed {
            update_id: update.update_id,
        });
        let effects = state.transition(TextClientEvent::RetryLocalPersistence);
        assert!(
            matches!(effects.as_slice(), [TextClientEffect::PersistLocalUpdate { update: retried }] if retried.update_id == update.update_id)
        );
    }

    #[test]
    fn pending_update_survives_restart_and_resends_on_reconnect() {
        let mut first = state(103);
        let update = local_edit(&mut first, "offline");
        first.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: update.update_id,
        });
        let document = first.confirmed_document().clone();
        let snapshot = first.confirmed_text.snapshot();

        let mut restarted =
            TextClientState::new(document.clone(), TextReplicaId::new(103).unwrap()).unwrap();
        restarted.transition(TextClientEvent::Hydrate {
            document,
            snapshot: Some(snapshot),
            pending_updates: vec![update.clone()],
        });
        assert_eq!(restarted.document().title, "offline");
        let effects = restarted.transition(TextClientEvent::Connected);
        assert!(
            matches!(effects.as_slice(), [TextClientEffect::SendUpdate(sent)] if sent.update_id == update.update_id)
        );
    }

    #[test]
    fn remote_merge_waits_for_persistence_and_echo_removes_outbox() {
        let mut state = state(104);
        let local = local_edit(&mut state, "mine");
        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: local.update_id,
        });
        let effects = state.transition(TextClientEvent::RemoteUpdate {
            update: local.clone(),
        });
        let token = match effects.as_slice() {
            [TextClientEffect::PersistRemoteMerge { token, checkpoint }] => {
                assert!(checkpoint.pending_updates.is_empty());
                *token
            }
            _ => panic!("expected remote persistence"),
        };
        assert_eq!(state.confirmed_document().title, "");
        state.transition(TextClientEvent::RemoteMergePersisted { token });
        assert_eq!(state.confirmed_document().title, "mine");
        assert!(state.pending_updates().is_empty());
    }

    #[test]
    fn concurrent_remote_merge_rebases_local_crdt_without_whole_text_conflict() {
        let mut state = state(105);
        let local = local_edit(&mut state, "L");
        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: local.update_id,
        });
        let remote = remote_update(106, state.confirmed_document(), "R");
        assert!(matches!(remote.op, DocumentOp::ApplyTextUpdateV2 { .. }));
        let effects = state.transition(TextClientEvent::RemoteUpdate { update: remote });
        let token = effects
            .iter()
            .find_map(|effect| match effect {
                TextClientEffect::PersistRemoteMerge { token, .. } => Some(*token),
                _ => None,
            })
            .unwrap();
        state.transition(TextClientEvent::RemoteMergePersisted { token });
        assert_eq!(state.document().title.encode_utf16().count(), 2);
        assert!(state.document().title.contains('L'));
        assert!(state.document().title.contains('R'));
    }

    #[test]
    fn remote_merge_before_local_persistence_cannot_install_stale_candidate() {
        let mut state = state(107);
        let local = local_edit(&mut state, "L");
        let remote = remote_update(108, state.confirmed_document(), "R");
        let effects = state.transition(TextClientEvent::RemoteUpdate { update: remote });
        let token = effects
            .iter()
            .find_map(|effect| match effect {
                TextClientEffect::PersistRemoteMerge { token, .. } => Some(*token),
                _ => None,
            })
            .unwrap();
        state.transition(TextClientEvent::RemoteMergePersisted { token });
        assert!(state.document().title.contains('L'));
        assert!(state.document().title.contains('R'));

        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: local.update_id,
        });
        assert!(state.document().title.contains('L'));
        assert!(state.document().title.contains('R'));
    }
}
