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
