use std::collections::VecDeque;

use inkstone_core::document::{BlockContent, InkstoneDocument};
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
    /// The update this merge confirmed. Local updates saved while the merge
    /// was in flight stay pending, so stores drop only this one.
    pub merged_update_id: UpdateId,
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

    /// [`Self::document`] with edits still waiting for persistence applied as
    /// plain text, so a display can show what was typed before it is durable.
    pub fn optimistic_document(&self) -> std::borrow::Cow<'_, InkstoneDocument> {
        if self.queued_edits.is_empty() {
            return std::borrow::Cow::Borrowed(self.document());
        }
        let mut document = self.document().clone();
        for queued in &self.queued_edits {
            apply_plain_edit(&mut document, queued.target, &queued.edit);
        }
        std::borrow::Cow::Owned(document)
    }

    /// Takes block structure (blocks inserted, removed, moved or retyped)
    /// from `structure`, keeping the text this state owns for every block it
    /// already has. New blocks bring their initial text, which seeds their
    /// text CRDT on first use.
    ///
    /// Unlike re-hydrating, this never re-seeds text that edits still in
    /// flight are relative to, so typed text cannot be applied twice.
    pub fn adopt_structure(&mut self, structure: &InkstoneDocument) {
        if structure.id != self.doc_id {
            return;
        }
        merge_structure(&mut self.confirmed_document, structure);
        merge_structure(&mut self.view_document, structure);
        if let Some(prepared) = self.prepared_edit.as_mut() {
            merge_structure(&mut prepared.document, structure);
        }
        if let Some(prepared) = self.prepared_merge.as_mut() {
            merge_structure(&mut prepared.document, structure);
        }
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

    /// A position in the current view text that follows concurrent edits.
    pub fn relative_position(
        &mut self,
        target: TextTarget,
        offset_utf16: u32,
        association: TextAssociation,
    ) -> Result<RelativePosition, TextCrdtError> {
        self.view_text
            .relative_position(&self.view_document, target, offset_utf16, association)
    }

    pub fn resolve_position(&self, position: &RelativePosition) -> Result<u32, TextCrdtError> {
        self.view_text.resolve_relative_position(position)
    }

    /// Resolves `position` in [`Self::optimistic_document`]: through the
    /// in-flight edit and then every edit still queued behind it.
    pub fn resolve_position_optimistic(
        &self,
        position: &RelativePosition,
    ) -> Result<u32, TextCrdtError> {
        let text = self
            .prepared_edit
            .as_ref()
            .map(|prepared| &prepared.text)
            .unwrap_or(&self.view_text);
        let mut offset = text.resolve_relative_position(position)?;
        for queued in &self.queued_edits {
            if queued.target == position.target {
                offset = shift_offset(offset, &queued.edit);
            }
        }
        Ok(offset)
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
                let merged = prepared.checkpoint.merged_update_id;
                self.pending_updates
                    .retain(|pending| pending.update_id != merged);
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
        while let Some(queued) = self.queued_edits.pop_front() {
            let mut edits = vec![queued.edit];
            let mut applied_at_ms = queued.applied_at_ms;
            while let Some(next) = self
                .queued_edits
                .pop_front_if(|next| next.target == queued.target)
            {
                edits.push(next.edit);
                applied_at_ms = next.applied_at_ms;
            }
            let mut document = self.view_document.clone();
            let mut text = self.view_text.clone();
            match text.local_document_edits(
                &mut document,
                queued.update_id,
                applied_at_ms,
                queued.target,
                edits,
            ) {
                Ok(update) => {
                    self.prepared_edit = Some(PreparedEdit {
                        update: update.clone(),
                        document,
                        text,
                    });
                    return vec![TextClientEffect::PersistLocalUpdate { update }];
                }
                // Typing that reached a block after it was deleted or turned
                // into another kind has nothing left to change.
                Err(TextCrdtError::TargetNotFound | TextCrdtError::TargetIsNotText) => {}
                Err(error) => return self.fail(error),
            }
        }
        Vec::new()
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
            merged_update_id: update.update_id,
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

/// Replaces `own`'s blocks with `structure`'s, carrying over `own`'s text for
/// blocks it already has with the same kind.
fn merge_structure(own: &mut InkstoneDocument, structure: &InkstoneDocument) {
    let mut blocks = structure.blocks.clone();
    for block in &mut blocks {
        let Some(existing) = own.get_block(block.id) else {
            continue;
        };
        match (&mut block.content, &existing.content) {
            (BlockContent::Markdown { text }, BlockContent::Markdown { text: mine }) => {
                text.clone_from(mine)
            }
            (BlockContent::Math { source }, BlockContent::Math { source: mine }) => {
                source.clone_from(mine)
            }
            _ => {}
        }
    }
    own.blocks = blocks;
}

fn apply_plain_edit(document: &mut InkstoneDocument, target: TextTarget, edit: &TextEdit) {
    let text = match target {
        TextTarget::Title => &mut document.title,
        TextTarget::Markdown(block_id) => match document
            .blocks
            .iter_mut()
            .find(|block| block.id == block_id)
            .map(|block| &mut block.content)
        {
            Some(BlockContent::Markdown { text }) => text,
            _ => return,
        },
        TextTarget::Math(block_id) => match document
            .blocks
            .iter_mut()
            .find(|block| block.id == block_id)
            .map(|block| &mut block.content)
        {
            Some(BlockContent::Math { source }) => source,
            _ => return,
        },
    };
    let (Some(start), Some(end)) = (
        byte_index_of_utf16(text, edit.start_utf16),
        byte_index_of_utf16(text, edit.end_utf16),
    ) else {
        return;
    };
    if start <= end {
        text.replace_range(start..end, &edit.replacement);
    }
}

/// Where an offset that follows its character lands after `edit`.
fn shift_offset(offset: u32, edit: &TextEdit) -> u32 {
    if offset < edit.start_utf16 {
        offset
    } else if offset >= edit.end_utf16 {
        offset - (edit.end_utf16 - edit.start_utf16)
            + edit.replacement.encode_utf16().count() as u32
    } else {
        edit.start_utf16
    }
}

fn byte_index_of_utf16(text: &str, offset: u32) -> Option<usize> {
    let mut units = 0_u32;
    for (index, character) in text.char_indices() {
        if units == offset {
            return Some(index);
        }
        units += character.len_utf16() as u32;
    }
    (units == offset).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::doc_ops::DocumentOp;
    use inkstone_core::document::InkstoneDocument;
    use inkstone_core::ids::BlockId;

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

    fn block_edit(
        state: &mut TextClientState,
        block_id: BlockId,
        at: u32,
        text: &str,
    ) -> Vec<TextClientEffect> {
        state.transition(TextClientEvent::LocalEdit {
            target: TextTarget::Markdown(block_id),
            edit: TextEdit {
                start_utf16: at,
                end_utf16: at,
                replacement: text.into(),
            },
            update_id: UpdateId::new_v4(),
            applied_at_ms: 1,
        })
    }

    fn markdown(state: &TextClientState, block_id: BlockId) -> String {
        match &state
            .optimistic_document()
            .get_block(block_id)
            .unwrap()
            .content
        {
            BlockContent::Markdown { text } => text.clone(),
            _ => panic!("not markdown"),
        }
    }

    #[test]
    fn adopting_new_blocks_never_replays_typing_twice() {
        use inkstone_core::document::DocumentBlock;
        let doc_id = DocId::new_v4();
        let mut document = InkstoneDocument::new(doc_id);
        let first = BlockId::new_v4();
        document.blocks.push(DocumentBlock::new(
            first,
            BlockContent::Markdown {
                text: String::new(),
            },
        ));
        let mut state =
            TextClientState::new(document.clone(), TextReplicaId::new(7).unwrap()).unwrap();

        // "Key" is persisted and pending; " ideas" is still in flight.
        let persisted = match block_edit(&mut state, first, 0, "Key").as_slice() {
            [TextClientEffect::PersistLocalUpdate { update }] => update.clone(),
            _ => panic!("expected persistence"),
        };
        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: persisted.update_id,
        });
        let in_flight = match block_edit(&mut state, first, 3, " ideas").as_slice() {
            [TextClientEffect::PersistLocalUpdate { update }] => update.clone(),
            _ => panic!("expected persistence"),
        };
        block_edit(&mut state, first, 9, "!");
        assert_eq!(markdown(&state, first), "Key ideas!");

        // A block is inserted elsewhere, structure only.
        let second = BlockId::new_v4();
        let mut structure = document.clone();
        structure.blocks.push(DocumentBlock::new(
            second,
            BlockContent::Markdown {
                text: "seed".into(),
            },
        ));
        state.adopt_structure(&structure);
        assert_eq!(markdown(&state, first), "Key ideas!");
        assert_eq!(markdown(&state, second), "seed");

        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: in_flight.update_id,
        });
        assert_eq!(markdown(&state, first), "Key ideas!");
        block_edit(&mut state, second, 4, "ed");
        assert_eq!(markdown(&state, second), "seeded");
        assert_eq!(markdown(&state, first), "Key ideas!");
        assert!(state.errors().is_empty());
    }

    #[test]
    fn typing_queued_for_a_deleted_block_is_dropped_not_stuck() {
        use inkstone_core::document::DocumentBlock;
        let doc_id = DocId::new_v4();
        let mut document = InkstoneDocument::new(doc_id);
        let (keep, gone) = (BlockId::new_v4(), BlockId::new_v4());
        for id in [keep, gone] {
            document.blocks.push(DocumentBlock::new(
                id,
                BlockContent::Markdown {
                    text: String::new(),
                },
            ));
        }
        let mut state =
            TextClientState::new(document.clone(), TextReplicaId::new(8).unwrap()).unwrap();
        let in_flight = match block_edit(&mut state, keep, 0, "a").as_slice() {
            [TextClientEffect::PersistLocalUpdate { update }] => update.clone(),
            _ => panic!("expected persistence"),
        };
        block_edit(&mut state, gone, 0, "/math");
        block_edit(&mut state, keep, 1, "b");

        // The block is deleted while its typing waits behind the save.
        let mut structure = document.clone();
        structure.blocks.retain(|block| block.id != gone);
        state.adopt_structure(&structure);

        let effects = state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: in_flight.update_id,
        });
        let next = match effects.as_slice() {
            [TextClientEffect::PersistLocalUpdate { update }] => update.clone(),
            other => panic!("expected the next edit to persist, got {other:?}"),
        };
        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: next.update_id,
        });
        assert_eq!(markdown(&state, keep), "ab");
        assert!(!state.has_pending_persistence());
        assert!(state.errors().is_empty());
    }

    #[test]
    fn optimistic_document_shows_edits_queued_behind_persistence() {
        let mut state = state(1);
        let first = local_edit(&mut state, "Hé");
        for (offset, text) in [(2, "llo"), (5, " 🌍"), (0, ">")] {
            let effects = state.transition(TextClientEvent::LocalEdit {
                target: TextTarget::Title,
                edit: TextEdit {
                    start_utf16: offset,
                    end_utf16: offset,
                    replacement: text.into(),
                },
                update_id: UpdateId::new_v4(),
                applied_at_ms: 1,
            });
            assert!(effects.is_empty(), "later edits queue behind the first");
        }
        assert_eq!(state.document().title, "Hé");
        assert_eq!(state.optimistic_document().title, ">Héllo 🌍");

        let effects = state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: first.update_id,
        });
        let [TextClientEffect::PersistLocalUpdate { update: merged }] = effects.as_slice() else {
            panic!("queued edits should save as one update, got {effects:?}");
        };
        assert_eq!(state.optimistic_document().title, ">Héllo 🌍");
        let mut replay = TextClientState::new(
            InkstoneDocument::new(first.doc_id),
            TextReplicaId::new(2).unwrap(),
        )
        .unwrap();
        for update in [&first, merged] {
            for effect in replay.transition(TextClientEvent::RemoteUpdate {
                update: update.clone(),
            }) {
                if let TextClientEffect::PersistRemoteMerge { token, .. } = effect {
                    replay.transition(TextClientEvent::RemoteMergePersisted { token });
                }
            }
        }
        assert_eq!(replay.optimistic_document().title, ">Héllo 🌍");
        // "é" in the persisted "Hé" sits after ">" and "H" once the rest lands.
        let e = state
            .relative_position(TextTarget::Title, 1, TextAssociation::After)
            .unwrap();
        assert_eq!(state.resolve_position_optimistic(&e).unwrap(), 2);
    }

    #[test]
    fn typing_saved_while_an_echo_merges_is_not_lost() {
        let mut state = state(103);
        state.transition(TextClientEvent::Connected);
        let first = local_edit(&mut state, "T");
        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: first.update_id,
        });
        let merge = state.transition(TextClientEvent::RemoteUpdate {
            update: first.clone(),
        });
        let token = match merge.as_slice() {
            [TextClientEffect::PersistRemoteMerge { token, checkpoint }] => {
                assert_eq!(checkpoint.merged_update_id, first.update_id);
                *token
            }
            other => panic!("expected the echo to merge, got {other:?}"),
        };
        for text in ["r", "a"] {
            let update = local_edit(&mut state, text);
            state.transition(TextClientEvent::LocalUpdatePersisted {
                update_id: update.update_id,
            });
        }
        state.transition(TextClientEvent::RemoteMergePersisted { token });
        assert_eq!(state.document().title, "Tra");
        assert_eq!(state.pending_updates().len(), 2);
        let next = local_edit(&mut state, "n");
        state.transition(TextClientEvent::LocalUpdatePersisted {
            update_id: next.update_id,
        });
        assert_eq!(state.document().title, "Tran");
        assert!(state.errors().is_empty());
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
