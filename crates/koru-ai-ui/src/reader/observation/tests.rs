// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::cell::Cell;

fn transcript() -> ChatTranscript {
    let chat = ChatId::new();
    serde_json::from_value(serde_json::json!({
        "chat": {
            "id": chat, "title": "Queued chat",
            "current_model": {
                "provider": "openai", "requested_model": "fixture",
                "default_reasoning": "default"
            },
            "branched_from": null, "created_at": 1, "updated_at": 1
        },
        "tool_policy": { "chat_id": chat, "mode": "manual", "workspace_ids": [] },
        "messages": [], "blocks": [], "pinned_documents": []
    }))
    .unwrap()
}

#[test]
fn unchanged_pending_reads_do_not_redraw_and_terminal_read_completes_once() {
    let first = transcript();
    let mut changed = first.clone();
    changed.chat.title = "New title".into();
    let mut snapshots = [first.clone(), first, changed.clone(), changed].into_iter();
    let mut states = [
        AttemptStatus::Pending,
        AttemptStatus::Pending,
        AttemptStatus::Streaming,
        AttemptStatus::Complete,
    ]
    .into_iter();
    let attempt = InferenceAttemptId::new();
    let mut updates = Vec::new();
    let mut waits = Vec::new();
    observe(
        || Ok(states.next().unwrap()),
        || Ok(snapshots.next().unwrap()),
        |update| {
            updates.push(update);
            true
        },
        || true,
        |wait| waits.push(wait),
        attempt,
    );
    assert_eq!(updates.len(), 3);
    assert!(matches!(&updates[0], Update::Observed { attempt: id, .. } if *id == attempt));
    assert!(
        matches!(&updates[1], Update::Observed { transcript, .. } if transcript.chat.title == "New title")
    );
    assert!(
        matches!(&updates[2], Update::Recovered { attempt: id, status: AttemptStatus::Complete, .. } if *id == attempt)
    );
    assert_eq!(
        waits,
        [RECOVERY_FIRST, RECOVERY_FIRST * 2, RECOVERY_FIRST * 4]
    );
}

#[test]
fn navigation_during_a_read_discards_its_result() {
    let alive = Cell::new(true);
    observe(
        || Ok(AttemptStatus::Pending),
        || {
            alive.set(false);
            Ok(transcript())
        },
        |_| panic!("navigation discarded this reply"),
        || alive.get(),
        |_| panic!("no further read"),
        InferenceAttemptId::new(),
    );
}

#[test]
fn retryable_reads_back_off_and_a_hold_ends_observation() {
    let reads = Cell::new(0);
    let mut waits = Vec::new();
    let mut updates = Vec::new();
    let attempt = InferenceAttemptId::new();
    observe(
        || {
            reads.set(reads.get() + 1);
            Err(koru_ai_wire::Failure {
                code: if reads.get() < 6 {
                    "unavailable"
                } else {
                    "bot_archived"
                }
                .into(),
                message: "Fixture".into(),
                retryable: reads.get() < 6,
                details: Default::default(),
            })
        },
        || panic!("a refused attempt cannot read a chat"),
        |update| {
            updates.push(update);
            true
        },
        || true,
        |wait| waits.push(wait),
        attempt,
    );
    assert_eq!(
        waits,
        [
            RECOVERY_FIRST,
            RECOVERY_FIRST * 2,
            RECOVERY_FIRST * 4,
            RECOVERY_MAX,
            RECOVERY_MAX
        ]
    );
    assert_eq!(updates.len(), 1);
    assert!(
        matches!(&updates[0], Update::RecoveryFailed { attempt: id, failure } if *id == attempt && failure.code == "bot_archived")
    );
}

#[test]
fn switching_chats_releases_observation_but_same_chat_read_and_model_change_keep_it() {
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    let life = Arc::new(());
    let weak = Arc::downgrade(&life);
    let mut observation = Observation {
        selected: Some(chat),
        active: Some(Active {
            chat,
            attempt,
            generation: 1,
            _life: life,
            running: Weak::new(),
        }),
    };
    observation.select(&Command::Read(chat));
    observation.select(&Command::UpdateModel(
        chat,
        koru_ai_wire::UpdateChatModelRequest {
            model: koru_ai_core::ModelSelection {
                provider: AiProviderId::parse("openai").unwrap(),
                requested_model: "next-model".into(),
                default_reasoning: koru_ai_core::ReasoningChoice::Default,
            },
        },
    ));
    assert!(weak.upgrade().is_some());
    observation.select(&Command::Read(ChatId::new()));
    assert!(weak.upgrade().is_none());
}

#[test]
fn dropping_reader_observation_stops_the_background_read() {
    let life = Arc::new(());
    let weak = Arc::downgrade(&life);
    let observation = Observation {
        selected: None,
        active: Some(Active {
            chat: ChatId::new(),
            attempt: InferenceAttemptId::new(),
            generation: 1,
            _life: life,
            running: Weak::new(),
        }),
    };
    drop(observation);
    observe(
        || panic!("no reader"),
        || panic!("no reader"),
        |_| panic!("no reader"),
        || weak.upgrade().is_some(),
        |_| panic!("no reader"),
        InferenceAttemptId::new(),
    );
}

#[test]
fn same_attempt_can_be_observed_again_after_its_worker_ends() {
    let mut observation = Observation::default();
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    observation.select(&Command::Read(chat));
    let (_, running) = observation.begin(chat, attempt, 1).unwrap();
    assert!(observation.begin(chat, attempt, 1).is_none());
    // A refusal ends the worker, while the selected conversation remains.
    drop(running);
    observation.select(&Command::Read(chat));
    let (_, restarted) = observation.begin(chat, attempt, 1).unwrap();
    assert!(observation.begin(chat, attempt, 1).is_none());
    drop(restarted);
    observation.select(&Command::Read(ChatId::new()));
    assert!(
        observation.begin(chat, attempt, 1).is_none(),
        "late old command"
    );
}

#[test]
fn new_unsent_chat_stops_observation_and_returning_can_start_it_again() {
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    let mut observation = Observation::default();
    observation.select(&Command::Read(chat));
    let (alive, _worker) = observation.begin(chat, attempt, 1).unwrap();
    observation.select(&Command::StopObserving(chat, attempt, 1));
    assert!(alive.upgrade().is_none());
    observation.select(&Command::Read(chat));
    assert!(observation.begin(chat, attempt, 1).is_some());
}

#[test]
fn delayed_stop_cannot_close_a_newer_observation() {
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    let next = InferenceAttemptId::new();
    let mut observation = Observation::default();
    observation.select(&Command::Read(chat));
    let (_, _old) = observation.begin(chat, attempt, 1).unwrap();
    let (alive, _next) = observation.begin(chat, next, 2).unwrap();
    observation.select(&Command::StopObserving(chat, attempt, 1));
    assert!(alive.upgrade().is_some());
    assert_eq!(observation.selected, Some(chat));
    let (returned, _returned_worker) = observation.begin(chat, next, 3).unwrap();
    observation.select(&Command::StopObserving(chat, next, 2));
    assert!(returned.upgrade().is_some(), "same attempt, newer observer");
    observation.select(&Command::Read(ChatId::new()));
    observation.select(&Command::StopObserving(chat, next, 2));
    assert_ne!(observation.selected, Some(chat));
}
