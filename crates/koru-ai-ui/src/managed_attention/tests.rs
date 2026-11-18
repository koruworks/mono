// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::reader::Reader;

fn item() -> wire::OperationAttention {
    wire::OperationAttention {
        key: wire::OperationRef {
            bot: uuid::Uuid::new_v4(),
            operation: uuid::Uuid::new_v4(),
        },
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        kind: "edit".into(),
        unresolved: true,
        drained: true,
    }
}
fn page(items: Vec<wire::OperationAttention>) -> Page {
    Page {
        items,
        ..Default::default()
    }
}

#[test]
fn continue_requires_the_complete_drained_set_and_preserves_last_complete_view() {
    let first = item();
    let last = item();
    let mut state = State::new(Page {
        items: vec![first.clone()],
        next: Some(first.key.clone()),
        after: None,
    });
    assert!(!state.ready());
    assert!(state.items.is_empty());
    assert!(state.ingest(Page {
        items: vec![last.clone()],
        next: None,
        after: Some(first.key.clone())
    }));
    assert!(state.ready());
    assert_eq!(state.items.len(), 2);
    state.cursor = 1;
    state.ingest(Page {
        items: vec![first.clone()],
        next: Some(first.key.clone()),
        after: None,
    });
    assert_eq!(
        state.items.len(),
        2,
        "a partial refresh cannot become the shown exact set"
    );
    state.ingest(Page {
        items: vec![wire::OperationAttention {
            drained: false,
            ..last
        }],
        next: None,
        after: Some(first.key),
    });
    assert!(!state.ready());
    assert_eq!(state.cursor, 0);
    assert_eq!(
        state.labels(koru_i18n::Lang::En, &HashMap::new()),
        vec![("Page 1".into(), false), ("Page 2".into(), false)]
    );
}

#[test]
fn ordinary_running_writes_do_not_claim_uncertainty() {
    let mut opened = crate::managed::tests::opened();
    let mut write = item();
    write.key.bot = opened.snapshot.turns[0].bot;
    write.unresolved = false;
    write.drained = false;
    let state = State::new(page(vec![write]));
    assert!(!state.visible(&opened.snapshot.turns));
    opened.snapshot.turns[0].state = "interrupted".into();
    assert!(state.visible(&opened.snapshot.turns));
    assert!(!state.ready());
}

#[test]
fn page_open_continue_retry_and_late_snapshot_keep_exact_identities() {
    let mut opened = crate::managed::tests::opened();
    let first = item();
    let second = item();
    let thread = opened.snapshot.conversation.chat.id;
    opened.attention = page(vec![first.clone(), second.clone()]);
    let mut surface = ChatSurface::new_owned_at(None);
    surface.managed_opened(opened);
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface
        .document_titles
        .insert(first.document, "Review page".into());
    surface.rebuild_managed_attention(340.0);
    surface.place_managed_attention(20.0);
    assert!(surface.focus_managed_attention(false));
    assert_eq!(
        surface.managed_attention_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(
        surface.take_open_document_request(),
        Some(koru_link::KoruLink::Document {
            document: first.document.as_uuid(),
            node: None
        })
    );
    assert!(commands.try_recv().is_err());
    assert!(surface.focus_managed_attention(true));
    surface.managed_attention_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
    let Command::Engine { command, .. } = commands.recv().unwrap() else {
        panic!("engine command");
    };
    let wire::Command::Continue { id, operations, .. } = &command else {
        panic!("Continue");
    };
    let id = *id;
    assert!(*operations == vec![first.key.clone(), second.key.clone()]);
    assert_eq!(surface.focus, Focus::Composer);
    assert!(!surface.managed.as_ref().unwrap().attention.ready());
    assert!(surface.failed_managed_attention(
        thread,
        &command,
        &koru_ai_wire::Failure {
            code: "gateway_unavailable".into(),
            message: "Unavailable".into(),
            retryable: true,
            details: Default::default(),
        }
    ));
    surface.maintain_managed_attention(Instant::now() + Duration::from_secs(3));
    let Command::Engine { command: retry, .. } = commands.recv().unwrap() else {
        panic!("retry");
    };
    assert!(serde_json::to_value(&command).unwrap() == serde_json::to_value(&retry).unwrap());
    surface.managed_reply(
        thread,
        wire::Response::Continued {
            request: id,
            messages: vec![],
            replayed: true,
        },
    );
    let state = &mut surface.managed.as_mut().unwrap().attention;
    assert!(state.pending.is_none());
    assert!(state.items.is_empty());
    state.ingest(page(vec![first, second]));
    assert!(
        state.items.is_empty(),
        "an in-flight old read cannot ask to continue twice"
    );
    state.ingest(Page::default());
    assert!(state.acknowledged.is_empty());
}

#[test]
fn changed_set_refusal_requires_a_fresh_read_and_keyboard_can_leave_the_card() {
    let mut opened = crate::managed::tests::opened();
    opened.attention = page(vec![item()]);
    let thread = opened.snapshot.conversation.chat.id;
    let mut surface = ChatSurface::new_owned_at(None);
    surface.managed_opened(opened);
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.rebuild_managed_attention(240.0);
    surface.focus_managed_attention(true);
    surface.managed_attention_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
    assert_eq!(surface.focus, Focus::Composer);
    surface.focus_managed_attention(false);
    surface.managed_attention_key(&Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT);
    assert_eq!(surface.focus, Focus::Composer);
    surface.activate_managed_attention(1);
    let Command::Engine { command, .. } = commands.recv().unwrap() else {
        panic!("Continue");
    };
    surface.failed_managed_attention(
        thread,
        &command,
        &koru_ai_wire::Failure {
            code: "recovery_pending".into(),
            message: "Changed".into(),
            retryable: true,
            details: Default::default(),
        },
    );
    let state = &surface.managed.as_ref().unwrap().attention;
    assert!(state.pending.is_none());
    assert!(!state.ready());
    surface.maintain_managed_attention(Instant::now() + Duration::from_secs(3));
    assert!(commands.try_recv().is_err());
}
