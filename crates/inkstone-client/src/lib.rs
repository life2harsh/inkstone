pub mod assets;
pub mod inbox;
pub mod text_sync;

use std::collections::VecDeque;

use inkstone_core::doc_ops::{ReplayOutcome, sequenced_replay};
use inkstone_core::document::InkstoneDocument;
use inkstone_core::ids::{DocId, UpdateId};
use inkstone_core::{DocumentUpdate, new_document};
use serde::{Deserialize, Serialize};

use crate::inbox::SequencedInbox;

fn update_version(op: &DocumentOp) -> u32 {
    match op {
        DocumentOp::ApplyTextUpdateV2 { .. } | DocumentOp::ApplyInkUpdateV2 { .. } => {
            inkstone_core::doc_ops::DOCUMENT_UPDATE_V2_VERSION
        }
        _ => inkstone_core::doc_ops::DOCUMENT_UPDATE_VERSION,
    }
}

fn main_state_update_supported(update: &DocumentUpdate) -> bool {
    update.version == inkstone_core::doc_ops::DOCUMENT_UPDATE_VERSION
        || update.version == inkstone_core::doc_ops::DOCUMENT_UPDATE_V2_VERSION
            && matches!(update.op, DocumentOp::ApplyInkUpdateV2 { .. })
}

// ── Public token types ─────────────────────────────────────────────────────

/// Monotonically increasing per-document-lifecycle identifier.
/// Incremented on OpenDocument and Close so stale async completions are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LifecycleId(pub u64);

/// Identifies a specific WebSocket connection attempt.
/// Generated fresh for every ConnectSocket effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

/// Identifies a specific fetch (HTTP catch-up or gap-recovery) request.
/// Generated fresh for every FetchUpdates effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FetchId(pub u64);

/// Unique per-scheduled-reconnect.
/// Generated fresh for every ScheduleReconnect effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReconnectToken(pub u64);

/// Why a FetchUpdates effect was emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchPurpose {
    /// During CatchingUp — pulling historical updates via HTTP.
    BootstrapCatchUp,
    /// During Live — recovering a single missing seq over HTTP.
    LiveGapRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Closed,
    HydratingLocal,
    Offline,
    Connecting,
    CatchingUp,
    Live,
    Error,
}

#[derive(Debug, Clone)]
pub enum ClientEvent {
    OpenDocument {
        doc_id: DocId,
    },
    Close,
    LocalIntent {
        op: DocumentOp,
        update_id: UpdateId,
        applied_at_ms: i64,
    },
    LocalUpdatePersisted {
        update_id: UpdateId,
    },
    LocalPersistenceFailed {
        update_id: UpdateId,
    },
    RetryLocalPersistence,
    RetrySync,
    LocalHydrationLoaded {
        lifecycle: LifecycleId,
        confirmed_doc: Option<InkstoneDocument>,
        confirmed_seq: i64,
        pending_updates: Vec<DocumentUpdate>,
    },
    SocketConnected {
        lifecycle: LifecycleId,
        connection_id: ConnectionId,
    },
    SocketDisconnected {
        lifecycle: LifecycleId,
        connection_id: ConnectionId,
    },
    Ready {
        lifecycle: LifecycleId,
        connection_id: ConnectionId,
        current_seq: i64,
    },
    SequencedUpdate {
        lifecycle: LifecycleId,
        connection_id: ConnectionId,
        seq: i64,
        update: DocumentUpdate,
    },
    CatchUpPageLoaded {
        lifecycle: LifecycleId,
        fetch_id: FetchId,
        items: Vec<EncryptedUpdateMeta>,
    },
    CatchUpFailed {
        lifecycle: LifecycleId,
        fetch_id: FetchId,
        reason: String,
    },
    ConfirmationPersisted {
        token: ConfirmToken,
    },
    ConfirmationPersistenceFailed {
        token: ConfirmToken,
    },
    ReconnectTimerFired {
        lifecycle: LifecycleId,
        reconnect_token: ReconnectToken,
    },
}

#[derive(Debug, Clone)]
pub enum Effect {
    LoadLocalState {
        lifecycle: LifecycleId,
        doc_id: DocId,
    },
    PersistLocalUpdate {
        update_id: UpdateId,
        update: DocumentUpdate,
    },
    PersistConfirmation {
        token: ConfirmToken,
        checkpoint: ConfirmationPayload,
    },
    ConnectSocket {
        lifecycle: LifecycleId,
        connection_id: ConnectionId,
        doc_id: DocId,
    },
    DisconnectSocket,
    FetchUpdates {
        lifecycle: LifecycleId,
        fetch_id: FetchId,
        purpose: FetchPurpose,
        doc_id: DocId,
        after_seq: i64,
        through_seq: i64,
        limit: i64,
    },
    SendUpdate(DocumentUpdate),
    UploadSnapshot(SnapshotUpload),
    ScheduleReconnect {
        lifecycle: LifecycleId,
        reconnect_token: ReconnectToken,
        delay_ms: u64,
    },
    CancelReconnect,
    NotifyError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedUpdateMeta {
    pub seq: i64,
    pub update: DocumentUpdate,
}

#[derive(Debug, Clone)]
pub struct ConfirmationPayload {
    pub doc_id: DocId,
    pub confirmed_seq: i64,
    pub confirmed_doc: InkstoneDocument,
    pub remove_outbox_update_id: Option<UpdateId>,
    pub pending_updates_after_rebase: Vec<DocumentUpdate>,
}

#[derive(Debug, Clone)]
pub struct SnapshotUpload {
    pub doc_id: DocId,
    pub base_seq: i64,
    pub confirmed_doc: InkstoneDocument,
}

#[derive(Debug, Clone)]
pub struct PreparedLocalUpdate {
    pub update: DocumentUpdate,
    pub candidate_view_doc: InkstoneDocument,
}

#[derive(Debug, Clone)]
pub struct PendingConflict {
    pub update_id: UpdateId,
    pub outcome: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfirmToken(pub u64);

#[derive(Debug, Clone)]
struct QueuedIntent {
    op: DocumentOp,
    update_id: UpdateId,
    applied_at_ms: i64,
}

#[derive(Debug, Clone)]
struct PendingConfirmationState {
    token: ConfirmToken,
    payload: ConfirmationPayload,
    persistence_retries: u8,
    replay_conflict: Option<PendingConflict>,
}

#[derive(Debug, Clone)]
pub struct ClientState {
    phase: SessionPhase,
    doc_id: DocId,
    confirmed_doc: InkstoneDocument,
    confirmed_seq: i64,
    pending_local: Vec<DocumentUpdate>,
    view_doc: InkstoneDocument,
    inbox: SequencedInbox,
    pending_intent: Option<PreparedLocalUpdate>,
    local_persistence_failed: bool,
    intent_queue: VecDeque<QueuedIntent>,
    pending_confirmation: Option<PendingConfirmationState>,
    next_confirm_token: u64,
    pending_conflicts: Vec<PendingConflict>,
    reconnect_attempts: u32,
    errors: Vec<String>,

    // ── Token tracking ──────────────────────────────────────────────────
    lifecycle_id: LifecycleId,
    active_connection_id: Option<ConnectionId>,
    next_connection_id: u64,
    active_fetch_id: Option<FetchId>,
    active_fetch_purpose: Option<FetchPurpose>,
    active_fetch_after_seq: Option<i64>,
    active_fetch_through_seq: Option<i64>,
    active_fetch_limit: Option<i64>,
    next_fetch_id: u64,
    reconnect_token: Option<ReconnectToken>,
    next_reconnect_token: u64,

    // ── Catch-up state ──────────────────────────────────────────────────
    ready_catchup_target: Option<i64>,
}

impl ClientState {
    pub fn new(doc_id: DocId) -> Self {
        let empty = new_document(doc_id);
        Self {
            phase: SessionPhase::Closed,
            doc_id,
            view_doc: empty.clone(),
            confirmed_doc: empty,
            confirmed_seq: 0,
            pending_local: Vec::new(),
            inbox: SequencedInbox::new(),
            pending_intent: None,
            local_persistence_failed: false,
            intent_queue: VecDeque::new(),
            pending_confirmation: None,
            next_confirm_token: 0,
            pending_conflicts: Vec::new(),
            reconnect_attempts: 0,
            errors: Vec::new(),

            lifecycle_id: LifecycleId(0),
            active_connection_id: None,
            next_connection_id: 0,
            active_fetch_id: None,
            active_fetch_purpose: None,
            active_fetch_after_seq: None,
            active_fetch_through_seq: None,
            active_fetch_limit: None,
            next_fetch_id: 0,
            reconnect_token: None,
            next_reconnect_token: 0,

            ready_catchup_target: None,
        }
    }

    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    pub fn confirmed_seq(&self) -> i64 {
        self.confirmed_seq
    }

    pub fn confirmed_doc(&self) -> &InkstoneDocument {
        &self.confirmed_doc
    }

    pub fn view_doc(&self) -> &InkstoneDocument {
        &self.view_doc
    }

    /// Returns the effective view document, incorporating any in-flight
    /// local intent that has not yet been persisted. This is needed for
    /// preview rendering that must reflect the most recent edit even
    /// before the async IndexedDB write completes.
    pub fn effective_view_doc(&self) -> &InkstoneDocument {
        match &self.pending_intent {
            Some(prepared) => &prepared.candidate_view_doc,
            None => &self.view_doc,
        }
    }

    pub fn pending_local(&self) -> &[DocumentUpdate] {
        &self.pending_local
    }

    pub fn pending_conflicts(&self) -> &[PendingConflict] {
        &self.pending_conflicts
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn has_pending_intent(&self) -> bool {
        self.pending_intent.is_some()
    }

    pub fn local_persistence_failed(&self) -> bool {
        self.local_persistence_failed
    }

    pub fn intent_queue_len(&self) -> usize {
        self.intent_queue.len()
    }

    pub fn pending_confirmation_token(&self) -> Option<ConfirmToken> {
        self.pending_confirmation.as_ref().map(|pc| pc.token)
    }

    pub fn lifecycle_id(&self) -> LifecycleId {
        self.lifecycle_id
    }

    pub fn active_connection_id(&self) -> Option<ConnectionId> {
        self.active_connection_id
    }

    pub fn active_fetch_id(&self) -> Option<FetchId> {
        self.active_fetch_id
    }

    pub fn active_fetch_purpose(&self) -> Option<FetchPurpose> {
        self.active_fetch_purpose
    }

    pub fn reconnect_token(&self) -> Option<ReconnectToken> {
        self.reconnect_token
    }

    pub fn ready_catchup_target(&self) -> Option<i64> {
        self.ready_catchup_target
    }

    pub fn is_confirmation_completion_current(
        &self,
        lifecycle: LifecycleId,
        doc_id: DocId,
        token: ConfirmToken,
    ) -> bool {
        lifecycle == self.lifecycle_id
            && doc_id == self.doc_id
            && self
                .pending_confirmation
                .as_ref()
                .is_some_and(|pending| pending.token == token && pending.payload.doc_id == doc_id)
    }

    fn reconnect_delay(&self) -> u64 {
        const BASE_MS: u64 = 1000;
        const MAX_MS: u64 = 60_000;
        let delay = BASE_MS.saturating_mul(2u64.saturating_pow(self.reconnect_attempts));
        delay.min(MAX_MS)
    }

    fn clear_active_fetch(&mut self) {
        self.active_fetch_id = None;
        self.active_fetch_purpose = None;
        self.active_fetch_after_seq = None;
        self.active_fetch_through_seq = None;
        self.active_fetch_limit = None;
    }

    fn fetch_updates(
        &mut self,
        purpose: FetchPurpose,
        after_seq: i64,
        through_seq: i64,
        limit: i64,
    ) -> Effect {
        let fetch_id = FetchId(self.next_fetch_id);
        self.next_fetch_id += 1;
        self.active_fetch_id = Some(fetch_id);
        self.active_fetch_purpose = Some(purpose);
        self.active_fetch_after_seq = Some(after_seq);
        self.active_fetch_through_seq = Some(through_seq);
        self.active_fetch_limit = Some(limit);
        Effect::FetchUpdates {
            lifecycle: self.lifecycle_id,
            fetch_id,
            purpose,
            doc_id: self.doc_id,
            after_seq,
            through_seq,
            limit,
        }
    }

    fn rebuild_view(&mut self) {
        let mut doc = self.confirmed_doc.clone();
        self.pending_conflicts.clear();
        for update in &self.pending_local {
            match sequenced_replay(&mut doc, update.clone()) {
                ReplayOutcome::Fatal { .. } => {
                    self.phase = SessionPhase::Error;
                    self.errors.push("Fatal during view rebuild".into());
                    return;
                }
                ReplayOutcome::Adjusted { reason } => {
                    self.pending_conflicts.push(PendingConflict {
                        update_id: update.update_id,
                        outcome: "Adjusted".into(),
                        reason,
                    });
                }
                ReplayOutcome::NoOp { conflict } => {
                    self.pending_conflicts.push(PendingConflict {
                        update_id: update.update_id,
                        outcome: "NoOp".into(),
                        reason: conflict,
                    });
                }
                _ => {}
            }
        }
        self.view_doc = doc;
    }

    fn enter_live(&mut self) -> Vec<Effect> {
        if self.phase != SessionPhase::CatchingUp || self.active_connection_id.is_none() {
            return vec![];
        }
        self.phase = SessionPhase::Live;
        self.reconnect_attempts = 0;
        self.ready_catchup_target = None;
        self.clear_active_fetch();

        let mut effects: Vec<Effect> = Vec::new();
        for update in &self.pending_local {
            effects.push(Effect::SendUpdate(update.clone()));
        }
        effects
    }

    fn prepare_next_intent(&mut self) -> Vec<Effect> {
        while let Some(queued) = self.intent_queue.pop_front() {
            let update = DocumentUpdate {
                version: update_version(&queued.op),
                update_id: queued.update_id,
                doc_id: self.doc_id,
                applied_at_ms: queued.applied_at_ms,
                op: queued.op,
            };
            let mut candidate = self.view_doc.clone();
            match inkstone_core::doc_ops::apply_update(&mut candidate, update.clone()) {
                Ok(()) => {
                    self.pending_intent = Some(PreparedLocalUpdate {
                        candidate_view_doc: candidate,
                        update: update.clone(),
                    });
                    return vec![Effect::PersistLocalUpdate {
                        update_id: update.update_id,
                        update,
                    }];
                }
                Err(e) => {
                    self.errors.push(format!("Queued intent rejected: {}", e));
                }
            }
        }
        vec![]
    }

    fn catch_up(&mut self, ready_seq: i64) -> Vec<Effect> {
        if self.confirmed_seq >= ready_seq {
            return self.enter_live();
        }
        self.ready_catchup_target = Some(ready_seq);
        vec![self.fetch_updates(
            FetchPurpose::BootstrapCatchUp,
            self.confirmed_seq,
            ready_seq,
            500,
        )]
    }

    fn drain_inbox(&mut self) -> Vec<Effect> {
        if self.pending_confirmation.is_some() {
            return vec![];
        }

        let mut effects = Vec::new();

        while let Some((entry_seq, entry_update)) = self.inbox.take(self.confirmed_seq + 1) {
            // During CatchingUp, don't drain beyond the ready target.
            // Post-boundary entries stay buffered until we reach Live.
            if self.phase == SessionPhase::CatchingUp {
                if let Some(target) = self.ready_catchup_target {
                    if entry_seq > target {
                        break;
                    }
                }
            }

            // If a LiveGapRecovery fetch was in flight, the gap has now been
            // filled naturally by a WebSocket delivery, so clear the fetch state.
            if self.active_fetch_purpose == Some(FetchPurpose::LiveGapRecovery) {
                self.clear_active_fetch();
            }

            let was_pending = self
                .pending_local
                .iter()
                .any(|u| u.update_id == entry_update.update_id);

            let mut candidate = self.confirmed_doc.clone();
            let outcome = sequenced_replay(&mut candidate, entry_update.clone());

            let replay_conflict = match outcome {
                ReplayOutcome::Fatal { reason } => {
                    self.phase = SessionPhase::Error;
                    self.errors.push(format!("Fatal replay: {}", reason));
                    effects.clear();
                    effects.push(Effect::NotifyError(format!("Fatal replay: {}", reason)));
                    return effects;
                }
                ReplayOutcome::Adjusted { reason } => Some(PendingConflict {
                    update_id: entry_update.update_id,
                    outcome: "Adjusted remote update".into(),
                    reason,
                }),
                ReplayOutcome::NoOp { conflict } => Some(PendingConflict {
                    update_id: entry_update.update_id,
                    outcome: "No-op remote update".into(),
                    reason: conflict,
                }),
                ReplayOutcome::Applied => None,
            };
            {
                let mut new_pending_local = Vec::new();
                for pending in &self.pending_local {
                    if pending.update_id != entry_update.update_id {
                        let mut rebased = candidate.clone();
                        match sequenced_replay(&mut rebased, pending.clone()) {
                            ReplayOutcome::Fatal { .. } => continue,
                            ReplayOutcome::Adjusted { reason } => {
                                new_pending_local.push(pending.clone());
                                self.pending_conflicts.push(PendingConflict {
                                    update_id: pending.update_id,
                                    outcome: "Adjusted".into(),
                                    reason,
                                });
                            }
                            ReplayOutcome::NoOp { conflict } => {
                                new_pending_local.push(pending.clone());
                                self.pending_conflicts.push(PendingConflict {
                                    update_id: pending.update_id,
                                    outcome: "NoOp".into(),
                                    reason: conflict,
                                });
                            }
                            _ => {
                                new_pending_local.push(pending.clone());
                            }
                        }
                    }
                }

                let remove_id = if was_pending {
                    Some(entry_update.update_id)
                } else {
                    None
                };

                let payload = ConfirmationPayload {
                    doc_id: self.doc_id,
                    confirmed_seq: entry_seq,
                    confirmed_doc: candidate.clone(),
                    remove_outbox_update_id: remove_id,
                    pending_updates_after_rebase: new_pending_local.clone(),
                };

                let token = ConfirmToken(self.next_confirm_token);
                self.next_confirm_token += 1;

                effects.push(Effect::PersistConfirmation {
                    token,
                    checkpoint: payload.clone(),
                });
                self.pending_confirmation = Some(PendingConfirmationState {
                    token,
                    payload,
                    persistence_retries: 0,
                    replay_conflict,
                });
                break;
            }
        }

        effects
    }

    fn apply_confirmation(&mut self, token: ConfirmToken) -> Vec<Effect> {
        let stored = match &self.pending_confirmation {
            None => {
                if token.0 < self.next_confirm_token {
                    return vec![];
                }
                self.phase = SessionPhase::Error;
                self.errors
                    .push("ConfirmationPersisted with no pending confirmation".into());
                return vec![];
            }
            Some(pc) if pc.token != token => {
                if token.0 < pc.token.0 {
                    return vec![];
                }
                self.phase = SessionPhase::Error;
                self.errors.push(format!(
                    "Confirmation token mismatch: expected {:?}, got {:?}",
                    pc.token, token
                ));
                return vec![];
            }
            Some(_) => self.pending_confirmation.take().unwrap(),
        };

        let payload = stored.payload;

        self.inbox.remove(payload.confirmed_seq);

        self.confirmed_doc = payload.confirmed_doc;
        self.confirmed_seq = payload.confirmed_seq;
        self.pending_local = payload.pending_updates_after_rebase;
        self.rebuild_view();
        if let Some(conflict) = stored.replay_conflict {
            self.pending_conflicts.push(conflict);
        }

        let mut effects = self.drain_inbox();

        if self.phase == SessionPhase::CatchingUp && effects.is_empty() {
            if let Some(target) = self.ready_catchup_target {
                if self.confirmed_seq >= target {
                    effects.extend(self.enter_live());
                    // Drain any post-boundary inbox entries immediately
                    effects.extend(self.drain_inbox());
                } else if self.active_fetch_id.is_none() {
                    effects.push(self.fetch_updates(
                        FetchPurpose::BootstrapCatchUp,
                        self.confirmed_seq,
                        target,
                        500,
                    ));
                }
            }
        }

        effects
    }

    fn emit_connect_socket(&mut self) -> Vec<Effect> {
        let connection_id = ConnectionId(self.next_connection_id);
        self.next_connection_id += 1;
        self.active_connection_id = Some(connection_id);
        vec![Effect::ConnectSocket {
            lifecycle: self.lifecycle_id,
            connection_id,
            doc_id: self.doc_id,
        }]
    }

    pub fn transition(&mut self, event: ClientEvent) -> Vec<Effect> {
        let effects = match event {
            // ── OpenDocument: reset lifecycle and state ──────────────────
            ClientEvent::OpenDocument { doc_id } => {
                self.lifecycle_id.0 += 1;
                let lifecycle = self.lifecycle_id;
                self.doc_id = doc_id;
                self.phase = SessionPhase::HydratingLocal;
                self.confirmed_doc = new_document(doc_id);
                self.confirmed_seq = 0;
                self.pending_local = Vec::new();
                self.view_doc = new_document(doc_id);
                self.inbox.clear();
                self.pending_intent = None;
                self.local_persistence_failed = false;
                self.intent_queue.clear();
                self.pending_confirmation = None;
                self.pending_conflicts.clear();
                self.reconnect_attempts = 0;
                self.ready_catchup_target = None;
                self.clear_active_fetch();
                self.active_connection_id = None;
                self.reconnect_token = None;
                vec![
                    Effect::CancelReconnect,
                    Effect::DisconnectSocket,
                    Effect::LoadLocalState { lifecycle, doc_id },
                ]
            }

            // ── Close ────────────────────────────────────────────────────
            ClientEvent::Close => {
                self.lifecycle_id.0 += 1;
                self.phase = SessionPhase::Closed;
                self.pending_local.clear();
                self.inbox.clear();
                self.pending_intent = None;
                self.local_persistence_failed = false;
                self.intent_queue.clear();
                self.pending_confirmation = None;
                self.pending_conflicts.clear();
                self.reconnect_attempts = 0;
                self.ready_catchup_target = None;
                self.clear_active_fetch();
                self.active_connection_id = None;
                self.reconnect_token = None;
                vec![Effect::CancelReconnect, Effect::DisconnectSocket]
            }

            // ── LocalIntent ──────────────────────────────────────────────
            ClientEvent::LocalIntent {
                op,
                update_id,
                applied_at_ms,
            } => {
                let allowed_phases = [
                    SessionPhase::Offline,
                    SessionPhase::Connecting,
                    SessionPhase::CatchingUp,
                    SessionPhase::Live,
                ];
                if !allowed_phases.contains(&self.phase) {
                    self.errors.push("LocalIntent in invalid phase".into());
                    return vec![Effect::NotifyError("Cannot edit in this phase".into())];
                }

                if self.pending_intent.is_some() {
                    self.intent_queue.push_back(QueuedIntent {
                        op,
                        update_id,
                        applied_at_ms,
                    });
                    return vec![];
                }

                let update = DocumentUpdate {
                    version: update_version(&op),
                    update_id,
                    doc_id: self.doc_id,
                    applied_at_ms,
                    op,
                };

                let mut candidate = self.view_doc.clone();
                match inkstone_core::doc_ops::apply_update(&mut candidate, update.clone()) {
                    Ok(()) => {
                        self.pending_intent = Some(PreparedLocalUpdate {
                            candidate_view_doc: candidate,
                            update: update.clone(),
                        });
                        vec![Effect::PersistLocalUpdate {
                            update_id: update.update_id,
                            update,
                        }]
                    }
                    Err(e) => {
                        self.errors.push(format!("Local intent rejected: {}", e));
                        vec![Effect::NotifyError(format!("Operation rejected: {}", e))]
                    }
                }
            }

            // ── LocalUpdatePersisted: do NOT install stale candidate ────
            ClientEvent::LocalUpdatePersisted { update_id } => match &self.pending_intent {
                Some(prepared) if prepared.update.update_id == update_id => {
                    let prepared = self.pending_intent.take().unwrap();
                    self.local_persistence_failed = false;
                    let was_live = self.phase == SessionPhase::Live;
                    self.pending_local.push(prepared.update.clone());
                    self.rebuild_view();

                    let mut effects = self.prepare_next_intent();

                    if was_live {
                        effects.push(Effect::SendUpdate(prepared.update));
                    }
                    effects
                }
                _ => vec![],
            },

            // Keep the candidate visible and retryable until the exact write succeeds.
            ClientEvent::LocalPersistenceFailed { update_id } => match &self.pending_intent {
                Some(prepared) if prepared.update.update_id == update_id => {
                    self.local_persistence_failed = true;
                    vec![Effect::NotifyError(format!(
                        "Failed to persist local update {}",
                        update_id
                    ))]
                }
                _ => vec![],
            },

            ClientEvent::RetryLocalPersistence => {
                if !self.local_persistence_failed {
                    vec![]
                } else if let Some(prepared) = &self.pending_intent {
                    self.local_persistence_failed = false;
                    vec![Effect::PersistLocalUpdate {
                        update_id: prepared.update.update_id,
                        update: prepared.update.clone(),
                    }]
                } else {
                    self.local_persistence_failed = false;
                    vec![]
                }
            }

            ClientEvent::RetrySync => {
                if self.phase != SessionPhase::Offline {
                    vec![]
                } else {
                    self.reconnect_token = None;
                    self.reconnect_attempts = 0;
                    vec![Effect::CancelReconnect]
                        .into_iter()
                        .chain(self.emit_connect_socket())
                        .collect()
                }
            }

            // ── LocalHydrationLoaded ──────────────────────────────────────
            ClientEvent::LocalHydrationLoaded {
                lifecycle,
                confirmed_doc,
                confirmed_seq,
                pending_updates,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }

                let mut errors = Vec::new();

                if confirmed_seq < 0 {
                    errors.push("confirmed_seq is negative".into());
                }

                if confirmed_seq > 0 && confirmed_doc.is_none() {
                    errors.push("confirmed_seq > 0 requires confirmed_doc".into());
                }

                if let Some(d) = &confirmed_doc {
                    if d.id != self.doc_id {
                        errors.push(format!(
                            "confirmed_doc id {} does not match active doc_id {}",
                            d.id, self.doc_id
                        ));
                    }
                    if d.schema_version != inkstone_core::document::DOCUMENT_SCHEMA_VERSION {
                        errors.push(format!(
                            "unsupported document schema version {}",
                            d.schema_version
                        ));
                    }
                }

                for pu in &pending_updates {
                    if pu.doc_id != self.doc_id {
                        errors.push(format!(
                            "pending update {} has wrong doc_id {}",
                            pu.update_id, pu.doc_id
                        ));
                    }
                    if !main_state_update_supported(pu) {
                        errors.push(format!(
                            "pending update {} has unsupported version {}",
                            pu.update_id, pu.version
                        ));
                    }
                }

                if !errors.is_empty() {
                    self.phase = SessionPhase::Error;
                    for e in errors {
                        self.errors.push(e);
                    }
                    return vec![];
                }

                if let Some(d) = confirmed_doc {
                    self.confirmed_doc = d;
                    self.confirmed_seq = confirmed_seq;
                }
                self.pending_local = pending_updates;
                self.rebuild_view();

                if self.phase == SessionPhase::Error {
                    return vec![];
                }

                self.phase = SessionPhase::Offline;
                self.emit_connect_socket()
            }

            // ── SocketConnected ─────────────────────────────────────────
            ClientEvent::SocketConnected {
                lifecycle,
                connection_id,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.active_connection_id != Some(connection_id) {
                    return vec![];
                }
                self.phase = SessionPhase::Connecting;
                vec![]
            }

            // ── SocketDisconnected ──────────────────────────────────────
            ClientEvent::SocketDisconnected {
                lifecycle,
                connection_id,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.active_connection_id != Some(connection_id) {
                    return vec![];
                }
                if self.phase == SessionPhase::Closed || self.phase == SessionPhase::Error {
                    self.active_connection_id = None;
                    self.clear_active_fetch();
                    self.ready_catchup_target = None;
                    return vec![];
                }
                self.active_connection_id = None;
                self.clear_active_fetch();
                self.ready_catchup_target = None;
                self.phase = SessionPhase::Offline;
                let delay = self.reconnect_delay();
                self.reconnect_attempts += 1;
                let token = ReconnectToken(self.next_reconnect_token);
                self.next_reconnect_token += 1;
                self.reconnect_token = Some(token);
                vec![Effect::ScheduleReconnect {
                    lifecycle: self.lifecycle_id,
                    reconnect_token: token,
                    delay_ms: delay,
                }]
            }

            // ── Ready ────────────────────────────────────────────────────
            ClientEvent::Ready {
                lifecycle,
                connection_id,
                current_seq,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.active_connection_id != Some(connection_id) {
                    return vec![];
                }
                if current_seq < self.confirmed_seq {
                    self.phase = SessionPhase::Error;
                    self.errors.push(format!(
                        "Ready current_seq {} is behind local confirmed_seq {}",
                        current_seq, self.confirmed_seq
                    ));
                    return vec![];
                }
                self.phase = SessionPhase::CatchingUp;
                self.reconnect_attempts = 0;
                self.reconnect_token = None;
                self.errors
                    .retain(|error| !error.starts_with("Catch-up failed:"));
                self.catch_up(current_seq)
            }

            // ── SequencedUpdate ─────────────────────────────────────────
            ClientEvent::SequencedUpdate {
                lifecycle,
                connection_id,
                seq,
                update,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.active_connection_id != Some(connection_id) {
                    return vec![];
                }

                // Do NOT reject seq > ready_catchup_target during CatchingUp —
                // these are buffered until we reach Live.

                let fatal = self.inbox.store(seq, update.clone(), self.confirmed_seq);
                if let Some(e) = fatal {
                    self.phase = SessionPhase::Error;
                    self.errors.push(e.message);
                    return vec![];
                }

                if self.pending_confirmation.is_some() {
                    return vec![];
                }

                let effects = self.drain_inbox();
                if !effects.is_empty() {
                    return effects;
                }

                if self.phase == SessionPhase::Live && self.inbox.has_gap(self.confirmed_seq) {
                    if self.active_fetch_id.is_none() {
                        return vec![self.fetch_updates(
                            FetchPurpose::LiveGapRecovery,
                            self.confirmed_seq,
                            self.confirmed_seq + 1,
                            1,
                        )];
                    }
                }

                vec![]
            }

            // ── CatchUpPageLoaded ───────────────────────────────────────
            ClientEvent::CatchUpPageLoaded {
                lifecycle,
                fetch_id,
                items,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.active_fetch_id != Some(fetch_id) {
                    return vec![];
                }

                let purpose = match self.active_fetch_purpose {
                    Some(purpose) => purpose,
                    None => {
                        self.clear_active_fetch();
                        return vec![];
                    }
                };
                let valid_phase = match purpose {
                    FetchPurpose::BootstrapCatchUp => self.phase == SessionPhase::CatchingUp,
                    FetchPurpose::LiveGapRecovery => self.phase == SessionPhase::Live,
                };
                if !valid_phase || self.active_connection_id.is_none() {
                    self.clear_active_fetch();
                    return vec![];
                }

                let (after_seq, through_seq, limit) = match (
                    self.active_fetch_after_seq,
                    self.active_fetch_through_seq,
                    self.active_fetch_limit,
                ) {
                    (Some(after_seq), Some(through_seq), Some(limit)) => {
                        (after_seq, through_seq, limit)
                    }
                    _ => {
                        self.clear_active_fetch();
                        self.phase = SessionPhase::Error;
                        self.errors
                            .push("Active fetch is missing range metadata".into());
                        return vec![];
                    }
                };
                self.clear_active_fetch();

                if purpose == FetchPurpose::BootstrapCatchUp
                    && self.ready_catchup_target != Some(through_seq)
                {
                    self.phase = SessionPhase::Error;
                    self.errors
                        .push("Bootstrap fetch does not match the active catch-up target".into());
                    return vec![];
                }

                if items.is_empty() {
                    match purpose {
                        FetchPurpose::BootstrapCatchUp => {
                            if let Some(target) = self.ready_catchup_target {
                                if self.confirmed_seq < target {
                                    self.phase = SessionPhase::Error;
                                    self.errors
                                        .push("Empty catch-up page before reaching target".into());
                                    return vec![];
                                }
                            }
                            return vec![];
                        }
                        FetchPurpose::LiveGapRecovery => {
                            self.phase = SessionPhase::Error;
                            self.errors
                                .push("Live gap recovery returned empty page".into());
                            return vec![];
                        }
                    }
                }

                if limit <= 0 || items.len() as i64 > limit {
                    self.phase = SessionPhase::Error;
                    self.errors.push(format!(
                        "Malformed catch-up page: {} items exceeds limit {}",
                        items.len(),
                        limit
                    ));
                    return vec![];
                }

                for (index, item) in items.iter().enumerate() {
                    let expected_seq = after_seq
                        .checked_add(index as i64 + 1)
                        .filter(|seq| *seq <= through_seq);
                    if expected_seq != Some(item.seq) {
                        self.phase = SessionPhase::Error;
                        self.errors.push(format!(
                            "Malformed catch-up page: expected seq {:?}, got {}",
                            expected_seq, item.seq
                        ));
                        return vec![];
                    }
                    if item.update.doc_id != self.doc_id {
                        self.phase = SessionPhase::Error;
                        self.errors.push(format!(
                            "Catch-up update {} has wrong doc_id {}",
                            item.update.update_id, item.update.doc_id
                        ));
                        return vec![];
                    }
                    if !main_state_update_supported(&item.update) {
                        self.phase = SessionPhase::Error;
                        self.errors.push(format!(
                            "Catch-up update {} has unsupported version {}",
                            item.update.update_id, item.update.version
                        ));
                        return vec![];
                    }
                }

                let mut staged_inbox = self.inbox.clone();
                for item in &items {
                    let fatal =
                        staged_inbox.store(item.seq, item.update.clone(), self.confirmed_seq);
                    if let Some(e) = fatal {
                        self.phase = SessionPhase::Error;
                        self.errors.push(e.message);
                        return vec![];
                    }
                }
                self.inbox = staged_inbox;

                let mut effects = self.drain_inbox();
                if effects.is_empty() {
                    if let Some(target) = self.ready_catchup_target {
                        if self.confirmed_seq >= target && self.pending_confirmation.is_none() {
                            effects = self.enter_live();
                            effects.extend(self.drain_inbox());
                        } else if self.confirmed_seq < target
                            && self.pending_confirmation.is_none()
                            && self.active_fetch_id.is_none()
                        {
                            effects.push(self.fetch_updates(
                                FetchPurpose::BootstrapCatchUp,
                                self.confirmed_seq,
                                target,
                                500,
                            ));
                        }
                    }
                }
                effects
            }

            // ── CatchUpFailed ────────────────────────────────────────────
            ClientEvent::CatchUpFailed {
                lifecycle,
                fetch_id,
                reason,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.active_fetch_id != Some(fetch_id) {
                    return vec![];
                }
                self.clear_active_fetch();
                let message = format!("Catch-up failed: {}", reason);
                self.errors.push(message.clone());
                self.active_connection_id = None;
                self.ready_catchup_target = None;
                self.phase = SessionPhase::Offline;
                let delay = self.reconnect_delay();
                self.reconnect_attempts += 1;
                let token = ReconnectToken(self.next_reconnect_token);
                self.next_reconnect_token += 1;
                self.reconnect_token = Some(token);
                vec![
                    Effect::DisconnectSocket,
                    Effect::NotifyError(message),
                    Effect::ScheduleReconnect {
                        lifecycle: self.lifecycle_id,
                        reconnect_token: token,
                        delay_ms: delay,
                    },
                ]
            }

            ClientEvent::ConfirmationPersisted { token } => self.apply_confirmation(token),

            ClientEvent::ConfirmationPersistenceFailed { token } => {
                match &mut self.pending_confirmation {
                    Some(pc) if pc.token == token && pc.persistence_retries < 3 => {
                        pc.persistence_retries += 1;
                        self.errors.push("Confirmation persistence failed".into());
                        vec![Effect::PersistConfirmation {
                            token,
                            checkpoint: pc.payload.clone(),
                        }]
                    }
                    Some(pc) if pc.token == token => {
                        self.phase = SessionPhase::Error;
                        let message =
                            "Confirmation persistence failed after three retries".to_string();
                        self.errors.push(message.clone());
                        vec![Effect::NotifyError(message)]
                    }
                    _ => vec![],
                }
            }

            // ── ReconnectTimerFired ─────────────────────────────────────
            ClientEvent::ReconnectTimerFired {
                lifecycle,
                reconnect_token,
            } => {
                if lifecycle != self.lifecycle_id {
                    return vec![];
                }
                if self.phase != SessionPhase::Offline {
                    return vec![];
                }
                if self.reconnect_token != Some(reconnect_token) {
                    return vec![];
                }
                self.reconnect_token = None;
                self.emit_connect_socket()
            }
        };

        let mut auto_effects: Vec<Effect> = Vec::new();
        if self.phase == SessionPhase::Live
            && self.confirmed_seq > 0
            && self.confirmed_seq % 50 == 0
            && self.pending_local.is_empty()
        {
            auto_effects.push(Effect::UploadSnapshot(SnapshotUpload {
                doc_id: self.doc_id,
                base_seq: self.confirmed_seq,
                confirmed_doc: self.confirmed_doc.clone(),
            }));
        }

        effects.into_iter().chain(auto_effects).collect()
    }
}

pub use inkstone_core::DocumentOp;
pub use inkstone_core::doc_ops::{DOCUMENT_UPDATE_VERSION, apply_update};

#[cfg(test)]
mod tests;
