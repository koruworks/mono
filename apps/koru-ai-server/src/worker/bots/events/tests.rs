// SPDX-License-Identifier: MPL-2.0

//! Cohort `m8_events_content_free`, the builders: what each publishes from
//! the values it is given, with no worker behind it. Each row at its real
//! transition is in the cohort's other files: `team_pause/tests/events.rs`,
//! `client/owner/tests/events.rs` and `client/mirror/tests/events.rs`.

use koru_ai_core::bots::BotError;

use super::recorder::{BotEventRecorder, RecordedBotEvent, assert_holds_none_of};
use super::*;

/// Text that must never be in a row: what a person, a Bot or another
/// computer could have written.
const CONTENT: &[&str] = &[
    "Budget Bot",
    "please summarize the acquisition",
    "C:\\Users\\sam\\Documents",
    "sk-live-0123456789",
    "Sams-Laptop",
];

fn recorded() -> (Arc<BotEventRecorder>, BotEvents) {
    let recorder = BotEventRecorder::new();
    let events = BotEvents::new(recorder.clone());
    (recorder, events)
}

fn copy(
    owner: DeviceId,
    state: LinkedOwnerState,
    generation: u64,
    cursor: u64,
    staging: Option<(u64, u64)>,
) -> MirrorOwner {
    MirrorOwner {
        owner_device: owner,
        feed_id: Some("0123456789abcdef0123456789abcdef".into()),
        boot_id: Some("fedcba9876543210fedcba9876543210".into()),
        generation,
        cursor,
        staging,
        withheld_bots: 0,
        state,
        last_confirmed_at_ms: None,
    }
}

/// `(state, reason, cursor)` of every `bots.mirror.state` row.
fn states(events: &[RecordedBotEvent]) -> Vec<(String, String, String)> {
    events
        .iter()
        .map(|event| {
            assert_eq!(event.name, "bots.mirror.state");
            assert_eq!(event.keys(), ["cursor", "owner_device", "reason", "state"]);
            (
                event.attr("state").unwrap().to_owned(),
                event.attr("reason").unwrap().to_owned(),
                event.attr("cursor").unwrap().to_owned(),
            )
        })
        .collect()
}

fn row(state: &str, reason: &str, cursor: u64) -> (String, String, String) {
    (state.to_owned(), reason.to_owned(), cursor.to_string())
}

#[test]
fn m8_events_content_free_a_copy_that_did_not_change_state_publishes_nothing() {
    use LinkedOwnerState::*;
    let owner = DeviceId::new();
    let (recorder, events) = recorded();
    // The same row: a page that was refused, or one that was sent again.
    let current = copy(owner, Current, 3, 40, None);
    events.mirror(Some(&current), &current, CopyChange::CaughtUp, false);
    // A page that moved the cursor of a copy that was already current.
    events.mirror(
        Some(&current),
        &copy(owner, Current, 3, 41, None),
        CopyChange::CaughtUp,
        false,
    );
    // A page of a reset that is being received: the staged cursor moves.
    let staging = copy(owner, Stale, 3, 40, Some((4, 10)));
    events.mirror(
        Some(&staging),
        &copy(owner, Stale, 3, 40, Some((4, 20))),
        CopyChange::CaughtUp,
        false,
    );
    // A row that is answered as it is when its owner is named again.
    let never = copy(owner, Never, 0, 0, None);
    events.mirror(Some(&never), &never, CopyChange::Linked, false);
    assert_eq!(recorder.count(), 0, "{:?}", recorder.events());
}

#[test]
fn m8_events_content_free_each_change_of_a_copy_is_one_row() {
    use LinkedOwnerState::*;
    let owner = DeviceId::new();
    let case = |before: Option<MirrorOwner>, after: MirrorOwner, why, began_again| {
        let (recorder, events) = recorded();
        events.mirror(before.as_ref(), &after, why, began_again);
        let published = recorder.events();
        for event in &published {
            assert_eq!(event.attr("owner_device"), Some(owner.to_string().as_str()));
            assert_eq!(event.actor, "ai-worker");
        }
        assert_holds_none_of(&published, CONTENT);
        states(&published)
    };
    // The computer became a linked one: its row was made.
    assert_eq!(
        case(
            None,
            copy(owner, Never, 0, 0, None),
            CopyChange::Linked,
            false
        ),
        [row("never", "linked", 0)]
    );
    // The first page of the first copy: a reset begins.
    assert_eq!(
        case(
            Some(copy(owner, Never, 0, 0, None)),
            copy(owner, Never, 0, 0, Some((1, 7))),
            CopyChange::CaughtUp,
            true
        ),
        [row("never", "reset_started", 0)]
    );
    // The page that finishes it switches the copy and brings it up to date.
    assert_eq!(
        case(
            Some(copy(owner, Never, 0, 0, Some((1, 7)))),
            copy(owner, Current, 1, 12, None),
            CopyChange::CaughtUp,
            false
        ),
        [
            row("current", "reset_finished", 12),
            row("current", "caught_up", 12)
        ]
    );
    // A whole copy in one page: the reset begins and finishes.
    assert_eq!(
        case(
            Some(copy(owner, Never, 0, 0, None)),
            copy(owner, Current, 1, 5, None),
            CopyChange::CaughtUp,
            true
        ),
        [
            row("current", "reset_started", 5),
            row("current", "reset_finished", 5),
            row("current", "caught_up", 5)
        ]
    );
    // A reset of a copy that stays current says only its two ends.
    assert_eq!(
        case(
            Some(copy(owner, Current, 1, 12, None)),
            copy(owner, Current, 1, 12, Some((2, 4))),
            CopyChange::CaughtUp,
            true
        ),
        [row("current", "reset_started", 12)]
    );
    assert_eq!(
        case(
            Some(copy(owner, Current, 1, 12, Some((2, 4)))),
            copy(owner, Current, 2, 9, None),
            CopyChange::CaughtUp,
            false
        ),
        [row("current", "reset_finished", 9)]
    );
    // A reset that begins again while one was being received.
    assert_eq!(
        case(
            Some(copy(owner, Stale, 1, 12, Some((2, 4)))),
            copy(owner, Stale, 1, 12, Some((2, 3))),
            CopyChange::CaughtUp,
            true
        ),
        [
            row("stale", "reset_dropped", 12),
            row("stale", "reset_started", 12)
        ]
    );
    // The stream ended.
    assert_eq!(
        case(
            Some(copy(owner, Current, 1, 12, None)),
            copy(owner, Stale, 1, 12, None),
            CopyChange::StreamLost,
            false
        ),
        [row("stale", "stream_lost", 12)]
    );
    // The worker started over a copy that was current and half way through
    // a reset.
    assert_eq!(
        case(
            Some(copy(owner, Current, 1, 12, Some((2, 4)))),
            copy(owner, Stale, 1, 12, None),
            CopyChange::WorkerStarted,
            false
        ),
        [
            row("stale", "reset_dropped", 12),
            row("stale", "worker_started", 12)
        ]
    );
    // What an interrupted reset left is dropped before the next subscribe.
    assert_eq!(
        case(
            Some(copy(owner, Stale, 1, 12, Some((2, 4)))),
            copy(owner, Stale, 1, 12, None),
            CopyChange::ResetDropped,
            false
        ),
        [row("stale", "reset_dropped", 12)]
    );
    // The two computers share no version.
    assert_eq!(
        case(
            Some(copy(owner, Stale, 1, 12, None)),
            copy(owner, Unsupported, 1, 12, None),
            CopyChange::NeedsUpdate,
            false
        ),
        [row("unsupported", "needs_update", 12)]
    );
    // The computer stopped being a linked one.
    assert_eq!(
        case(
            Some(copy(owner, Current, 1, 12, None)),
            copy(owner, Withdrawing, 1, 12, None),
            CopyChange::Unlinked,
            false
        ),
        [row("withdrawing", "unlinked", 12)]
    );
}

/// `(stage, role, protocol_version, reason)` of every `bots.client.session`
/// row.
type Session = (String, Option<String>, Option<String>, Option<String>);

fn sessions(events: &[RecordedBotEvent], peer: DeviceId) -> Vec<Session> {
    events
        .iter()
        .map(|event| {
            assert_eq!(event.name, "bots.client.session");
            assert_eq!(event.attr("peer_device"), Some(peer.to_string().as_str()));
            for key in event.keys() {
                assert!(
                    ["peer_device", "stage", "role", "protocol_version", "reason"].contains(&key),
                    "{key}"
                );
            }
            (
                event.attr("stage").unwrap().to_owned(),
                event.attr("role").map(str::to_owned),
                event.attr("protocol_version").map(str::to_owned),
                event.attr("reason").map(str::to_owned),
            )
        })
        .collect()
}

fn session(stage: &str, role: &str, version: Option<u32>, reason: Option<&str>) -> Session {
    (
        stage.to_owned(),
        Some(role.to_owned()),
        version.map(|version| version.to_string()),
        reason.map(str::to_owned),
    )
}

#[test]
fn m8_events_content_free_a_stream_publishes_admitted_once_and_exactly_one_end() {
    let peer = DeviceId::new();
    let run = |work: &dyn Fn(&mut SessionLog)| {
        let (recorder, events) = recorded();
        let mut log = SessionLog::new(events, peer, Role::Command);
        work(&mut log);
        assert_eq!(
            recorder.events().len(),
            recorder
                .events()
                .iter()
                .filter(|event| event.attr("stage") == Some("admitted"))
                .count(),
            "no end is published while the stream is open"
        );
        drop(log);
        let published = recorder.events();
        assert_holds_none_of(&published, CONTENT);
        sessions(&published, peer)
    };
    // Dropped before anything was decided: the other end went away.
    assert_eq!(
        run(&|_| {}),
        [session("closed", "command", None, Some("closed"))]
    );
    // Refused before its hello was answered, with no version agreed.
    assert_eq!(
        run(&|log| log.refused("unsupported_version")),
        [session(
            "refused",
            "command",
            None,
            Some("unsupported_version")
        )]
    );
    // Refused after a version was agreed: a limit.
    assert_eq!(
        run(&|log| {
            log.negotiated(1);
            log.refused("busy");
        }),
        [session("refused", "command", Some(1), Some("busy"))]
    );
    // Admitted, then closed by the other end. `admitted` twice is one row.
    assert_eq!(
        run(&|log| {
            log.negotiated(1);
            log.admitted();
            log.admitted();
        }),
        [
            session("admitted", "command", Some(1), None),
            session("closed", "command", Some(1), Some("closed"))
        ]
    );
    // Admitted, then ended with a refusal: that is how it closed.
    assert_eq!(
        run(&|log| {
            log.negotiated(1);
            log.admitted();
            log.refused("not_own_device");
        }),
        [
            session("admitted", "command", Some(1), None),
            session("closed", "command", Some(1), Some("not_own_device"))
        ]
    );
    // The first reason given stands.
    assert_eq!(
        run(&|log| {
            log.negotiated(1);
            log.admitted();
            log.closing(SessionClose::Idle);
            log.closing(SessionClose::Timeout);
            log.refused("protocol_error");
        }),
        [
            session("admitted", "command", Some(1), None),
            session("closed", "command", Some(1), Some("idle"))
        ]
    );
    assert_eq!(
        run(&|log| {
            log.negotiated(1);
            log.admitted();
            log.closing(SessionClose::Timeout);
        })[1],
        session("closed", "command", Some(1), Some("timeout"))
    );
    // A reason this build does not know is `other`, never its text.
    for text in CONTENT {
        assert_eq!(
            run(&|log| log.refused(text)),
            [session("refused", "command", None, Some("other"))]
        );
    }
}

#[test]
fn m8_events_content_free_a_code_held_as_text_is_looked_up_never_carried() {
    let (recorder, events) = recorded();
    let (peer, request) = (DeviceId::new(), Uuid::new_v4());
    // A receipt keeps the local command's own code, which is any short
    // token. One outside the declared set is published as `other`.
    events.command(
        peer,
        request,
        "pause_bot",
        ClientOutcome::Refused,
        Some("revision_conflict"),
    );
    events.command(
        peer,
        request,
        "pause_bot",
        ClientOutcome::Refused,
        Some("sk-live-0123456789"),
    );
    events.command(peer, request, "Budget Bot", ClientOutcome::Unknown, None);
    events.stream_turned_away(peer, None, BotStreamTurnedAway::Refused("Sams-Laptop"));
    let published = recorder.events();
    assert_holds_none_of(&published, CONTENT);
    let command = |index: usize| {
        let event: &RecordedBotEvent = &published[index];
        assert_eq!(event.name, "bots.client.command");
        assert_eq!(event.attr("peer_device"), Some(peer.to_string().as_str()));
        assert_eq!(event.attr("request"), Some(request.to_string().as_str()));
        (
            event.attr("kind").unwrap().to_owned(),
            event.attr("outcome").unwrap().to_owned(),
            event.attr("failure").map(str::to_owned),
        )
    };
    let text = |value: &str| value.to_owned();
    assert_eq!(
        command(0),
        (
            text("pause_bot"),
            text("refused"),
            Some(text("revision_conflict"))
        )
    );
    assert_eq!(published[0].keys().len(), 5);
    assert_eq!(
        command(1),
        (text("pause_bot"), text("refused"), Some(text("other")))
    );
    assert_eq!(command(2), (text("other"), text("unknown"), None));
    assert_eq!(
        published[2].keys(),
        ["kind", "outcome", "peer_device", "request"]
    );
    assert_eq!(published[3].attr("reason"), Some("other"));
    assert_eq!(published[3].actor, "gateway");
}

#[test]
fn m8_events_content_free_the_sets_hold_every_code_of_the_protocol() {
    // Every refusal, kind and outcome this build can produce has a token of
    // its own: `other` is for what another build may send, not for these.
    for reason in koru_bot_client_wire::refusal::ALL {
        assert!(
            BotsSessionReason::from_token(reason).is_some_and(|known| known.token() == reason),
            "{reason}"
        );
    }
    for kind in crate::worker::bots::client::owner::command::ALLOWED_KINDS {
        assert!(
            BotsCommandKind::from_token(kind).is_some_and(|known| known.token() == kind),
            "{kind}"
        );
    }
    use crate::worker::bots::client::owner::command::{
        BUSY, EXECUTING_COMPUTER_ONLY, NOT_APPLIED, UNCERTAIN_CODES,
    };
    for code in [BUSY, EXECUTING_COMPUTER_ONLY, NOT_APPLIED]
        .into_iter()
        .chain(UNCERTAIN_CODES)
        .chain([
            BotError::Invalid.code(),
            BotError::NotFound.code(),
            BotError::Forbidden.code(),
            BotError::Unavailable.code(),
            BotError::Conflict.code(),
            BotError::RevisionConflict.code(),
            BotError::FolderLocked.code(),
            BotError::Storage.code(),
            BotError::Settling.code(),
            BotError::TeamPaused.code(),
            BotError::TeamRevisionConflict.code(),
        ])
    {
        assert!(
            BotsCommandFailure::from_token(code).is_some_and(|known| known.token() == code),
            "{code}"
        );
    }
    for outcome in [
        ClientOutcome::Completed,
        ClientOutcome::Accepted,
        ClientOutcome::Refused,
        ClientOutcome::Unknown,
    ] {
        assert!(BotsCommandOutcome::from_token(outcome.token()).is_some());
    }
    for state in [
        LinkedOwnerState::Never,
        LinkedOwnerState::Current,
        LinkedOwnerState::Stale,
        LinkedOwnerState::Unsupported,
        LinkedOwnerState::Withdrawing,
    ] {
        assert_eq!(mirror_state(state).token(), state.as_str());
    }
    for stage in [TeamPauseStage::Pending, TeamPauseStage::Acknowledged] {
        assert!(BotsTeamPauseStage::from_token(stage.as_str()).is_some());
    }
    for role in [Role::Feed, Role::Command, Role::Live] {
        assert_eq!(role_of(role).token(), role.as_str());
    }
}

#[test]
fn m8_events_content_free_a_worker_with_no_sink_publishes_and_reads_nothing() {
    let events = BotEvents::default();
    assert!(!events.enabled());
    let device = DeviceId::new();
    events.team_pause(device, 1, true, TeamPauseStage::Pending);
    events.reset(device, GapReason::FeedRewound);
    events.mirror_removed(device);
    drop(SessionLog::new(events, device, Role::Feed));
}
