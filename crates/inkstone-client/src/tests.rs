use crate::*;
use inkstone_core::DocumentOp;
use inkstone_core::document::{BlockContent, DocumentBlock};
use inkstone_core::ids::BlockId;

fn make_doc_id() -> DocId {
    DocId::new_v4()
}

fn make_update(doc_id: DocId, op: DocumentOp) -> DocumentUpdate {
    DocumentUpdate {
        version: DOCUMENT_UPDATE_VERSION,
        update_id: UpdateId::new_v4(),
        doc_id,
        applied_at_ms: 1000,
        op,
    }
}

fn set_title(title: &str) -> DocumentOp {
    DocumentOp::SetTitle {
        title: title.into(),
    }
}

fn hydrate(state: &mut ClientState, confirmed_seq: i64) {
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: state.lifecycle_id,
        confirmed_doc: Some(new_document(state.doc_id)),
        confirmed_seq,
        pending_updates: vec![],
    });
}

fn local_intent(state: &mut ClientState, op: DocumentOp) -> UpdateId {
    let update_id = UpdateId::new_v4();
    let effects = state.transition(ClientEvent::LocalIntent {
        op,
        update_id,
        applied_at_ms: 1000,
    });
    effects
        .iter()
        .find_map(|e| match e {
            Effect::PersistLocalUpdate { update_id, .. } => Some(*update_id),
            _ => None,
        })
        .expect("Expected PersistLocalUpdate effect")
}

fn persist_local(state: &mut ClientState, update_id: UpdateId) {
    state.transition(ClientEvent::LocalUpdatePersisted { update_id });
}

fn persist_local_fail(state: &mut ClientState, update_id: UpdateId) {
    state.transition(ClientEvent::LocalPersistenceFailed { update_id });
}

fn confirm_seq(state: &mut ClientState, seq: i64) -> ConfirmToken {
    let lifecycle = state.lifecycle_id;
    let conn_id = state.active_connection_id.unwrap();
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle,
        connection_id: conn_id,
        seq,
        update: make_update(state.doc_id, set_title(&format!("seq{}", seq))),
    });
    effects
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("Expected PersistConfirmation")
}

fn confirm_persisted(state: &mut ClientState, token: ConfirmToken) -> Vec<Effect> {
    state.transition(ClientEvent::ConfirmationPersisted { token })
}

fn confirm_failed(state: &mut ClientState, token: ConfirmToken) -> Vec<Effect> {
    state.transition(ClientEvent::ConfirmationPersistenceFailed { token })
}

// ── Existing test: local intent emits persist effect ─────────────────────

#[test]
fn test_local_intent_emits_persist_effect() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    assert_eq!(state.phase(), SessionPhase::Offline);

    let effects = state.transition(ClientEvent::LocalIntent {
        op: set_title("Hello"),
        update_id: UpdateId::new_v4(),
        applied_at_ms: 1000,
    });
    assert!(state.has_pending_intent());
    assert!(state.view_doc().title.is_empty());
    let has_persist = effects
        .iter()
        .any(|e| matches!(e, Effect::PersistLocalUpdate { .. }));
    assert!(has_persist);
}

#[test]
fn test_persistence_success_commits_to_view() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let uid = local_intent(&mut state, set_title("Hello"));
    persist_local(&mut state, uid);
    assert_eq!(state.view_doc().title, "Hello");
    assert_eq!(state.pending_local().len(), 1);
    assert!(!state.has_pending_intent());
}

#[test]
fn test_persistence_failure_keeps_retryable_candidate() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let uid = local_intent(&mut state, set_title("Hello"));
    persist_local_fail(&mut state, uid);
    assert_eq!(state.view_doc().title, "");
    assert_eq!(state.effective_view_doc().title, "Hello");
    assert_eq!(state.pending_local().len(), 0);
    assert!(state.has_pending_intent());
    assert!(state.local_persistence_failed());

    let effects = state.transition(ClientEvent::RetryLocalPersistence);
    assert!(matches!(
        effects.as_slice(),
        [Effect::PersistLocalUpdate { update_id, .. }] if *update_id == uid
    ));
    assert!(!state.local_persistence_failed());
    persist_local(&mut state, uid);
    assert_eq!(state.view_doc().title, "Hello");
}

#[test]
fn test_local_update_persisted_wrong_id_rejected() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let real_uid = local_intent(&mut state, set_title("Hello"));
    persist_local(&mut state, UpdateId::new_v4());
    assert!(state.view_doc().title.is_empty());
    assert_eq!(state.pending_local().len(), 0);
    assert!(state.has_pending_intent());
    persist_local(&mut state, real_uid);
    assert_eq!(state.view_doc().title, "Hello");
    assert_eq!(state.pending_local().len(), 1);
    assert!(!state.has_pending_intent());
}

// ── Spec item 1: live-send transition ────────────────────────────────────

#[test]
fn test_live_persisted_local_edit_emits_send_update() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);
    let conn_id = state.active_connection_id.unwrap();
    let effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 5,
    });
    assert_eq!(state.phase(), SessionPhase::Live);

    let uid = local_intent(&mut state, set_title("live-edit"));
    let effects = state.transition(ClientEvent::LocalUpdatePersisted { update_id: uid });
    let sends: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::SendUpdate(u) => Some(&u.op),
            _ => None,
        })
        .collect();
    assert_eq!(sends.len(), 1);
    assert!(matches!(sends[0], DocumentOp::SetTitle { title } if title == "live-edit"));
}

#[test]
fn test_offline_persisted_local_edit_does_not_send() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);
    assert_eq!(state.phase(), SessionPhase::Offline);

    let uid = local_intent(&mut state, set_title("offline-edit"));
    let effects = state.transition(ClientEvent::LocalUpdatePersisted { update_id: uid });
    let sends: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::SendUpdate(_) => Some(()),
            _ => None,
        })
        .collect();
    assert!(sends.is_empty());
}

#[test]
fn offline_ink_update_survives_hydration_and_sends_after_reconnect() {
    let doc_id = make_doc_id();
    let block_id = BlockId::new_v4();
    let stream_epoch = uuid::Uuid::new_v4();
    let mut document = new_document(doc_id);
    document.blocks.push(DocumentBlock::new(
        block_id,
        inkstone_core::new_opaque_ink_content(640.0, 480.0, stream_epoch).unwrap(),
    ));
    let pending = DocumentUpdate {
        version: inkstone_core::DOCUMENT_UPDATE_V2_VERSION,
        update_id: UpdateId::new_v4(),
        doc_id,
        applied_at_ms: 2_000,
        op: DocumentOp::ApplyInkUpdateV2 {
            payload: inkstone_core::CollaborativeInkUpdate {
                version: inkstone_core::INK_UPDATE_PAYLOAD_VERSION,
                block_id,
                stream_epoch,
                update: Vec::new(),
                bootstrap_snapshot: None,
            },
        },
    };

    let mut state = ClientState::new(doc_id);
    state.transition(ClientEvent::OpenDocument { doc_id });
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: state.lifecycle_id(),
        confirmed_doc: Some(document),
        confirmed_seq: 5,
        pending_updates: vec![pending.clone()],
    });
    assert_eq!(state.pending_local().len(), 1);
    let connection_id = state.active_connection_id().unwrap();
    let effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id(),
        connection_id,
        current_seq: 5,
    });

    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::SendUpdate(update)
            if update.update_id == pending.update_id
                && matches!(update.op, DocumentOp::ApplyInkUpdateV2 { .. })
    )));
}

#[test]
fn test_enter_live_resends_pending_update() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);
    let uid = local_intent(&mut state, set_title("pending"));
    persist_local(&mut state, uid);
    assert_eq!(state.pending_local().len(), 1);

    let conn_id = state.active_connection_id.unwrap();
    let effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 5,
    });
    assert_eq!(state.phase(), SessionPhase::Live);
    let sends: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::SendUpdate(u) => Some(u.update_id),
            _ => None,
        })
        .collect();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0], state.pending_local()[0].update_id);
}

// ── Spec item 2: local-intent serialization ──────────────────────────────

#[test]
fn test_three_rapid_local_intents_do_not_overwrite() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    let uid1 = local_intent(&mut state, set_title("first"));
    assert_eq!(state.intent_queue_len(), 0);

    let effects2 = state.transition(ClientEvent::LocalIntent {
        op: set_title("second"),
        update_id: UpdateId::new_v4(),
        applied_at_ms: 1000,
    });
    assert!(effects2.is_empty());
    assert_eq!(state.intent_queue_len(), 1);

    let effects3 = state.transition(ClientEvent::LocalIntent {
        op: set_title("third"),
        update_id: UpdateId::new_v4(),
        applied_at_ms: 1000,
    });
    assert!(effects3.is_empty());
    assert_eq!(state.intent_queue_len(), 2);

    assert!(state.has_pending_intent());
    assert_eq!(
        state.pending_intent.as_ref().unwrap().update.update_id,
        uid1
    );
    assert!(state.view_doc().title.is_empty());

    persist_local(&mut state, uid1);
    assert_eq!(state.view_doc().title, "first");
    assert!(state.has_pending_intent());
    assert_eq!(state.intent_queue_len(), 1);

    let uid2 = state.pending_intent.as_ref().unwrap().update.update_id;
    persist_local(&mut state, uid2);
    assert_eq!(state.view_doc().title, "second");
    assert!(state.has_pending_intent());
    assert_eq!(state.intent_queue_len(), 0);

    let uid3 = state.pending_intent.as_ref().unwrap().update.update_id;
    persist_local(&mut state, uid3);
    assert_eq!(state.view_doc().title, "third");
    assert!(!state.has_pending_intent());
    assert_eq!(state.intent_queue_len(), 0);
}

#[test]
fn test_stale_local_persistence_cannot_clear_current_work() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    let uid1 = local_intent(&mut state, set_title("first"));
    let stale_uid = UpdateId::new_v4();

    state.transition(ClientEvent::LocalUpdatePersisted {
        update_id: stale_uid,
    });
    assert!(state.has_pending_intent());
    assert!(state.view_doc().title.is_empty());

    persist_local(&mut state, uid1);
    assert_eq!(state.view_doc().title, "first");
}

// ── Spec item 3: token-based completions ─────────────────────────────────

#[test]
fn test_confirmation_uses_internal_candidate_not_adapter_supplied() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);

    let token = confirm_seq(&mut state, 11);
    let effects = confirm_persisted(&mut state, token);
    assert_eq!(state.confirmed_seq(), 11);
    assert_eq!(state.view_doc().title, "seq11");
    let has_confirm = effects
        .iter()
        .any(|e| matches!(e, Effect::PersistConfirmation { .. }));
    assert!(!has_confirm);
}

// ── Spec item 4: deterministic metadata ──────────────────────────────────

#[test]
fn test_same_state_same_local_intent_deterministic() {
    let doc_id = make_doc_id();
    let mut state_a = ClientState::new(doc_id);
    hydrate(&mut state_a, 0);
    let mut state_b = state_a.clone();

    let uid = UpdateId::new_v4();
    let effects_a = state_a.transition(ClientEvent::LocalIntent {
        op: set_title("det"),
        update_id: uid,
        applied_at_ms: 5000,
    });
    let effects_b = state_b.transition(ClientEvent::LocalIntent {
        op: set_title("det"),
        update_id: uid,
        applied_at_ms: 5000,
    });

    assert_eq!(
        state_a.pending_intent.as_ref().map(|p| p.update.update_id),
        state_b.pending_intent.as_ref().map(|p| p.update.update_id)
    );
    assert_eq!(state_a.view_doc().title, state_b.view_doc().title);

    let update_a = effects_a
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistLocalUpdate { update, .. } => Some(update),
            _ => None,
        })
        .unwrap();
    let update_b = effects_b
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistLocalUpdate { update, .. } => Some(update),
            _ => None,
        })
        .unwrap();

    assert_eq!(update_a.update_id, update_b.update_id);
    assert_eq!(update_a.applied_at_ms, update_b.applied_at_ms);
    assert_eq!(format!("{:?}", update_a.op), format!("{:?}", update_b.op));
}

// ── Spec item 5: multi-page catch-up ────────────────────────────────────

#[test]
fn test_multi_page_catch_up_emits_next_fetch() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 1200,
    });
    let fetch_id = state.active_fetch_id.unwrap();

    let effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: (1..=500)
            .map(|seq| EncryptedUpdateMeta {
                seq,
                update: make_update(doc_id, set_title(&format!("s{}", seq))),
            })
            .collect(),
    });

    let mut tokens = effects.into_iter().filter_map(|e| match e {
        Effect::PersistConfirmation { token, .. } => Some(token),
        _ => None,
    });
    let first_token = tokens.next().expect("Expected PersistConfirmation");
    assert!(
        tokens.next().is_none(),
        "only one PersistConfirmation per drain"
    );

    let mut effects = confirm_persisted(&mut state, first_token);
    while state.confirmed_seq < 500 && state.phase() != SessionPhase::Error {
        let next_token = effects.clone().into_iter().find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        });
        match next_token {
            Some(tok) => effects = confirm_persisted(&mut state, tok),
            None => break,
        }
    }

    assert_eq!(
        state.confirmed_seq(),
        500,
        "should have confirmed through seq 500"
    );
    assert_eq!(state.phase(), SessionPhase::CatchingUp);

    let fetches: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::FetchUpdates {
                after_seq,
                through_seq,
                ..
            } => Some((*after_seq, *through_seq)),
            _ => None,
        })
        .collect();
    assert_eq!(
        fetches.len(),
        1,
        "should emit next FetchUpdates after all seqs confirmed"
    );
    assert_eq!(fetches[0].0, 500);
    assert_eq!(fetches[0].1, 1200);
}

#[test]
fn test_multi_page_catch_up_continues_with_future_live_update_buffered() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    let connection_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id,
        current_seq: 501,
    });
    let fetch_id = state.active_fetch_id.unwrap();
    assert!(
        state
            .transition(ClientEvent::SequencedUpdate {
                lifecycle: state.lifecycle_id,
                connection_id,
                seq: 502,
                update: make_update(doc_id, set_title("future-live")),
            })
            .is_empty()
    );

    let effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: (1..=500)
            .map(|seq| EncryptedUpdateMeta {
                seq,
                update: make_update(doc_id, set_title(&format!("s{seq}"))),
            })
            .collect(),
    });
    let first_token = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("expected first confirmation");

    let mut effects = confirm_persisted(&mut state, first_token);
    while state.confirmed_seq() < 500 {
        let token = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::PersistConfirmation { token, .. } => Some(*token),
                _ => None,
            })
            .expect("expected contiguous confirmation");
        effects = confirm_persisted(&mut state, token);
    }

    assert_eq!(state.phase(), SessionPhase::CatchingUp);
    assert_eq!(state.confirmed_seq(), 500);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::FetchUpdates {
            purpose: FetchPurpose::BootstrapCatchUp,
            after_seq: 500,
            through_seq: 501,
            ..
        }
    )));
}

#[test]
fn test_empty_catch_up_page_before_target_is_failure() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);

    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });
    let fetch_id = state.active_fetch_id.unwrap();

    let _effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: vec![],
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

#[test]
fn catch_up_http_failure_is_offline_retryable_and_reconnects() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);
    let connection_id = state.active_connection_id().unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id(),
        connection_id,
        current_seq: 10,
    });
    let fetch_id = state.active_fetch_id().unwrap();

    let effects = state.transition(ClientEvent::CatchUpFailed {
        lifecycle: state.lifecycle_id(),
        fetch_id,
        reason: "network unavailable".into(),
    });

    assert_eq!(state.phase(), SessionPhase::Offline);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::ScheduleReconnect { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::DisconnectSocket))
    );

    let effects = state.transition(ClientEvent::RetrySync);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::ConnectSocket { .. }))
    );
}

#[test]
fn sequenced_structural_noop_remains_visible_after_confirmation() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let connection_id = state.active_connection_id().unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id(),
        connection_id,
        current_seq: 0,
    });
    let update = make_update(
        doc_id,
        DocumentOp::DeleteBlock {
            block_id: BlockId::new_v4(),
        },
    );
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id(),
        connection_id,
        seq: 1,
        update,
    });
    let token = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .unwrap();

    state.transition(ClientEvent::ConfirmationPersisted { token });

    assert!(state.pending_conflicts().iter().any(|conflict| {
        conflict.outcome == "No-op remote update" && conflict.reason.contains("not found")
    }));
    assert_eq!(state.phase(), SessionPhase::Live);
}

#[test]
fn successful_reconnect_clears_stale_catch_up_error() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let connection_id = state.active_connection_id().unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id(),
        connection_id,
        current_seq: 1,
    });
    let fetch_id = state.active_fetch_id().unwrap();
    state.transition(ClientEvent::CatchUpFailed {
        lifecycle: state.lifecycle_id(),
        fetch_id,
        reason: "temporary".into(),
    });
    state.transition(ClientEvent::RetrySync);
    let reconnected = state.active_connection_id().unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id(),
        connection_id: reconnected,
        current_seq: 0,
    });

    assert!(
        state
            .errors()
            .iter()
            .all(|error| !error.starts_with("Catch-up failed:"))
    );
}

// ── Spec item 6: live gap recovery ───────────────────────────────────────

#[test]
fn test_live_seq_gap_triggers_one_recovery_fetch() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });
    assert_eq!(state.phase(), SessionPhase::Live);

    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 12,
        update: make_update(doc_id, set_title("twelve")),
    });
    let fetches: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::FetchUpdates {
                after_seq,
                through_seq,
                ..
            } => Some((*after_seq, *through_seq)),
            _ => None,
        })
        .collect();
    assert_eq!(fetches.len(), 1);
    assert_eq!(fetches[0], (10, 11));
    assert_eq!(
        state.active_fetch_purpose,
        Some(FetchPurpose::LiveGapRecovery)
    );
}

#[test]
fn test_duplicate_gap_seq_does_not_trigger_another_fetch() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });

    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 12,
        update: make_update(doc_id, set_title("twelve")),
    });

    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 13,
        update: make_update(doc_id, set_title("thirteen")),
    });
    let fetches: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::FetchUpdates { .. } => Some(()),
            _ => None,
        })
        .collect();
    assert!(
        fetches.is_empty(),
        "should not emit another fetch for same gap"
    );
}

#[test]
fn test_recovered_missing_seq_drains_buffered_future() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });

    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 12,
        update: make_update(doc_id, set_title("twelve")),
    });
    assert_eq!(
        state.active_fetch_purpose,
        Some(FetchPurpose::LiveGapRecovery)
    );

    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update: make_update(doc_id, set_title("eleven")),
    });
    let token = effects
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("Expected PersistConfirmation for seq 11");
    assert_eq!(state.active_fetch_purpose, None);

    let effects2 = confirm_persisted(&mut state, token);
    assert_eq!(state.confirmed_seq(), 11);
    let token_12 = effects2
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("Expected follow-up PersistConfirmation for seq 12");

    let _effects3 = confirm_persisted(&mut state, token_12);
    assert_eq!(state.confirmed_seq(), 12);
    assert_eq!(state.view_doc().title, "twelve");
}

// ── Spec item 7: lifecycle generation ───────────────────────────────────

#[test]
fn test_stale_socket_disconnected_after_close_does_nothing() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let stale_lifecycle = state.lifecycle_id;
    let stale_conn_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::Close);

    let effects = state.transition(ClientEvent::SocketDisconnected {
        lifecycle: stale_lifecycle,
        connection_id: stale_conn_id,
    });
    assert_eq!(state.phase(), SessionPhase::Closed);
    let has_reconnect = effects
        .iter()
        .any(|e| matches!(e, Effect::ScheduleReconnect { .. }));
    assert!(!has_reconnect);
}

#[test]
fn test_stale_reconnect_timer_after_close_does_nothing() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
    });
    let stale_lifecycle = state.lifecycle_id;
    let stale_token = state.reconnect_token.unwrap();
    state.transition(ClientEvent::Close);

    let effects = state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: stale_lifecycle,
        reconnect_token: stale_token,
    });
    assert_eq!(state.phase(), SessionPhase::Closed);
    let has_connect = effects
        .iter()
        .any(|e| matches!(e, Effect::ConnectSocket { .. }));
    assert!(!has_connect);
}

#[test]
fn test_stale_hydration_from_old_lifecycle_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    state.transition(ClientEvent::OpenDocument {
        doc_id: make_doc_id(),
    });
    let gen_a = state.lifecycle_id;

    state.transition(ClientEvent::Close);
    state.transition(ClientEvent::OpenDocument { doc_id });

    let effects = state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: gen_a,
        confirmed_doc: Some(new_document(doc_id)),
        confirmed_seq: 5,
        pending_updates: vec![],
    });
    assert_eq!(
        state.phase(),
        SessionPhase::HydratingLocal,
        "should stay HydratingLocal because stale hydration was ignored"
    );
    assert!(effects.is_empty());
}

#[test]
fn test_stale_catch_up_page_from_previous_connection_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 5,
    });
    let stale_lifecycle = state.lifecycle_id;
    let stale_fetch_id = state.active_fetch_id.unwrap();
    state.transition(ClientEvent::Close);

    state.transition(ClientEvent::OpenDocument { doc_id });
    let effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: stale_lifecycle,
        fetch_id: stale_fetch_id,
        items: vec![EncryptedUpdateMeta {
            seq: 1,
            update: make_update(doc_id, set_title("stale")),
        }],
    });
    assert!(effects.is_empty());
}

// ── Spec item 8: hydration validation ───────────────────────────────────

#[test]
fn test_wrong_doc_local_checkpoint_rejected() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    let wrong_doc = new_document(make_doc_id());
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: state.lifecycle_id,
        confirmed_doc: Some(wrong_doc),
        confirmed_seq: 3,
        pending_updates: vec![],
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

#[test]
fn test_unsupported_schema_checkpoint_rejected() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    let mut bad = new_document(doc_id);
    bad.schema_version = 99;
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: state.lifecycle_id,
        confirmed_doc: Some(bad),
        confirmed_seq: 0,
        pending_updates: vec![],
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

#[test]
fn test_negative_confirmed_seq_rejected() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: state.lifecycle_id,
        confirmed_doc: Some(new_document(doc_id)),
        confirmed_seq: -1,
        pending_updates: vec![],
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

// ── Spec item 9: server behind local ────────────────────────────────────

#[test]
fn test_ready_behind_local_confirmed_seq_rejected() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 50);

    let conn_id = state.active_connection_id.unwrap();
    let effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 40,
    });
    assert_eq!(state.phase(), SessionPhase::Error);
    let has_send = effects.iter().any(|e| matches!(e, Effect::SendUpdate(_)));
    assert!(!has_send);
}

// ── Spec item 10: snapshot contract (FetchSnapshot removed) ─────────────

#[test]
fn test_no_fetch_snapshot_effect_defined() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 49);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 49,
    });
    let payload = confirm_seq(&mut state, 50);
    let effects = confirm_persisted(&mut state, payload);
    assert_eq!(state.confirmed_seq(), 50);

    let has_fetch_snapshot = effects
        .iter()
        .any(|e| matches!(e, Effect::FetchUpdates { .. }));
    assert!(
        !has_fetch_snapshot,
        "should not emit FetchUpdates during snapshot cycle"
    );

    let snapshots: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::UploadSnapshot(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(snapshots.len(), 1);
}

// ── Spec item 11: reconnect guards ──────────────────────────────────────

#[test]
fn test_socket_disconnected_in_closed_does_nothing() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    state.transition(ClientEvent::Close);

    let effects = state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: ConnectionId(0),
    });
    let has_reconnect = effects
        .iter()
        .any(|e| matches!(e, Effect::ScheduleReconnect { .. }));
    assert!(!has_reconnect);
}

#[test]
fn test_reconnect_timer_fired_offline_only() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    assert_eq!(state.phase(), SessionPhase::Offline);

    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::SocketConnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
    });
    assert_eq!(state.phase(), SessionPhase::Connecting);

    let effects = state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: ReconnectToken(0),
    });
    let has_connect = effects
        .iter()
        .any(|e| matches!(e, Effect::ConnectSocket { .. }));
    assert!(!has_connect, "should not connect when not Offline");
}

// ── Existing tests preserved ────────────────────────────────────────────

#[test]
fn test_pending_insert_then_edit() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let block = markdown_block("hello");
    let uid1 = local_intent(
        &mut state,
        DocumentOp::InsertBlock {
            idx: 0,
            block: block.clone(),
        },
    );
    persist_local(&mut state, uid1);
    assert_eq!(state.view_doc().blocks.len(), 1);
    let uid2 = local_intent(
        &mut state,
        DocumentOp::ReplaceBlockContent {
            block_id: block.id,
            content: BlockContent::Markdown {
                text: "world".into(),
            },
        },
    );
    persist_local(&mut state, uid2);
    assert_eq!(state.view_doc().blocks.len(), 1);
    match &state.view_doc().blocks[0].content {
        BlockContent::Markdown { text } => assert_eq!(text, "world"),
        _ => panic!("wrong content"),
    }
}

#[test]
fn test_local_hydration_reconstructs_view() {
    let doc_id = make_doc_id();
    let mut doc = new_document(doc_id);
    doc.title = "Existing".into();
    let update = make_update(doc_id, set_title("Pending"));
    let mut state = ClientState::new(doc_id);
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: state.lifecycle_id,
        confirmed_doc: Some(doc),
        confirmed_seq: 5,
        pending_updates: vec![update],
    });
    assert_eq!(state.confirmed_seq(), 5);
    assert_eq!(state.confirmed_doc().title, "Existing");
    assert_eq!(state.view_doc().title, "Pending");
    assert_eq!(state.pending_local().len(), 1);
}

#[test]
fn test_offline_open_is_editable() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    state.transition(ClientEvent::OpenDocument { doc_id });
    assert_eq!(state.phase(), SessionPhase::HydratingLocal);
    hydrate(&mut state, 0);
    assert_eq!(state.phase(), SessionPhase::Offline);
    let uid = local_intent(&mut state, set_title("Offline edit"));
    persist_local(&mut state, uid);
    assert_eq!(state.view_doc().title, "Offline edit");
}

#[test]
fn test_confirmation_chains_follow_up_effects() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let u12 = make_update(doc_id, set_title("twelve"));
    let u11 = make_update(doc_id, set_title("eleven"));
    let _effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 12,
        update: u12.clone(),
    });
    assert_eq!(state.confirmed_seq(), 10);
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update: u11.clone(),
    });
    let token11 = effects
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("Expected PersistConfirmation(seq 11)");
    let follow_up = confirm_persisted(&mut state, token11);
    assert_eq!(state.confirmed_seq(), 11);
    let token12 = follow_up
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("Expected follow-up PersistConfirmation(seq 12)");
    let _final = confirm_persisted(&mut state, token12);
    assert_eq!(state.confirmed_seq(), 12);
    assert_eq!(state.view_doc().title, "twelve");
}

#[test]
fn test_stale_confirmation_token_rejected() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let _token = confirm_seq(&mut state, 11);
    let effects = confirm_persisted(&mut state, ConfirmToken(999));
    assert_eq!(state.phase(), SessionPhase::Error);
    assert_eq!(state.confirmed_seq(), 10);
    assert!(effects.is_empty());
}

#[test]
fn test_confirmation_failure_retries_exact_token() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let token = confirm_seq(&mut state, 11);
    let retry_effects = confirm_failed(&mut state, token);
    assert_eq!(state.confirmed_seq(), 10);
    let has_retry = retry_effects.iter().any(|e| match e {
        Effect::PersistConfirmation { token: t, .. } => *t == token,
        _ => false,
    });
    assert!(has_retry);
    let _ok = confirm_persisted(&mut state, token);
    assert_eq!(state.confirmed_seq(), 11);
}

#[test]
fn test_confirmation_failure_stops_after_three_retries() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let token = confirm_seq(&mut state, 11);

    for _ in 0..3 {
        assert!(
            confirm_failed(&mut state, token)
                .iter()
                .any(|effect| matches!(effect, Effect::PersistConfirmation { .. }))
        );
    }
    let final_effects = confirm_failed(&mut state, token);

    assert_eq!(state.phase(), SessionPhase::Error);
    assert!(
        final_effects
            .iter()
            .any(|effect| matches!(effect, Effect::NotifyError(_)))
    );
}

#[test]
fn test_fetch_updates_includes_through_seq() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);
    let conn_id = state.active_connection_id.unwrap();
    let effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });
    let fetches: Vec<_> = effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::FetchUpdates {
                after_seq,
                through_seq,
                ..
            } => Some((after_seq, through_seq)),
            _ => None,
        })
        .collect();
    assert_eq!(fetches, vec![(5, 10)]);
}

// ── Spec 3: Ready boundary — WS seq > target buffers, not errors ────────

#[test]
fn test_ready_ws_gt_target_buffers_not_errors() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 5);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });
    // WS delivers seq 11, which is > target 10 — should NOT error
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update: make_update(doc_id, set_title("beyond")),
    });
    assert_eq!(
        state.phase(),
        SessionPhase::CatchingUp,
        "should stay CatchingUp, not error"
    );
    // No PersistConfirmation because 11 > target 10, buffered
    let has_confirm = effects
        .iter()
        .any(|e| matches!(e, Effect::PersistConfirmation { .. }));
    assert!(!has_confirm, "seq 11 should be buffered, not confirmed");

    // Now confirm through target 10 via CatchUpPageLoaded
    let fetch_id = state.active_fetch_id.unwrap();
    let page_effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: (6..=10)
            .map(|seq| EncryptedUpdateMeta {
                seq,
                update: make_update(doc_id, set_title(&format!("s{}", seq))),
            })
            .collect(),
    });

    // drain_inbox confirms seq 6, need to chain through to 10
    let mut effects = page_effects;
    while state.confirmed_seq < 10 && state.phase() != SessionPhase::Error {
        let token = effects.clone().into_iter().find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        });
        match token {
            Some(tok) => effects = confirm_persisted(&mut state, tok),
            None => break,
        }
    }
    assert_eq!(state.confirmed_seq(), 10, "should confirm through target");
    assert_eq!(state.phase(), SessionPhase::Live);

    // Now the buffered seq 11 should be confirmed
    // (drain_inbox after enter_live picks it up)
    let token_11 = effects.into_iter().find_map(|e| match e {
        Effect::PersistConfirmation { token, .. } => Some(token),
        _ => None,
    });
    // If token_11 was returned from the confirmation that entered Live,
    // confirm it now. Otherwise drain already handled it.
    if let Some(tok) = token_11 {
        confirm_persisted(&mut state, tok);
    }
    // We may need one more drain cycle — check if 11 still needs confirmation
    if state.confirmed_seq < 11 {
        let final_effects = confirm_persisted(&mut state, ConfirmToken(999));
        // This will error from stale token, but that's expected.
        // Instead check that inbox has a pending_confirmation draining.
        // Let's just assert that the next drain picks up seq 11.
        // Since we're Live and confirmed_seq=10, the next drain should take 11.
        if state.pending_confirmation.is_none() {
            // Force a drain: feed a dummy WS event to trigger drain_inbox
            let more = state.transition(ClientEvent::SequencedUpdate {
                lifecycle: state.lifecycle_id,
                connection_id: conn_id,
                seq: 13,
                update: make_update(doc_id, set_title("dummy")),
            });
            let tok13 = more.into_iter().find_map(|e| match e {
                Effect::PersistConfirmation { token, .. } => Some(token),
                _ => None,
            });
            if let Some(tok) = tok13 {
                confirm_persisted(&mut state, tok);
            }
        }
    }
    assert_eq!(
        state.confirmed_seq(),
        11,
        "seq 11 should eventually confirm"
    );
    assert_eq!(state.view_doc().title, "beyond");
}

// ── Spec 4: queued-intent double-pop fix ─────────────────────────────────

#[test]
fn test_invalid_queued_b_then_valid_c_produces_c() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    // A — succeeds
    let uid_a = local_intent(&mut state, set_title("a"));

    // B — invalid (title already "a" — both set_title ops succeed, but let's
    // use delete on a non-existent block to cause rejection)
    let missing_id = BlockId::new_v4();
    state.transition(ClientEvent::LocalIntent {
        op: DocumentOp::DeleteBlock {
            block_id: missing_id,
        },
        update_id: UpdateId::new_v4(),
        applied_at_ms: 1000,
    });
    assert_eq!(state.intent_queue_len(), 1);

    // C — valid
    state.transition(ClientEvent::LocalIntent {
        op: set_title("c"),
        update_id: UpdateId::new_v4(),
        applied_at_ms: 1000,
    });
    assert_eq!(state.intent_queue_len(), 2);

    // Persist A — pending_intent consumed, prepare_next_intent runs
    persist_local(&mut state, uid_a);
    assert_eq!(state.view_doc().title, "a");

    // B was rejected (double-pop no longer happens)
    // C should now be pending_intent
    assert!(state.has_pending_intent());
    let uid_c = state.pending_intent.as_ref().unwrap().update.update_id;
    persist_local(&mut state, uid_c);
    assert_eq!(state.view_doc().title, "c");
}

// ── Spec 5: stale persistence failure handling ───────────────────────────

#[test]
fn test_stale_persistence_failure_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    let uid = local_intent(&mut state, set_title("hello"));
    let stale_uid = UpdateId::new_v4();

    // Stale failure does NOT clear pending_intent
    state.transition(ClientEvent::LocalPersistenceFailed {
        update_id: stale_uid,
    });
    assert!(state.has_pending_intent(), "pending_intent should survive");

    // Real failure retains the correlated candidate for retry.
    persist_local_fail(&mut state, uid);
    assert!(state.has_pending_intent());
    assert!(state.local_persistence_failed());
}

#[test]
fn test_current_persistence_failure_retries_before_queued_intent() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);

    // A — in flight (pending_intent)
    let uid_a = local_intent(&mut state, set_title("a"));
    // B — queued
    state.transition(ClientEvent::LocalIntent {
        op: set_title("b"),
        update_id: UpdateId::new_v4(),
        applied_at_ms: 1000,
    });
    assert_eq!(state.intent_queue_len(), 1);

    // A fails — surface the error, but do not reorder B ahead of retrying A.
    let effects = state.transition(ClientEvent::LocalPersistenceFailed { update_id: uid_a });
    let has_error = effects.iter().any(|e| matches!(e, Effect::NotifyError(_)));
    assert!(has_error, "should surface persistence failure");
    assert!(state.has_pending_intent(), "A should remain pending_intent");
    assert_eq!(state.intent_queue_len(), 1);

    let retry = state.transition(ClientEvent::RetryLocalPersistence);
    assert!(matches!(
        retry.as_slice(),
        [Effect::PersistLocalUpdate { update_id, .. }] if *update_id == uid_a
    ));
    persist_local(&mut state, uid_a);
    assert_eq!(state.view_doc().title, "a");
    assert!(
        state.has_pending_intent(),
        "B should be prepared after A persists"
    );
    assert_eq!(state.intent_queue_len(), 0);
}

// ── Spec 6: concurrent remote/local persistence ──────────────────────────

#[test]
fn test_concurrent_remote_confirmation_before_local_persist() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });
    assert_eq!(state.phase(), SessionPhase::Live);

    // Local intent starts persistence (pending_intent set)
    let uid = local_intent(&mut state, set_title("local-edit"));

    // Remote server confirms an update before local persistence completes
    let token11 = confirm_seq(&mut state, 11);
    confirm_persisted(&mut state, token11);
    assert_eq!(state.confirmed_seq(), 11);
    // confirmed_doc now has title seq11
    // But view_doc should NOT yet have "local-edit" (it's still pending_intent)

    // Now local persistence succeeds (was started BEFORE remote confirmation)
    let effects = persist_local(&mut state, uid);
    // view must contain BOTH remote confirmed state AND local pending effect
    // confirmed_doc has title "seq11", local pending has title "local-edit"
    // So view should have "local-edit" (last write wins)
    assert_eq!(state.view_doc().title, "local-edit");
    assert_eq!(state.pending_local().len(), 1);
}

// ── Spec 7: fetch purpose — empty page handling ─────────────────────────

#[test]
fn test_gap_recovery_empty_page_failure() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });

    // Trigger a gap
    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 12,
        update: make_update(doc_id, set_title("twelve")),
    });
    let fetch_id = state.active_fetch_id.unwrap();

    // Gap recovery returns empty → error
    state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: vec![],
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

// ── Spec 8: reconnect token model ───────────────────────────────────────

#[test]
fn test_reconnect_token_mismatch_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();

    // Socket disconnects → ScheduleReconnect with unique token
    let effects = state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
    });
    let real_token = state.reconnect_token.unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ScheduleReconnect { .. }))
    );

    // Wrong token → ignored
    let wrong_effects = state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: ReconnectToken(999),
    });
    assert!(
        !wrong_effects
            .iter()
            .any(|e| matches!(e, Effect::ConnectSocket { .. }))
    );

    // Correct token → connect
    let correct_effects = state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: real_token,
    });
    assert!(
        correct_effects
            .iter()
            .any(|e| matches!(e, Effect::ConnectSocket { .. }))
    );
    // Token consumed (state.reconnect_token should be None)
    assert_eq!(state.reconnect_token, None);
    assert_eq!(state.phase(), SessionPhase::Offline);

    // Duplicate firing with consumed token → ignored
    let dup_effects = state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: real_token,
    });
    assert!(
        !dup_effects
            .iter()
            .any(|e| matches!(e, Effect::ConnectSocket { .. }))
    );
}

#[test]
fn test_reconnect_timer_fired_wrong_phase_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();

    // SocketDisconnected → Offline, timer scheduled
    state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
    });
    let token = state.reconnect_token.unwrap();

    // Fire the timer to get a new connection_id and enter Connecting
    state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: token,
    });
    let new_conn = state.active_connection_id.unwrap();
    state.transition(ClientEvent::SocketConnected {
        lifecycle: state.lifecycle_id,
        connection_id: new_conn,
    });
    assert_eq!(state.phase(), SessionPhase::Connecting);

    // While Connecting, another ReconnectTimerFired is ignored
    let effects = state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: token,
    });
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::ConnectSocket { .. }))
    );
}

// ── Spec 9: connection identity ─────────────────────────────────────────

#[test]
fn test_stale_connection_sequenced_update_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_a_id = state.active_connection_id.unwrap();

    // Connect A, with confirmed_seq=0 and ready_seq=5 to enter CatchingUp
    state.transition(ClientEvent::SocketConnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_a_id,
    });
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_a_id,
        current_seq: 5,
    });
    assert_eq!(state.phase(), SessionPhase::CatchingUp);

    // A disconnects → B connects with new connection_id
    state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_a_id,
    });
    let token = state.reconnect_token.unwrap();
    state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: token,
    });
    let conn_b_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::SocketConnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_b_id,
    });

    // Late SequencedUpdate from A → ignored
    assert!(state.phase() == SessionPhase::Connecting || state.phase() == SessionPhase::CatchingUp);
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_a_id, // stale!
        seq: 5,
        update: make_update(doc_id, set_title("stale")),
    });
    // No effect, no phase change
    assert!(effects.is_empty());
    assert!(state.phase() != SessionPhase::Error);
}

#[test]
fn test_late_ready_from_old_connection_ignored() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_a_id = state.active_connection_id.unwrap();

    // A disconnects
    state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_a_id,
    });
    let token = state.reconnect_token.unwrap();

    // Late Ready from A → ignored
    let effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_a_id,
        current_seq: 5,
    });
    assert!(effects.is_empty());
    assert_eq!(state.phase(), SessionPhase::Offline);

    // B connects properly
    state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: token,
    });
    let conn_b_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::SocketConnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_b_id,
    });
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_b_id,
        current_seq: 5,
    });
    assert_eq!(state.phase(), SessionPhase::CatchingUp);
}

// ── Spec 10: confirmed update-ID history ────────────────────────────────

#[test]
fn test_duplicate_update_id_across_seqs_fatal() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();

    // Manually confirm seq 11 with a specific update_id
    let original_update = make_update(doc_id, set_title("first"));
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update: original_update.clone(),
    });
    let token = effects
        .into_iter()
        .find_map(|e| match e {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .expect("Expected PersistConfirmation");
    confirm_persisted(&mut state, token);
    assert_eq!(state.confirmed_seq(), 11);

    // Same update_id arrives at seq 15 → fatal consistency error
    let dup = DocumentUpdate {
        update_id: original_update.update_id,
        ..make_update(doc_id, set_title("conflict"))
    };
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 15,
        update: dup,
    });
    assert_eq!(state.phase(), SessionPhase::Error);
    // Error should mention confirmed_history
    assert!(state.errors().iter().any(|e| e.contains("confirmed")));
}

// ── Spec 11 (remains): reconnect backoff ─────────────────────────────────

fn reconnect_cycle(state: &mut ClientState) {
    // After SocketDisconnected, fire the reconnect timer to get a new connection_id
    let token = state.reconnect_token.unwrap();
    state.transition(ClientEvent::ReconnectTimerFired {
        lifecycle: state.lifecycle_id,
        reconnect_token: token,
    });
    let new_conn = state.active_connection_id.unwrap();
    state.transition(ClientEvent::SocketConnected {
        lifecycle: state.lifecycle_id,
        connection_id: new_conn,
    });
}

#[test]
fn test_reconnect_backoff_increments() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();

    let e1 = state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
    });
    let d1: u64 = e1
        .iter()
        .find_map(|e| match e {
            Effect::ScheduleReconnect { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
        .unwrap();
    assert_eq!(d1, 1000);

    reconnect_cycle(&mut state);

    let conn_id2 = state.active_connection_id.unwrap();
    let e2 = state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id2,
    });
    let d2: u64 = e2
        .iter()
        .find_map(|e| match e {
            Effect::ScheduleReconnect { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
        .unwrap();
    assert_eq!(d2, 2000);

    reconnect_cycle(&mut state);

    let conn_id3 = state.active_connection_id.unwrap();
    let e3 = state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id3,
    });
    let d3: u64 = e3
        .iter()
        .find_map(|e| match e {
            Effect::ScheduleReconnect { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
        .unwrap();
    assert_eq!(d3, 4000);
}

// ── Existing tests: inbox consistency ────────────────────────────────────

#[test]
fn test_same_seq_different_update_id_is_fatal() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();
    let u1 = make_update(doc_id, set_title("first"));
    let u2 = make_update(doc_id, set_title("second"));
    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update: u1,
    });
    let _effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update: u2,
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

#[test]
fn test_same_update_id_different_seq_is_fatal() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let conn_id = state.active_connection_id.unwrap();
    let update = make_update(doc_id, set_title("test"));
    let update2 = DocumentUpdate {
        update_id: update.update_id,
        ..make_update(doc_id, set_title("test2"))
    };
    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 11,
        update,
    });
    let _effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        seq: 12,
        update: update2,
    });
    assert_eq!(state.phase(), SessionPhase::Error);
}

#[test]
fn test_close_cancels_reconnect() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    let effects = state.transition(ClientEvent::Close);
    assert_eq!(state.phase(), SessionPhase::Closed);
    assert!(effects.iter().any(|e| matches!(e, Effect::CancelReconnect)));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DisconnectSocket))
    );
}

// ── Spec 2: OpenDocument lifecycle reset ─────────────────────────────────

#[test]
fn test_open_b_rejects_late_state_from_open_a() {
    let doc_id_a = make_doc_id();
    let doc_id_b = make_doc_id();
    let mut state = ClientState::new(doc_id_a);

    // Open A -> HydratingLocal
    state.transition(ClientEvent::OpenDocument { doc_id: doc_id_a });
    let lifecycle_a = state.lifecycle_id;

    // Hydrate A -> Offline
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: lifecycle_a,
        confirmed_doc: Some(new_document(doc_id_a)),
        confirmed_seq: 5,
        pending_updates: vec![],
    });
    assert_eq!(state.phase(), SessionPhase::Offline);

    // Open B -> resets lifecycle and state
    state.transition(ClientEvent::OpenDocument { doc_id: doc_id_b });
    assert_eq!(state.phase(), SessionPhase::HydratingLocal);
    assert_eq!(state.doc_id, doc_id_b);
    let lifecycle_b = state.lifecycle_id;
    assert_ne!(lifecycle_a, lifecycle_b, "lifecycle must change");

    // Late hydration for A arrives -> ignored (lifecycle mismatch)
    let effects = state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: lifecycle_a,
        confirmed_doc: Some(new_document(doc_id_a)),
        confirmed_seq: 5,
        pending_updates: vec![],
    });
    assert!(effects.is_empty());
    assert_eq!(
        state.phase(),
        SessionPhase::HydratingLocal,
        "state must not advance"
    );
}

#[test]
fn test_open_b_starts_from_empty_document() {
    let doc_id_a = make_doc_id();
    let doc_id_b = make_doc_id();
    let mut state = ClientState::new(doc_id_a);

    // Open A, hydrate with confirmed state
    state.transition(ClientEvent::OpenDocument { doc_id: doc_id_a });
    let lifecycle_a = state.lifecycle_id;

    let mut doc_a = new_document(doc_id_a);
    doc_a.title = "Doc A State".into();
    state.transition(ClientEvent::LocalHydrationLoaded {
        lifecycle: lifecycle_a,
        confirmed_doc: Some(doc_a),
        confirmed_seq: 5,
        pending_updates: vec![],
    });
    assert_eq!(state.confirmed_doc().title, "Doc A State");
    assert_eq!(state.confirmed_seq(), 5);

    // Open B -> confirmed_doc reset to empty doc for doc_id_b
    state.transition(ClientEvent::OpenDocument { doc_id: doc_id_b });
    assert_eq!(state.confirmed_doc().id, doc_id_b);
    assert_eq!(state.confirmed_seq(), 0);
    assert!(state.confirmed_doc().title.is_empty());
    assert!(state.pending_local().is_empty());
    assert_eq!(state.intent_queue_len(), 0);
    assert_eq!(state.phase(), SessionPhase::HydratingLocal);
}

// ── Spec 6 (continued): do not install stale candidate_view_doc ──────────

#[test]
fn test_local_update_persisted_does_not_install_stale_candidate() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let conn_id = state.active_connection_id.unwrap();
    let _effects = state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id: conn_id,
        current_seq: 10,
    });
    assert_eq!(state.phase(), SessionPhase::Live);

    // Fire local intent (creates candidate_view_doc with title "local-edit")
    let uid = local_intent(&mut state, set_title("local-edit"));

    // Remote confirmation changes confirmed_doc before local persists
    let token11 = confirm_seq(&mut state, 11);
    confirm_persisted(&mut state, token11);
    // confirmed_doc now has title "seq11"
    assert_eq!(state.confirmed_doc().title, "seq11");

    // Now local persistence completes
    // candidate_view_doc was based on old view (before seq11),
    // but we should NOT install it. Instead, rebuild from confirmed_doc + pending_local.
    persist_local(&mut state, uid);

    // view must be rebuilt from confirmed_doc (title "seq11") + pending local ("local-edit")
    // The result should have "local-edit" (pending local applied after confirmed)
    assert_eq!(state.view_doc().title, "local-edit");
    assert_eq!(state.confirmed_doc().title, "seq11");
    assert_eq!(state.pending_local().len(), 1);
}

#[test]
fn test_local_intent_is_rejected_in_every_non_editable_phase() {
    let doc_id = make_doc_id();

    for expected_phase in [
        SessionPhase::Closed,
        SessionPhase::HydratingLocal,
        SessionPhase::Error,
    ] {
        let mut state = ClientState::new(doc_id);
        state.phase = expected_phase;
        let effects = state.transition(ClientEvent::LocalIntent {
            op: set_title("invalid"),
            update_id: UpdateId::new_v4(),
            applied_at_ms: 1000,
        });
        assert_eq!(state.phase(), expected_phase);
        assert!(!state.has_pending_intent());
        assert!(state.intent_queue.is_empty());
        assert!(matches!(effects.as_slice(), [Effect::NotifyError(_)]));
    }
}

#[test]
fn test_disconnect_invalidates_bootstrap_fetch_and_late_page() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let connection_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id,
        current_seq: 2,
    });
    let stale_fetch_id = state.active_fetch_id.unwrap();

    state.transition(ClientEvent::SocketDisconnected {
        lifecycle: state.lifecycle_id,
        connection_id,
    });
    assert_eq!(state.phase(), SessionPhase::Offline);
    assert_eq!(state.active_fetch_id(), None);
    assert_eq!(state.active_fetch_purpose(), None);
    assert_eq!(state.ready_catchup_target(), None);

    let effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id: stale_fetch_id,
        items: vec![EncryptedUpdateMeta {
            seq: 1,
            update: make_update(doc_id, set_title("late")),
        }],
    });
    assert!(effects.is_empty());
    assert_eq!(state.phase(), SessionPhase::Offline);
    assert_eq!(state.confirmed_seq(), 0);
}

#[test]
fn test_bootstrap_page_in_wrong_phase_cannot_enter_live() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let connection_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id,
        current_seq: 1,
    });
    let fetch_id = state.active_fetch_id.unwrap();

    state.phase = SessionPhase::Offline;
    let effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: vec![EncryptedUpdateMeta {
            seq: 1,
            update: make_update(doc_id, set_title("wrong-phase")),
        }],
    });

    assert!(effects.is_empty());
    assert_eq!(state.phase(), SessionPhase::Offline);
    assert_eq!(state.confirmed_seq(), 0);
    assert_eq!(state.active_fetch_id(), None);
}

#[test]
fn test_websocket_reaching_ready_target_invalidates_bootstrap_fetch() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let connection_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id,
        current_seq: 2,
    });
    let stale_fetch_id = state.active_fetch_id.unwrap();

    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id,
        seq: 1,
        update: make_update(doc_id, set_title("one")),
    });
    let token_1 = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .unwrap();
    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id,
        seq: 2,
        update: make_update(doc_id, set_title("two")),
    });
    let token_2 = confirm_persisted(&mut state, token_1)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .unwrap();
    confirm_persisted(&mut state, token_2);

    assert_eq!(state.phase(), SessionPhase::Live);
    assert_eq!(state.confirmed_seq(), 2);
    assert_eq!(state.active_fetch_id(), None);
    assert_eq!(state.ready_catchup_target(), None);

    let effects = state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id: stale_fetch_id,
        items: vec![],
    });
    assert!(effects.is_empty());
    assert_eq!(state.phase(), SessionPhase::Live);
}

#[test]
fn test_holey_catch_up_page_is_rejected_without_partial_buffering() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 0);
    let connection_id = state.active_connection_id.unwrap();
    state.transition(ClientEvent::Ready {
        lifecycle: state.lifecycle_id,
        connection_id,
        current_seq: 3,
    });
    let fetch_id = state.active_fetch_id.unwrap();

    state.transition(ClientEvent::CatchUpPageLoaded {
        lifecycle: state.lifecycle_id,
        fetch_id,
        items: vec![
            EncryptedUpdateMeta {
                seq: 1,
                update: make_update(doc_id, set_title("one")),
            },
            EncryptedUpdateMeta {
                seq: 3,
                update: make_update(doc_id, set_title("three")),
            },
        ],
    });

    assert_eq!(state.phase(), SessionPhase::Error);
    assert!(state.inbox.is_empty());
    assert!(state.pending_confirmation.is_none());
    assert!(
        state
            .errors()
            .iter()
            .any(|error| error.contains("expected seq"))
    );
}

#[test]
fn test_old_sequenced_updates_are_ignored_but_known_seq_conflicts_are_fatal() {
    let doc_id = make_doc_id();
    let mut state = ClientState::new(doc_id);
    hydrate(&mut state, 10);
    let connection_id = state.active_connection_id.unwrap();

    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id,
        seq: 5,
        update: make_update(doc_id, set_title("unknown-old")),
    });
    assert!(effects.is_empty());
    assert!(state.inbox.is_empty());
    assert_eq!(state.phase(), SessionPhase::Offline);

    let update_11 = make_update(doc_id, set_title("eleven"));
    let effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id,
        seq: 11,
        update: update_11.clone(),
    });
    let token = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::PersistConfirmation { token, .. } => Some(token),
            _ => None,
        })
        .unwrap();
    confirm_persisted(&mut state, token);

    let duplicate_effects = state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id,
        seq: 11,
        update: update_11,
    });
    assert!(duplicate_effects.is_empty());
    assert_eq!(state.phase(), SessionPhase::Offline);

    state.transition(ClientEvent::SequencedUpdate {
        lifecycle: state.lifecycle_id,
        connection_id,
        seq: 11,
        update: make_update(doc_id, set_title("conflict")),
    });
    assert_eq!(state.phase(), SessionPhase::Error);
    assert!(
        state
            .errors()
            .iter()
            .any(|error| error.contains("already confirmed"))
    );
}

#[test]
fn test_confirmation_completion_guard_rejects_previous_document_lifecycle() {
    let doc_id_a = make_doc_id();
    let doc_id_b = make_doc_id();
    let mut state = ClientState::new(doc_id_a);
    hydrate(&mut state, 0);
    let lifecycle_a = state.lifecycle_id;
    let token_a = confirm_seq(&mut state, 1);
    assert!(state.is_confirmation_completion_current(lifecycle_a, doc_id_a, token_a));

    state.transition(ClientEvent::OpenDocument { doc_id: doc_id_b });
    assert!(!state.is_confirmation_completion_current(lifecycle_a, doc_id_a, token_a));
    let effects = confirm_persisted(&mut state, token_a);
    assert!(effects.is_empty());
    assert_eq!(state.phase(), SessionPhase::HydratingLocal);
    assert_eq!(state.confirmed_seq(), 0);

    hydrate(&mut state, 0);
    let token_b = confirm_seq(&mut state, 1);
    assert!(state.is_confirmation_completion_current(state.lifecycle_id, doc_id_b, token_b));
    assert!(!state.is_confirmation_completion_current(lifecycle_a, doc_id_a, token_b));
}

// ── Existing old test replaced: test_catch_up_rejects_beyond_boundary ────
// replaced by test_ready_ws_gt_target_buffers_not_errors above

fn markdown_block(text: &str) -> DocumentBlock {
    DocumentBlock::new(
        BlockId::new_v4(),
        BlockContent::Markdown { text: text.into() },
    )
}
