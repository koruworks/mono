// SPDX-License-Identifier: MPL-2.0

//! The runtime half of the private commands, over the real in-memory ledger
//! and the scripted session host (the public decision-chain builder). The
//! scenario tests through the real actor loop live in `koru-delegate-host`;
//! these hold what is the orchestrator's own: replay first, the settle hook's
//! idempotence and the one settled event per effect.

use super::*;
use crate::test_support::{
    SettledEvents, granted_private_run, private_decision_chain, private_proposal,
};
use koru_delegate_core::{
    PrivateEffectDisposition, PrivateEffectRefusal, PrivateEffectStage, PrivateEffectTransition,
};

fn code(error: RunError) -> String {
    match error {
        RunError::Port(error) => error.code,
        other => format!("{other:?}"),
    }
}

#[test]
fn m5_private_actor_prepare_decision_replays_first_and_refuses_another_meaning() {
    let mut chain = private_decision_chain();
    let prepared = chain.prepared.clone();
    let proposal = prepared.private.clone().expect("a private decision");
    let device = prepared.owner.device;
    // Exact replay: the retained decision, under a boot that is no longer
    // current and without a fresh publication check.
    let stale_boot = koru_delegate_core::DelegateBootId::mint();
    let replay = chain
        .orchestrator
        .bot_prepare_private_decision(
            &prepared.fence,
            device,
            &proposal,
            &prepared.proof,
            stale_boot,
        )
        .unwrap();
    assert_eq!(replay.receipt.decision, prepared.decision);
    assert!(chain.waits.get(prepared.run).is_some(), "the fence is up");
    // The same operation id with another proposal is a conflict.
    let mut other = private_proposal();
    other.operation_id = proposal.operation_id;
    other.connection_revision = koru_delegate_core::decisions::DecisionRevision::new(2).unwrap();
    assert_eq!(
        code(
            chain
                .orchestrator
                .bot_prepare_private_decision(
                    &prepared.fence,
                    device,
                    &other,
                    &prepared.proof,
                    prepared.boot
                )
                .unwrap_err()
        ),
        "bot_conflict"
    );
    // Another operation while the decision owns the Run.
    assert_eq!(
        code(
            chain
                .orchestrator
                .bot_prepare_private_decision(
                    &prepared.fence,
                    device,
                    &private_proposal(),
                    &prepared.proof,
                    prepared.boot
                )
                .unwrap_err()
        ),
        "bot_decision_owned"
    );
    // A proposal whose fingerprint does not recompute is never a question.
    let mut forged = proposal.clone();
    forged.request_fingerprint = [1; 32];
    assert_eq!(
        code(
            chain
                .orchestrator
                .bot_prepare_private_decision(
                    &prepared.fence,
                    device,
                    &forged,
                    &prepared.proof,
                    prepared.boot
                )
                .unwrap_err()
        ),
        "bot_invalid"
    );
    // Another device is refused before anything is read.
    assert_eq!(
        code(
            chain
                .orchestrator
                .bot_prepare_private_decision(
                    &prepared.fence,
                    koru_delegate_core::DeviceId::new(),
                    &proposal,
                    &prepared.proof,
                    prepared.boot
                )
                .unwrap_err()
        ),
        "bot_owner_mismatch"
    );
    // The decision asked nothing of an effect: no row exists.
    assert!(
        chain
            .orchestrator
            .private_ledger()
            .unwrap()
            .private_effect(prepared.run, proposal.operation_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn m5_events_content_free_one_settled_event_per_effect_under_one_derived_id() {
    let mut run = granted_private_run();
    let events = SettledEvents::install(&mut run.orchestrator);
    let device = run.owner.device;
    let dispatcher = PrivateDispatcherId::mint();
    let prepared = run
        .orchestrator
        .bot_prepare_private_effect(
            &run.fence,
            device,
            dispatcher,
            &run.proposal,
            &run.proof,
            run.boot,
        )
        .unwrap();
    assert_eq!(prepared.disposition, PrivateEffectDisposition::Execute);
    let advance = |orchestrator: &mut Orchestrator, transition| {
        orchestrator.bot_advance_private_effect(&prepared.key, dispatcher, &transition, device)
    };
    advance(
        &mut run.orchestrator,
        PrivateEffectTransition::Dispatched {},
    )
    .unwrap();
    assert!(events.published().is_empty(), "an open stage is no fact");
    // The bus refuses the first delivery: the row is final all the same, and
    // the next observation of it publishes the same id.
    events.refuse(true);
    let confirmed = PrivateEffectTransition::Confirmed {
        outcome_bytes: 77,
        withheld: false,
    };
    let first = advance(&mut run.orchestrator, confirmed).unwrap();
    assert!(first.deliver);
    assert!(events.published().is_empty());
    events.refuse(false);
    for _ in 0..3 {
        // The exact replay, a read and a prepare replay each observe the row.
        let replay = advance(&mut run.orchestrator, confirmed).unwrap();
        assert!(!replay.deliver, "the one deliver went with the commit");
        run.orchestrator
            .bot_read_private_effect(run.run, run.proposal.operation_id, dispatcher, device)
            .unwrap()
            .unwrap();
        run.orchestrator
            .bot_prepare_private_effect(
                &run.fence,
                device,
                dispatcher,
                &run.proposal,
                &run.proof,
                run.boot,
            )
            .unwrap();
    }
    let published = events.published();
    assert_eq!(published.len(), 1, "one settled event per effect");
    let (id, fact) = published[0];
    assert_eq!(id, fact.event_id());
    assert_eq!(
        (
            fact.run,
            fact.operation,
            fact.connection,
            fact.stage,
            fact.refusal,
            fact.withheld,
            fact.outcome_bytes,
            fact.decision
        ),
        (
            run.run,
            run.proposal.operation_id,
            run.proposal.connection,
            PrivateEffectStage::Confirmed,
            None,
            Some(false),
            Some(77),
            None
        )
    );
    // The id is a function of the Run and the operation only, and a v4 UUID.
    let mut other = fact;
    other.stage = PrivateEffectStage::Unknown;
    other.outcome_bytes = None;
    assert_eq!(other.event_id(), id);
    other.operation = uuid::Uuid::new_v4();
    assert_ne!(other.event_id(), id);
    assert_eq!(id.0.get_version_num(), 4);
    // The fact's Debug holds identifiers, tokens and numbers only.
    let debug = format!("{fact:?}");
    assert!(debug.contains("Confirmed"));
    assert!(!debug.contains("task"));
}

#[test]
fn m5_events_content_free_a_refused_effect_names_its_code_and_nothing_else() {
    let mut run = granted_private_run();
    let events = SettledEvents::install(&mut run.orchestrator);
    let device = run.owner.device;
    let dispatcher = PrivateDispatcherId::mint();
    let prepared = run
        .orchestrator
        .bot_prepare_private_effect(
            &run.fence,
            device,
            dispatcher,
            &run.proposal,
            &run.proof,
            run.boot,
        )
        .unwrap();
    let refused = run
        .orchestrator
        .bot_advance_private_effect(
            &prepared.key,
            dispatcher,
            &PrivateEffectTransition::Refused {
                code: PrivateEffectRefusal::CredentialLocked,
            },
            device,
        )
        .unwrap();
    assert_eq!(refused.stage, PrivateEffectStage::Refused);
    assert!(!refused.deliver);
    let published = events.published();
    assert_eq!(published.len(), 1);
    let fact = published[0].1;
    assert_eq!(fact.stage, PrivateEffectStage::Refused);
    assert_eq!(fact.refusal, Some(PrivateEffectRefusal::CredentialLocked));
    assert_eq!((fact.withheld, fact.outcome_bytes), (None, None));
}
