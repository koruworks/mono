// SPDX-License-Identifier: MPL-2.0

//! Every current builder produces what the registry declares. Retired rows
//! remain registered so already stored journal envelopes still validate.

use koru_events::{REGISTRY, TelemetryPolicy, validate};

use super::*;

#[test]
fn every_row_validates_against_the_registry() {
    for (name, envelope) in every_row() {
        assert_eq!(
            envelope.name.as_str(),
            name,
            "a builder spelled another row"
        );
        validate(&envelope).unwrap_or_else(|violation| {
            panic!(
                "`{name}` was refused by the boundary: {violation} ({})",
                violation.code()
            )
        });
    }
}

/// The charter in one assertion: the set of names the builders produce is
/// exactly the current `delegate.*` rows, each once. The three retired Bot
/// rows retain their stored vocabulary but cannot be constructed here.
#[test]
fn every_delegate_row_has_exactly_one_builder() {
    let registered: BTreeSet<&str> = REGISTRY
        .iter()
        .map(|spec| spec.name)
        .filter(|name| name.starts_with("delegate."))
        .filter(|name| {
            !matches!(
                *name,
                "delegate.private_operation.settled"
                    | "delegate.child.requested"
                    | "delegate.child.changed"
            )
        })
        .collect();
    let built: Vec<&str> = every_row().into_iter().map(|(name, _)| name).collect();
    let built_set: BTreeSet<&str> = built.iter().copied().collect();
    assert_eq!(built.len(), built_set.len(), "a row was built twice");
    assert_eq!(built_set, registered);
    assert_eq!(
        registered.len(),
        32,
        "the ordinary Run and Schedule rows, exactly"
    );
}

/// D41: every row stays on this machine. The class is domain — 90 days in
/// the journal, and the Run document keeps the history forever (D32).
#[test]
fn every_delegate_row_is_domain_and_never_exports() {
    for spec in REGISTRY
        .iter()
        .filter(|spec| spec.name.starts_with("delegate."))
    {
        assert_eq!(spec.class, koru_events::EventClass::Domain, "{}", spec.name);
        assert_eq!(spec.telemetry, TelemetryPolicy::Never, "{}", spec.name);
        assert!(!spec.telemetry.exportable(), "{}", spec.name);
    }
}

/// Run rows carry their RunId and schedule rows carry their ScheduleId. Only a
/// fired schedule has a real run; skipped occurrences never expose the planned
/// id that did not become one.
#[test]
fn run_and_schedule_identities_ride_their_own_rows() {
    let run = koru_events::AttrValue::Id(run_ref().run.as_uuid());
    let schedule = koru_events::AttrValue::Id(schedule_ref().schedule.as_uuid());
    for (name, envelope) in every_row() {
        if name.starts_with("delegate.schedule.") {
            assert_eq!(envelope.attrs.get("schedule"), Some(&schedule), "{name}");
            if name == "delegate.schedule.fired" {
                assert_eq!(
                    envelope.attrs.get("run"),
                    Some(&koru_events::AttrValue::Id(scheduled_run().as_uuid()))
                );
            } else {
                assert!(!envelope.attrs.contains_key("run"), "{name}");
            }
        } else if name == "delegate.scope.skipped" {
            assert!(!envelope.attrs.contains_key("run"));
            assert!(envelope.attrs.contains_key("schedule"));
        } else {
            assert_eq!(envelope.attrs.get("run"), Some(&run), "{name}");
        }
    }
}

#[test]
fn optional_schedule_details_follow_their_closed_reason() {
    let chain = schedule_chain();
    let manual = chain.paused(
        stamp(40),
        SchedulePaused {
            revision: 4,
            reason: SchedulePauseReason::Manual,
        },
    );
    assert!(!manual.attrs.contains_key("context_item"));
    assert!(!manual.attrs.contains_key("stage"));

    let missed = chain.skipped(
        stamp(41),
        ScheduleSkipped {
            revision: 4,
            occurrence_ms: T0,
            reason: ScheduleSkipReason::missed(),
        },
    );
    assert!(!missed.attrs.contains_key("holder"));

    let busy = chain.skipped(
        stamp(42),
        ScheduleSkipped {
            revision: 4,
            occurrence_ms: T0,
            reason: ScheduleSkipReason::busy(run_ref().run),
        },
    );
    assert_eq!(
        busy.attrs.get("holder"),
        Some(&koru_events::AttrValue::Id(run_ref().run.as_uuid()))
    );
}

/// D29 is derived, not asked: zero artifacts is `needs_attention`.
#[test]
fn a_completion_without_an_artifact_needs_attention() {
    let chain = chain();
    let none = chain.completed(
        stamp(40),
        RunCompleted {
            attempt: attempt(),
            generation: AttemptGeneration::FIRST,
            artifacts: 0,
        },
    );
    let one = chain.completed(
        stamp(41),
        RunCompleted {
            attempt: attempt(),
            generation: AttemptGeneration::FIRST,
            artifacts: 1,
        },
    );
    let outcome = |envelope: &Envelope| match envelope.attrs.get("outcome") {
        Some(koru_events::AttrValue::Token(token)) => token.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(outcome(&none), "needs_attention");
    assert_eq!(outcome(&one), "delivered");
}

/// A failure before any attempt existed carries no `attempt`, and the
/// registry accepts that: the attribute is declared optional, not defaulted.
#[test]
fn a_failure_before_any_attempt_has_no_attempt() {
    let envelope = chain().failed(
        stamp(42),
        RunFailed {
            attempt: None,
            generation: AttemptGeneration::FIRST,
            stage: FailureStage::ScopeAdmission,
        },
    );
    assert!(!envelope.attrs.contains_key("attempt"));
    validate(&envelope).unwrap();
}

/// A gap that reported no count carries no count — unknown is never zero.
#[test]
fn an_uncounted_gap_carries_no_count() {
    let chain = chain();
    let unknown = chain.timeline_gap(
        stamp(43),
        TimelineGap {
            attempt: attempt(),
            events_lost: None,
        },
    );
    let counted = chain.timeline_gap(
        stamp(44),
        TimelineGap {
            attempt: attempt(),
            events_lost: Some(12),
        },
    );
    assert!(!unknown.attrs.contains_key("events_lost"));
    assert_eq!(
        counted.attrs.get("events_lost"),
        Some(&koru_events::AttrValue::Count(12))
    );
}
