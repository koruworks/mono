// SPDX-License-Identifier: MPL-2.0
//! The owner controls' contract types (P4.3): closed commands, the Test
//! minute, the history page and this computer's zone. Pure values only.
use super::*;
use crate::bots::{BotAccess, BotCommand, BotResponse};
use koru_delegate_schedule::{ScheduleName, ScheduleState};

const MINUTE: i64 = 60_000;

fn floating() -> RecurrenceRule {
    RecurrenceRule::floating("0 9 * * *".parse().unwrap())
}

#[test]
fn routine_control_commands_are_closed_and_carry_no_owner_or_authority_field() {
    let bot_id = BotId::from_uuid(id(4));
    let schedule_id = ScheduleId::from_uuid(id(5));
    let commands = [
        BotCommand::SetRoutineLifecycle {
            bot_id,
            schedule_id,
            expected_definition_revision: 1,
            action: BotRoutineLifecycleAction::Enable,
        },
        BotCommand::EditRoutine {
            bot_id,
            schedule_id,
            expected_definition_revision: 2,
            expected_name: ScheduleName::new("Before").unwrap(),
            name: ScheduleName::new("After").unwrap(),
            recurrence: floating(),
        },
        BotCommand::UpdateRoutineAccess {
            request_id: BotRequestId::from_uuid(id(6)),
            bot_id,
            schedule_id,
            expected_grant_revision: 1,
            access: BotAccess::Workspace,
            selected_pages: vec![],
        },
    ];
    for command in commands {
        assert_eq!(command.routine_control(), Some((bot_id, schedule_id)));
        let json = serde_json::to_value(&command).unwrap();
        assert_eq!(
            serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
            command
        );
        // No caller can name an owner, a device, a grant, a template, an
        // effective time or a minute: each is an unknown field.
        for forbidden in [
            "owner",
            "owner_device",
            "user_scope",
            "grant",
            "template",
            "objective",
            "effective_at_ms",
            "occurrence_ms",
            "route",
        ] {
            let mut changed = json.clone();
            changed
                .as_object_mut()
                .unwrap()
                .insert(forbidden.into(), serde_json::json!("x"));
            assert!(
                serde_json::from_value::<BotCommand>(changed).is_err(),
                "{forbidden}"
            );
        }
    }
    // The lifecycle action is enable or pause and nothing else.
    for action in ["edit", "test", "resume", "delete"] {
        let json = serde_json::json!({
            "command": "set_routine_lifecycle",
            "bot_id": bot_id,
            "schedule_id": schedule_id,
            "expected_definition_revision": 1,
            "action": action,
        });
        assert!(serde_json::from_value::<BotCommand>(json).is_err());
    }
    assert_eq!(BotCommand::Read { bot_id }.routine_control(), None);
}

#[test]
fn routine_control_replies_roundtrip_with_their_stable_spelling() {
    let schedule_id = ScheduleId::from_uuid(id(5));
    let control = BotResponse::RoutineControl {
        control: BotRoutineControl {
            schedule_id,
            action: BotRoutineControlAction::Enable,
            outcome: RoutineControlOutcome::AlreadyApplied,
            definition_revision: 3,
            state: routine_control_state(
                RoutineControlLifecycle::Active,
                DeviceId::from_uuid(id(2)),
            ),
            next: RoutineNextObservation::Exact { at_ms: MINUTE },
        },
    };
    let json = serde_json::to_value(&control).unwrap();
    assert_eq!(json["result"], "routine_control");
    assert_eq!(json["control"]["action"], "enable");
    assert_eq!(json["control"]["outcome"], "already_applied");
    assert_eq!(json["control"]["state"]["state"], "active");
    assert_eq!(json["control"]["next"]["next"], "exact");
    assert_eq!(
        serde_json::from_value::<BotResponse>(json).unwrap(),
        control
    );
    // An owner control only ever leaves a manual pause.
    assert_eq!(
        routine_control_state(RoutineControlLifecycle::Paused, DeviceId::from_uuid(id(2))),
        ScheduleState::Paused {
            owner: DeviceId::from_uuid(id(2)),
            reason: SchedulePauseReason::Manual,
        }
    );
    let access = BotResponse::RoutineAccess {
        access: BotRoutineAccess::of(
            schedule_id,
            &BotRoutineGrantSummary {
                source_bot_policy_revision: 4,
                grant_revision: 2,
                access: BotAccess::Workspace,
                selected_page_count: 3,
            },
        ),
    };
    let json = serde_json::to_value(&access).unwrap();
    assert_eq!(json["result"], "routine_access");
    assert_eq!(json["access"]["grant_revision"], 2);
    // The reply names no page and has no pending state.
    assert_eq!(json["access"].as_object().unwrap().len(), 4);
    assert_eq!(serde_json::from_value::<BotResponse>(json).unwrap(), access);
    assert!(!format!("{control:?}").contains("active"));
}

#[test]
fn routine_control_not_ready_reasons_have_one_order_and_stable_tokens() {
    use BotRoutineNotReadyReason::*;
    let mut reasons = vec![
        AccessPending,
        CreationNotBound,
        BotArchived,
        AccessUnavailable,
        BotPaused,
    ];
    reasons.sort();
    assert_eq!(
        reasons.iter().map(|r| r.token()).collect::<Vec<_>>(),
        [
            "bot_paused",
            "bot_archived",
            "creation_not_bound",
            "access_unavailable",
            "access_pending"
        ]
    );
    for reason in reasons {
        assert_eq!(serde_json::to_value(reason).unwrap(), reason.token());
    }
}

fn line(minute: i64, outcome: RoutineOccurrenceOutcome) -> RoutineFiringLine {
    RoutineFiringLine {
        occurrence_ms: minute * MINUTE,
        outcome,
        recorded_at_ms: minute * MINUTE + 7,
    }
}

#[test]
fn routine_control_history_pages_unordered_firings_newest_first_below_the_cursor() {
    use RoutineOccurrenceOutcome::*;
    // Merge order, not time order: the last entries are not the newest.
    let firings: Vec<_> = [
        7, 2, 30, 11, 4, 19, 1, 25, 3, 28, 6, 5, 9, 8, 10, 12, 13, 14, 15, 16,
    ]
    .into_iter()
    .map(|minute| line(minute, if minute % 3 == 0 { Missed } else { Ran }))
    .collect();
    let RoutineFiringsObservation::Available {
        lines,
        next_before_occurrence_ms,
    } = RoutineFiringsObservation::page(firings.clone(), None, 16)
    else {
        panic!("available")
    };
    assert_eq!(lines.len(), 16);
    assert!(
        lines
            .windows(2)
            .all(|w| w[0].occurrence_ms > w[1].occurrence_ms)
    );
    assert_eq!(lines[0].occurrence_ms, 30 * MINUTE);
    assert_eq!(next_before_occurrence_ms, Some(lines[15].occurrence_ms));
    // The next page starts strictly below the cursor and ends the list.
    let RoutineFiringsObservation::Available {
        lines: rest,
        next_before_occurrence_ms: end,
    } = RoutineFiringsObservation::page(firings.clone(), next_before_occurrence_ms, 16)
    else {
        panic!("available")
    };
    assert_eq!(rest.len(), 4);
    assert!(
        rest.iter()
            .all(|l| l.occurrence_ms < lines[15].occurrence_ms)
    );
    assert_eq!(end, None);
    // A smaller limit pages the same list; the limit is clamped to 1..=16.
    let RoutineFiringsObservation::Available { lines: two, .. } =
        RoutineFiringsObservation::page(firings.clone(), None, 2)
    else {
        panic!("available")
    };
    assert_eq!(
        two.iter().map(|l| l.occurrence_ms).collect::<Vec<_>>(),
        [30 * MINUTE, 28 * MINUTE]
    );
    let RoutineFiringsObservation::Available { lines: capped, .. } =
        RoutineFiringsObservation::page(firings, None, u8::MAX)
    else {
        panic!("available")
    };
    assert_eq!(capped.len(), BOT_ROUTINE_HISTORY_LIMIT);
    assert_eq!(
        RoutineFiringsObservation::page(Vec::new(), None, 16),
        RoutineFiringsObservation::Available {
            lines: vec![],
            next_before_occurrence_ms: None
        }
    );
}

#[test]
fn routine_control_history_joins_local_conversations_and_marks_a_test_by_minute() {
    use RoutineOccurrenceOutcome::*;
    let local = |minute: i64| BotRoutineLocalConversation {
        occurrence_ms: minute * MINUTE,
        run_id: RunId::mint(),
        chat_id: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
        conversation: BotRoutineConversationVisibility::Available,
    };
    let rows = vec![(local(5), false), (local(4), true), (local(3), true)];
    let page = RoutineFiringsObservation::page(
        vec![
            line(2, Missed),
            line(5, Ran),
            // A Test that ran, and a Test that found the Bot busy.
            line(4, Ran),
            line(3, SkippedBusy),
            // Another computer's firing: no local row.
            line(1, Ran),
        ],
        None,
        16,
    );
    let RoutineOccurrenceHistoryObservation::Available {
        items,
        next_before_occurrence_ms,
    } = page.history(&rows)
    else {
        panic!("available")
    };
    assert_eq!(next_before_occurrence_ms, None);
    let seen: Vec<_> = items
        .iter()
        .map(|l| {
            (
                l.occurrence_ms / MINUTE,
                l.outcome,
                l.test,
                l.conversation.is_some(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (5, Ran, false, true),
            (4, Ran, true, true),
            // Missed work and a busy skip are lines, with nothing to open.
            (3, SkippedBusy, true, false),
            (2, Missed, false, false),
            (1, Ran, false, false),
        ]
    );
    assert_eq!(items[0].recorded_at_ms, 5 * MINUTE + 7);
    assert_eq!(items[0].conversation.as_ref(), Some(&rows[0].0));
    // A firing list that did not decode is unavailable, never empty.
    assert_eq!(
        RoutineFiringsObservation::Unavailable.history(&rows),
        RoutineOccurrenceHistoryObservation::Unavailable {}
    );
    // The wire closes each line and bounds the page at sixteen.
    let json = serde_json::to_value(page.history(&rows)).unwrap();
    assert_eq!(json["items"][1]["outcome"], "ran");
    assert_eq!(json["items"][2]["outcome"], "skipped_busy");
    assert_eq!(json["items"][3]["outcome"], "missed");
    let mut extra = json.clone();
    extra["items"][0]["catch_up"] = serde_json::json!(true);
    assert!(serde_json::from_value::<RoutineOccurrenceHistoryObservation>(extra).is_err());
    let mut long = json.clone();
    let one = long["items"][0].clone();
    long["items"] = serde_json::Value::Array(vec![one; 17]);
    assert!(serde_json::from_value::<RoutineOccurrenceHistoryObservation>(long).is_err());
    assert_eq!(
        serde_json::from_value::<RoutineOccurrenceHistoryObservation>(json).unwrap(),
        page.history(&rows)
    );
}

#[test]
fn routine_control_zone_is_this_computer_only_for_a_floating_rule_it_owns() {
    let here = DeviceId::from_uuid(id(2));
    let elsewhere = DeviceId::from_uuid(id(9));
    let this = RoutineThisComputer::detected(here, "Europe/Zurich").unwrap();
    // A name the zone database does not know is no zone at all.
    assert!(RoutineThisComputer::detected(here, "Mars/Olympus").is_none());
    let active = |owner| ScheduleState::Active { owner };
    let paused = |owner| ScheduleState::Paused {
        owner,
        reason: SchedulePauseReason::Manual,
    };
    let observed = 1_700_000_000_000;

    // Owned here and on: this computer's zone, and an exact next time in it.
    let (zone, next) = routine_zone_and_next(&floating(), &active(here), 0, observed, Some(&this));
    assert_eq!(
        zone,
        RoutineOwnerZoneObservation::ThisComputer {
            iana: "Europe/Zurich".into()
        }
    );
    let RoutineNextObservation::Exact { at_ms } = next else {
        panic!("exact")
    };
    assert!(at_ms > observed && at_ms <= observed + 24 * 60 * MINUTE);
    // The same rule read in another zone is another instant: it is this
    // computer's zone that was used, not a constant.
    let tokyo = RoutineThisComputer::detected(here, "Asia/Tokyo").unwrap();
    let (_, other) = routine_zone_and_next(&floating(), &active(here), 0, observed, Some(&tokyo));
    assert_ne!(other, RoutineNextObservation::Exact { at_ms });

    // Owned here and paused: the zone is shown, and nothing is next.
    assert_eq!(
        routine_zone_and_next(&floating(), &paused(here), 0, observed, Some(&this)),
        (
            RoutineOwnerZoneObservation::ThisComputer {
                iana: "Europe/Zurich".into()
            },
            RoutineNextObservation::Inactive {}
        )
    );
    // Owned elsewhere: owner local, and the viewer's zone is never used.
    assert_eq!(
        routine_zone_and_next(&floating(), &active(elsewhere), 0, observed, Some(&this)),
        (
            RoutineOwnerZoneObservation::OwnerLocal {},
            RoutineNextObservation::OwnerLocal {}
        )
    );
    // A pending handoff has no owner this computer may speak for.
    assert_eq!(
        routine_zone_and_next(
            &floating(),
            &ScheduleState::HandoffPending {
                owner: here,
                requests: vec![]
            },
            0,
            observed,
            Some(&this)
        )
        .0,
        RoutineOwnerZoneObservation::OwnerLocal {}
    );
    // No zone source: as before this change.
    assert_eq!(
        routine_zone_and_next(&floating(), &active(here), 0, observed, None),
        (
            RoutineOwnerZoneObservation::OwnerLocal {},
            RoutineNextObservation::OwnerLocal {}
        )
    );
    // A fixed rule is unchanged, whoever reads it.
    let fixed = RecurrenceRule::zoned("0 9 * * *".parse().unwrap(), "America/New_York").unwrap();
    let (zone, next) = routine_zone_and_next(&fixed, &active(elsewhere), 0, observed, Some(&this));
    assert_eq!(
        zone,
        RoutineOwnerZoneObservation::Fixed {
            iana: "America/New_York".into()
        }
    );
    assert!(matches!(next, RoutineNextObservation::Exact { .. }));
    // The detail and a list row compute the next time the same way.
    let detail = RoutineDetailObservation::Current {
        name: ScheduleName::new("Morning").unwrap(),
        definition_revision: 2,
        effective_at_ms: 0,
        recurrence: floating(),
        state: active(here),
    }
    .live_on(observed, Some(&this));
    let row = BotRoutineLiveObservation::observed(
        RoutineOverviewObservation::Current {
            name: ScheduleName::new("Morning").unwrap(),
            definition_revision: 2,
            state: active(here),
            effective_at_ms: 0,
            recurrence: floating(),
        },
        observed,
        Some(&this),
    );
    let (
        RoutineDetailLive::Current {
            next: detail_next,
            owner_zone,
            ..
        },
        BotRoutineLiveObservation::Current { next: row_next, .. },
    ) = (detail, row)
    else {
        panic!("current")
    };
    assert_eq!(detail_next, row_next);
    assert_eq!(detail_next, RoutineNextObservation::Exact { at_ms });
    let json = serde_json::to_value(&owner_zone).unwrap();
    assert_eq!(json["zone"], "this_computer");
    assert_eq!(
        serde_json::from_value::<RoutineOwnerZoneObservation>(json).unwrap(),
        owner_zone
    );
}
