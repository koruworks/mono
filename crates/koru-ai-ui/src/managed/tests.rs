// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::{BotChatLabel, BotChatPurpose, BotConversation, BotId};
use koru_ai_core::{AiProviderId, Chat, ModelSelection, ReasoningChoice};

#[test]
fn saved_attachment_tiles_keep_the_accepted_message_and_order_on_refresh() {
    let mut state = State::new(&mut opened(), Instant::now());
    let mut saved = message(1, Uuid::new_v4());
    saved.sender = "person:synthetic".into();
    let attachment = koru_ai_core::ChatAttachment {
        id: koru_ai_core::ChatAttachmentId::new(),
        message_id: Some(MessageId::from_uuid(saved.id)),
        ordinal: Some(0),
        kind: koru_ai_core::AttachmentKind::File,
        source: koru_ai_core::AttachmentSource::Drop,
        file_name: "fixture.txt".into(),
        media_type: "text/plain".into(),
        byte_count: 5,
        dimensions: None,
        origin: None,
        reference_path: None,
        created_at: Timestamp::from_millis(7),
    };
    saved
        .content
        .attachments
        .push(serde_json::to_value(&attachment).unwrap());
    assert!(state.ingest(saved.clone()));
    assert_eq!(state.transcript().attachments, vec![attachment.clone()]);
    assert!(!state.ingest(saved));
    state.ingest(message(2, Uuid::new_v4()));
    assert_eq!(state.transcript().attachments, vec![attachment]);
}

pub(crate) fn opened() -> Opened {
    let bot = BotId::from_uuid(Uuid::new_v4());
    let now = Timestamp::from_millis(7);
    Opened {
        navigation: Default::default(),
        snapshot: wire::Snapshot {
            joined: true,
            participants: vec![],
            origin: None,
            conversation: BotConversation {
                chat: Chat {
                    id: ChatId::new(),
                    title: "Main".into(),
                    private: false,
                    current_model: ModelSelection {
                        provider: AiProviderId::parse("agent-codex").unwrap(),
                        requested_model: "qualified-model".into(),
                        default_reasoning: ReasoningChoice::Default,
                    },
                    branched_from: None,
                    agent_working_folder: None,
                    agent_mode: None,
                    agent_fast: None,
                    pinned_at: None,
                    profile_id: None,
                    bot: None,
                    created_at: now,
                    updated_at: now,
                },
                label: BotChatLabel {
                    bot_id: bot,
                    bot_name: "Test Bot".into(),
                    purpose: BotChatPurpose::Main,
                },
            },
            settings_revision: 1,
            paused: false,
            team_paused: false,
            waiting_messages: 0,
            head_sequence: 0,
            turns: vec![wire::ActiveTurn {
                id: Uuid::new_v4(),
                bot: bot.as_uuid(),
                state: "running".into(),
                generation: 2,
                worker: Some(Uuid::new_v4()),
                session: Some(Uuid::new_v4()),
                reply_message: Some(Uuid::new_v4()),
                created_at_ms: 8,
                details: serde_json::json!({}),
            }],
        },
        messages: vec![],
        before: None,
        after: 0,
        settings: None,
        questions: vec![],
        attention: Default::default(),
    }
}

pub(crate) fn event(state: &State, cursor: u64, text: Option<&str>) -> wire::Event {
    let turn = state.primary_turn().unwrap();
    let turn_id = turn.id.to_string().parse().unwrap();
    wire::Event {
        cursor,
        observation: wire::Observation {
            thread: state.thread().as_uuid(),
            bot: state.snapshot.conversation.label.bot_id.as_uuid(),
            turn,
            event: match text {
                None => wire::AgentEvent::TurnStarted { turn_id },
                Some(text) => wire::AgentEvent::MessageDelta {
                    turn_id,
                    text: text.into(),
                },
            },
        },
    }
}

pub(crate) fn message(sequence: u64, id: Uuid) -> wire::Message {
    wire::Message {
        id,
        sequence,
        sender: format!("bot:{}", Uuid::new_v4()),
        kind: "words".into(),
        content: wire::Content {
            body: "Saved reply".into(),
            attachments: vec![],
            references: vec![],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: "none".into(),
        },
        created_at_ms: 18,
    }
}

#[test]
fn live_gap_never_displays_a_suffix_as_the_whole_answer() {
    let mut state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let events = vec![event(&state, 1, None), event(&state, 2, Some("start "))];
    assert!(state.observe(Feed {
        boot,
        cursor: 2,
        gap: false,
        entries: events
    }));
    assert_eq!(state.live.values().next().unwrap().text, "start ");
    let events = vec![event(&state, 4, Some("suffix"))];
    state.observe(Feed {
        boot,
        cursor: 4,
        gap: true,
        entries: events,
    });
    assert!(state.gap);
    assert!(state.live.values().all(|live| live.text.is_empty()));
    let events = vec![event(&state, 5, Some("more suffix"))];
    state.observe(Feed {
        boot,
        cursor: 5,
        gap: false,
        entries: events,
    });
    assert!(state.live.values().all(|live| live.text.is_empty()));
}

#[test]
fn repeated_events_and_foreign_turns_cannot_duplicate_or_pollute_live_reply() {
    let mut state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let events = vec![event(&state, 1, None), event(&state, 2, Some("hello"))];
    state.observe(Feed {
        boot,
        cursor: 2,
        gap: false,
        entries: events.clone(),
    });
    assert!(!state.observe(Feed {
        boot,
        cursor: 2,
        gap: false,
        entries: events
    }));
    let mut stale = event(&state, 3, Some("wrong session"));
    stale.observation.turn.session = Uuid::new_v4();
    state.observe(Feed {
        boot,
        cursor: 3,
        gap: false,
        entries: vec![stale],
    });
    assert_eq!(state.live.values().next().unwrap().text, "hello");
}

#[test]
fn publish_replaces_live_identity_and_preserves_real_history_facts() {
    let mut state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let events = vec![event(&state, 1, None), event(&state, 2, Some("draft"))];
    state.observe(Feed {
        boot,
        cursor: 2,
        gap: false,
        entries: events,
    });
    let reply = state.live.values().next().unwrap().reply;
    let mut snapshot = state.snapshot.clone();
    snapshot.turns.clear();
    let published = message(4, reply.as_uuid());
    assert!(state.refresh(
        Refreshed {
            navigation: Default::default(),
            snapshot,
            messages: vec![published],
            attention: Default::default(),
            questions: vec![],
            after: 4,
            feed: Feed {
                boot,
                cursor: 2,
                gap: false,
                entries: vec![]
            }
        },
        Instant::now()
    ));
    assert!(state.live.is_empty());
    let transcript = state.transcript();
    assert_eq!(transcript.messages.len(), 1);
    let saved = &transcript.messages[0];
    assert_eq!(saved.id, reply);
    assert_eq!(saved.markdown, "Saved reply");
    assert_eq!(saved.created_at.millis(), 18);
    assert!(saved.provider.is_none() && saved.requested_model.is_none());
}

#[test]
fn poll_backpressure_and_old_settings_snapshot_are_safe() {
    let now = Instant::now();
    let mut state = State::new(&mut opened(), now);
    assert!(state.poll(now).is_some());
    assert!(state.poll(now + Duration::from_secs(9)).is_none());
    let snapshot = state.snapshot.clone();
    state.snapshot.settings_revision = 2;
    state.snapshot.conversation.chat.agent_fast = Some(true);
    state.refresh(
        Refreshed {
            navigation: Default::default(),
            snapshot,
            messages: vec![],
            attention: Default::default(),
            questions: vec![],
            after: 0,
            feed: Feed {
                boot: Uuid::new_v4(),
                cursor: 0,
                gap: false,
                entries: vec![],
            },
        },
        now,
    );
    assert_eq!(state.snapshot.settings_revision, 2);
    assert_eq!(state.snapshot.conversation.chat.agent_fast, Some(true));
    assert!(state.poll(now).is_none());
    assert!(state.poll(now + REFRESH_INTERVAL).is_some());
}

#[test]
fn saved_markdown_blocks_keep_layout_keys_when_other_history_arrives() {
    let mut state = State::new(&mut opened(), Instant::now());
    let mut first = message(1, Uuid::new_v4());
    first.content.body = "# Heading\n\nA paragraph.\n\n```rust\nlet x = 1;\n```".into();
    assert!(state.ingest(first.clone()));
    let before = state.transcript().blocks;
    assert_eq!(before.len(), 3);
    assert!(!state.ingest(first));
    state.ingest(message(2, Uuid::new_v4()));
    let after = state.transcript().blocks;
    assert_eq!(&before, &after[..3]);
}
