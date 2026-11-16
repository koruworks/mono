// SPDX-License-Identifier: MPL-2.0
use super::*;

fn round_trips<T: Copy + PartialEq + std::fmt::Debug + Serialize>(
    all: &[T],
    token: fn(T) -> &'static str,
    parse: fn(&str) -> Option<T>,
) {
    let mut seen = std::collections::BTreeSet::new();
    for value in all {
        assert!(seen.insert(token(*value)), "duplicate token");
        assert_eq!(parse(token(*value)), Some(*value));
        // The wire spelling and the stored token are the same word.
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::json!(token(*value))
        );
    }
    assert_eq!(parse("unknown"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn every_vocabulary_round_trips_and_refuses_unknown_tokens() {
    round_trips(BotLifecycle::ALL, BotLifecycle::token, BotLifecycle::parse);
    round_trips(
        BotChatPurpose::ALL,
        BotChatPurpose::token,
        BotChatPurpose::parse,
    );
    round_trips(
        BotLifecycleKind::ALL,
        BotLifecycleKind::token,
        BotLifecycleKind::parse,
    );
    round_trips(
        BotLifecycleStage::ALL,
        BotLifecycleStage::token,
        BotLifecycleStage::parse,
    );
    round_trips(
        BotLifecycleRefusal::ALL,
        BotLifecycleRefusal::token,
        BotLifecycleRefusal::parse,
    );
    round_trips(
        BotLifecycleBarrierState::ALL,
        BotLifecycleBarrierState::token,
        BotLifecycleBarrierState::parse,
    );
    round_trips(
        BotLifecycleMemberKind::ALL,
        BotLifecycleMemberKind::token,
        BotLifecycleMemberKind::parse,
    );
    round_trips(
        BotLifecycleOwner::ALL,
        BotLifecycleOwner::token,
        BotLifecycleOwner::parse,
    );
    round_trips(
        BotLifecycleReason::ALL,
        BotLifecycleReason::token,
        BotLifecycleReason::parse,
    );
    round_trips(
        BotLifecyclePriorState::ALL,
        BotLifecyclePriorState::token,
        BotLifecyclePriorState::parse,
    );
    round_trips(
        BotLifecycleIntended::ALL,
        BotLifecycleIntended::token,
        BotLifecycleIntended::parse,
    );
    round_trips(
        BotLifecycleMemberStage::ALL,
        BotLifecycleMemberStage::token,
        BotLifecycleMemberStage::parse,
    );
    round_trips(
        BotLifecycleRetained::ALL,
        BotLifecycleRetained::token,
        BotLifecycleRetained::parse,
    );
    round_trips(
        BotTombstoneSubject::ALL,
        BotTombstoneSubject::token,
        BotTombstoneSubject::parse,
    );
}

#[test]
fn lifecycle_routine_subject_is_a_delegate_member_named_by_its_schedule_document() {
    use crate::{DocumentId, WorkspaceId};
    let subject = BotLifecycleSubject::Routine {
        workspace_id: WorkspaceId::new(),
        document_id: DocumentId::new(),
    };
    assert_eq!(subject.kind(), BotLifecycleMemberKind::Routine);
    assert_eq!(subject.owner(), BotLifecycleOwner::Delegate);
    assert_eq!(BotLifecycleMemberKind::Routine.token(), "routine");
    assert_eq!(BotLifecycleReason::OwnedRoutine.token(), "owned_routine");
    let json = serde_json::to_value(&subject).unwrap();
    assert_eq!(json["subject"], serde_json::json!("routine"));
    assert_eq!(
        serde_json::from_value::<BotLifecycleSubject>(json.clone()).unwrap(),
        subject
    );
    // Strict: a Routine member carries no conversation or Run identity.
    let mut extra = json;
    extra["chat_id"] = serde_json::json!(crate::ChatId::new());
    assert!(serde_json::from_value::<BotLifecycleSubject>(extra).is_err());
    // Every other kind keeps the owner it had.
    assert_eq!(BotLifecycleSubject::Bot.owner(), BotLifecycleOwner::Ai);
    assert_eq!(
        BotLifecycleSubject::OwnedRole {
            workspace_id: WorkspaceId::new(),
            document_id: DocumentId::new()
        }
        .owner(),
        BotLifecycleOwner::Store
    );
}

#[test]
fn kind_rules_match_the_stored_constraints() {
    use BotLifecycleKind::*;
    for kind in BotLifecycleKind::ALL {
        assert_eq!(
            kind.names_chat(),
            matches!(kind, TrashChat | RestoreChat | PurgeChat)
        );
        assert_eq!(
            kind.needs_source(),
            matches!(kind, Restore | RestoreChat | PurgeBot | PurgeChat)
        );
        for prior in BotLifecycle::ALL {
            assert_eq!(
                kind.pauses(*prior),
                matches!(kind, Archive | Trash) && *prior == BotLifecycle::Active
            );
        }
    }
}

#[test]
fn lifecycle_commands_are_strict_and_name_their_kind() {
    use crate::bots::BotCommand;
    use crate::{ChatId, WorkspaceId};
    let (request_id, bot_id, chat_id) = (BotRequestId::mint(), BotId::mint(), ChatId::new());
    let source_request_id = BotRequestId::mint();
    let commands = [
        (
            BotCommand::Archive {
                request_id,
                bot_id,
                expected_revision: 1,
            },
            Some(BotLifecycleKind::Archive),
        ),
        (
            BotCommand::Trash {
                request_id,
                bot_id,
                expected_revision: 1,
            },
            Some(BotLifecycleKind::Trash),
        ),
        (
            BotCommand::Restore {
                request_id,
                bot_id,
                expected_revision: 1,
                source_request_id,
            },
            Some(BotLifecycleKind::Restore),
        ),
        (
            BotCommand::TrashChat {
                request_id,
                bot_id,
                chat_id,
                expected_revision: 1,
            },
            Some(BotLifecycleKind::TrashChat),
        ),
        (
            BotCommand::RestoreChat {
                request_id,
                bot_id,
                chat_id,
                expected_revision: 1,
                source_request_id,
            },
            Some(BotLifecycleKind::RestoreChat),
        ),
        (
            BotCommand::PurgeBot {
                request_id,
                bot_id,
                expected_revision: 1,
                source_request_id,
            },
            Some(BotLifecycleKind::PurgeBot),
        ),
        (
            BotCommand::PurgeChat {
                request_id,
                bot_id,
                chat_id,
                expected_revision: 1,
                source_request_id,
            },
            Some(BotLifecycleKind::PurgeChat),
        ),
        (
            BotCommand::ReadLifecycleOperation { request_id, bot_id },
            None,
        ),
        (
            BotCommand::ListLifecycle {
                workspace: WorkspaceId::new(),
                lifecycle: BotLifecycle::Archived,
            },
            None,
        ),
    ];
    for (command, kind) in commands {
        assert_eq!(command.lifecycle_kind(), kind);
        let mut json = serde_json::to_value(&command).unwrap();
        if let Some(kind) = kind {
            assert_eq!(json["command"], serde_json::json!(kind.token()));
        }
        assert_eq!(
            serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
            command
        );
        json["owner_device"] = serde_json::json!("caller-chosen");
        assert!(serde_json::from_value::<BotCommand>(json).is_err());
    }
}

#[test]
fn lifecycle_progress_is_strict_and_carries_no_owner_correlation() {
    let progress = BotLifecycleProgress {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        kind: BotLifecycleKind::TrashChat,
        chat_id: Some(crate::ChatId::new()),
        source_request_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::PersonHolding),
        members: vec![BotLifecycleMemberOutcome {
            subject: BotLifecycleSubject::OwnedRole {
                workspace_id: crate::WorkspaceId::new(),
                document_id: crate::DocumentId::new(),
            },
            intended: BotLifecycleIntended::Retain,
            stage: BotLifecycleMemberStage::Retained,
            retained: Some(BotLifecycleRetained::OwnershipUnproven),
        }],
        left_in_place: BotLeftInPlace::of(BotLifecycleKind::TrashChat),
        routines_needing_access: Vec::new(),
        routines_still_on: Vec::new(),
    };
    let mut json = serde_json::to_value(&progress).unwrap();
    assert_eq!(
        json["left_in_place"],
        serde_json::json!(["pages", "run_records"])
    );
    assert_eq!(json["members"][0]["subject"]["subject"], "owned_role");
    assert!(!json.to_string().contains("child_request"));
    assert_eq!(
        serde_json::from_value::<BotLifecycleProgress>(json.clone()).unwrap(),
        progress
    );
    json["members"][0]["child_request_id"] = serde_json::json!("x");
    assert!(serde_json::from_value::<BotLifecycleProgress>(json).is_err());
    for (subject, kind, owner) in [
        (
            BotLifecycleSubject::Bot,
            BotLifecycleMemberKind::Bot,
            BotLifecycleOwner::Ai,
        ),
        (
            BotLifecycleSubject::RunReference {
                run_id: koru_delegate_core::RunId::mint(),
                workspace_id: crate::WorkspaceId::new(),
                document_id: None,
                chat_id: None,
            },
            BotLifecycleMemberKind::RunReference,
            BotLifecycleOwner::Delegate,
        ),
    ] {
        assert_eq!((subject.kind(), subject.owner()), (kind, owner));
    }
}

/// `reversed` is the one new value on the wire (additive), and it is final:
/// only the stages before `completed` are moving.
#[test]
fn a_reversed_operation_is_final_and_says_so_on_the_wire() {
    use BotLifecycleStage::*;
    assert_eq!(
        serde_json::to_value(Reversed).unwrap(),
        serde_json::json!("reversed")
    );
    assert_eq!(
        serde_json::from_value::<BotLifecycleStage>(serde_json::json!("reversed")).unwrap(),
        Reversed
    );
    let moving: Vec<_> = BotLifecycleStage::ALL
        .iter()
        .filter(|stage| stage.moving())
        .copied()
        .collect();
    assert_eq!(
        moving,
        vec![
            Captured,
            Settling,
            Applying,
            Restoring,
            PreservingReferences,
            Purging
        ]
    );
}

#[test]
fn only_trash_says_what_it_left_in_place_and_archive_serializes_as_before() {
    use BotLifecycleKind::*;
    for kind in BotLifecycleKind::ALL {
        let left = BotLeftInPlace::of(*kind);
        assert_eq!(
            left,
            if matches!(kind, Trash | TrashChat) {
                vec![BotLeftInPlace::Pages, BotLeftInPlace::RunRecords]
            } else {
                vec![]
            },
            "{kind:?}"
        );
    }
    // An Archive result has no such field on the wire, as before (additive).
    let archive = BotLifecycleProgress {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        kind: Archive,
        chat_id: None,
        source_request_id: None,
        stage: BotLifecycleStage::Completed,
        refusal: None,
        members: vec![],
        left_in_place: BotLeftInPlace::of(Archive),
        routines_needing_access: Vec::new(),
        routines_still_on: Vec::new(),
    };
    let json = serde_json::to_value(&archive).unwrap();
    assert!(json.get("left_in_place").is_none());
    assert_eq!(
        serde_json::from_value::<BotLifecycleProgress>(json).unwrap(),
        archive
    );
}
