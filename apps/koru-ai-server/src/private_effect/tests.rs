// SPDX-License-Identifier: MPL-2.0

//! The private read against the real connection service, the scripted
//! vendor and credential store, the real ai.db writer on an in-memory
//! database, and a scripted delegate over the scripted effect ledger.
//!
//! No socket, no browser, no OS keychain, no real vendor, no file. The
//! delegate here is a double: the real actor's rules are held by its own
//! tests (`m5_private_actor`).

use super::*;
use crate::fixtures::connections::{
    CONTENT_1, LABEL_A, PrivateDelegate, PrivateWorld, SUBJECT_A, TITLE_1, captured, fence, owner,
    proof,
};
use koru_connections::PresentCaller;
use koru_delegate_core::{PrivateEffectDisposition, WorkspaceId};

struct Case {
    world: PrivateWorld,
    delegate: PrivateDelegate,
    fence: BotExecutionFence,
    proof: FreshBotPolicyProof,
    proposal: PrivateOperationProposal,
}

fn case() -> Case {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let connection = world.connect("Work files", SUBJECT_A, &["doc-1"]);
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[connection.id]);
    let proof = proof(&fence, &policy);
    let delegate = PrivateDelegate::new(&world, fence.clone(), policy);
    let proposal = world.proposal(uuid::Uuid::new_v4(), connection.id, "doc-1");
    Case {
        world,
        delegate,
        fence,
        proof,
        proposal,
    }
}

impl Case {
    /// On the world's own clock, which a test moves.
    fn budget(&self) -> Instant {
        use koru_connections::Clock;
        self.world.clock.now() + PRIVATE_READ_BUDGET
    }
    fn read(&self, approved: bool) -> Result<PrivateReadContent, PrivateReadRefusal> {
        let boot = self.delegate.script().boot;
        perform(
            &self.delegate,
            &self.world.service,
            &PrivateRead {
                fence: &self.fence,
                proposal: &self.proposal,
                proof: &self.proof,
                boot,
                approved,
            },
            &PresentCaller,
            self.budget(),
        )
    }
    fn retry(&self) -> Result<PrivateReadContent, PrivateReadRefusal> {
        let effect = retained(
            &self.delegate,
            self.fence.run,
            self.proposal.operation_id,
            self.budget(),
        )
        .unwrap()
        .expect("a retained effect");
        assert_eq!(effect.disposition, PrivateEffectDisposition::ReconcileOnly);
        reconcile(
            &self.world.service,
            self.fence.owner,
            &effect,
            &PresentCaller,
            self.budget(),
        )
    }
    fn refusal(&self, approved: bool) -> PrivateReadRefusal {
        self.read(approved).expect_err("the read was refused")
    }
}

fn code_of(code: &'static str) -> PrivateReadRefusal {
    PrivateReadRefusal::Code(code)
}

#[test]
fn m5_private_perform_a_granted_read_records_the_intent_and_the_receipt_and_reads_once() {
    let c = case();
    assert!(
        retained(
            &c.delegate,
            c.fence.run,
            c.proposal.operation_id,
            c.budget()
        )
        .unwrap()
        .is_none(),
        "a fresh operation has no retained effect"
    );
    let content = c.read(false).expect("the read is delivered");
    assert_eq!(content.text(), CONTENT_1);
    assert!(!content.truncated());
    assert_eq!(content.total_bytes(), CONTENT_1.len() as u64);
    // One lookup and one intent at the delegate; then the dispatch mark and
    // the receipt at the ledger, in that order, once each.
    assert_eq!(c.delegate.log(), ["read_effect", "prepare_effect"]);
    let advances: Vec<_> = c
        .world
        .ledger
        .advances()
        .into_iter()
        .map(|(_, stage)| stage)
        .collect();
    assert_eq!(
        advances,
        [
            Ok(PrivateEffectStage::Dispatched),
            Ok(PrivateEffectStage::Confirmed)
        ]
    );
    assert_eq!(c.world.vendor.content_reads(), 1);
    assert_eq!(c.world.service.open_obligations(), 0);

    // The exact retry reconciles. The read is never performed twice, and its
    // content is not available a second time.
    assert_eq!(
        c.retry().unwrap_err(),
        code_of("private_result_unavailable")
    );
    assert_eq!(
        c.read(false).unwrap_err(),
        code_of("private_result_unavailable")
    );
    assert_eq!(c.world.vendor.content_reads(), 1);
}

#[test]
fn m5_private_perform_no_grant_at_the_delegate_commits_nothing_and_reads_nothing() {
    let c = case();
    c.delegate.script().granted = false;
    assert_eq!(c.refusal(false), PrivateReadRefusal::NotGranted);
    assert!(c.delegate.effect(c.proposal.operation_id).is_none());
    assert_eq!(c.world.vendor.content_reads(), 0);
    assert_eq!(c.world.service.open_obligations(), 0);
}

#[test]
fn m5_private_perform_every_typed_delegate_refusal_has_one_code_and_reads_nothing() {
    for (refused_with, told) in [
        ("bot_decision_owned", "decision_in_progress"),
        ("bot_conflict", "native_operation_conflict"),
        ("bot_private_effect_open", "private_read_in_progress"),
        ("bot_private_unavailable", "private_unavailable"),
        ("bot_fenced", "native_execution_unavailable"),
        ("bot_boot_stale", "native_execution_unavailable"),
    ] {
        let c = case();
        c.delegate
            .script()
            .refuse
            .insert("prepare_effect", refused_with);
        assert_eq!(c.refusal(false), code_of(told), "{refused_with}");
        assert_eq!(c.world.vendor.content_reads(), 0, "{refused_with}");
        assert!(c.delegate.effect(c.proposal.operation_id).is_none());
    }
}

#[test]
fn m5_private_perform_a_lost_intent_reply_is_unknown_and_its_retry_closes_the_row_unread() {
    let c = case();
    c.delegate.script().lose.insert("prepare_effect");
    // The intent may be recorded: the call cannot say, and reads nothing.
    assert_eq!(c.refusal(false), code_of("private_outcome_unknown"));
    assert_eq!(c.world.vendor.content_reads(), 0);
    // The reply that carried the license is gone. The row is this process's
    // own and no call here performs it: it is closed as not dispatched, so it
    // does not stay open for the life of the process.
    assert_eq!(c.retry().unwrap_err(), code_of("private_read_not_done"));
    let row = c.delegate.effect(c.proposal.operation_id).unwrap();
    assert_eq!(row.stage, PrivateEffectStage::Refused);
    assert_eq!(row.refusal, Some(PrivateEffectRefusal::NotDispatched));
    assert_eq!(c.world.vendor.content_reads(), 0);
}

#[test]
fn m5_private_perform_a_refused_read_is_recorded_and_told_by_its_own_code() {
    // The credential store cannot be read: refused from `prepared`.
    let c = case();
    c.world.credentials.set_locked(true);
    c.world
        .clock
        .advance(std::time::Duration::from_secs(2 * 3600));
    assert_eq!(c.refusal(false), code_of("credential_locked"));
    let row = c.delegate.effect(c.proposal.operation_id).unwrap();
    assert_eq!(row.stage, PrivateEffectStage::Refused);
    assert_eq!(row.refusal, Some(PrivateEffectRefusal::CredentialLocked));
    assert_eq!(c.world.vendor.content_reads(), 0);
    // The retry is told the same thing from the retained row.
    assert_eq!(c.retry().unwrap_err(), code_of("credential_locked"));

    // Access removed before dispatch: the mark is answered as a refusal, a
    // reply and not a failure, and the vendor is never called.
    let c = case();
    let boot = c.delegate.script().boot;
    let Ok(koru_delegate_api::bots::BotReply::PrivateEffect { effect }) =
        c.delegate
            .execute(koru_delegate_api::bots::BotCommand::PreparePrivateEffect {
                fence: c.fence.clone(),
                dispatcher: koru_connections::process_dispatcher(),
                operation: c.proposal.clone(),
                proof: c.proof.clone(),
                boot,
            })
    else {
        panic!("the intent was not recorded");
    };
    c.world.ledger.remove_access();
    let ended = c
        .world
        .service
        .read(c.world.owner, &effect, &PresentCaller, c.budget());
    assert_eq!(
        outcome(ended, true).unwrap_err(),
        code_of("connection_revoked")
    );
    assert_eq!(c.world.vendor.content_reads(), 0);

    // The account disconnected before the read: nothing is dispatched.
    let c = case();
    c.world
        .service
        .disconnect(
            c.world.owner,
            koru_ai_core::bots::BotRequestId::mint(),
            c.proposal.connection,
            c.proposal.connection_revision.get(),
        )
        .unwrap();
    assert_eq!(c.refusal(false), code_of("connection_revoked"));
    assert_eq!(c.world.vendor.content_reads(), 0);
}

#[test]
fn m5_private_perform_an_approved_read_consumes_once_and_is_told_one_code_when_refused() {
    use crate::fixtures::connections::successor;
    use koru_delegate_api::bots::BotCommand;
    // The question is asked by the origin and allowed; the successor's exact
    // retry consumes it and performs the read in the same call.
    let ask = |c: &Case| {
        let boot = c.delegate.script().boot;
        c.delegate
            .execute(BotCommand::PreparePrivateDecision {
                fence: c.fence.clone(),
                operation: c.proposal.clone(),
                proof: c.proof.clone(),
                boot,
            })
            .unwrap();
    };
    let ungranted = || {
        let mut c = case();
        c.delegate.script().captured.connections.clear();
        ask(&c);
        let next = successor(&c.fence);
        c.delegate.allow(next.clone());
        c.fence = next;
        c
    };
    let c = ungranted();
    assert_eq!(c.read(true).unwrap().text(), CONTENT_1);
    assert_eq!(c.delegate.count("consume"), 1);
    assert_eq!(c.world.vendor.content_reads(), 1);
    let row = c.delegate.effect(c.proposal.operation_id).unwrap();
    assert!(row.decision.is_some(), "the effect names its decision");
    // A replayed consume carries no license: nothing is read again.
    assert_eq!(
        c.read(true).unwrap_err(),
        code_of("private_result_unavailable")
    );
    assert_eq!(c.world.vendor.content_reads(), 1);

    // Refused after approval: one code, whatever the reason, and the retry
    // says the same from the retained row.
    let c = ungranted();
    c.world.credentials.set_locked(true);
    c.world
        .clock
        .advance(std::time::Duration::from_secs(2 * 3600));
    assert_eq!(c.refusal(true), code_of("private_read_not_done"));
    assert_eq!(c.retry().unwrap_err(), code_of("private_read_not_done"));
    assert_eq!(c.world.vendor.content_reads(), 0);
}

#[test]
fn m5_private_perform_refusal_codes_are_specific_only_for_a_run_that_holds_the_grant() {
    use PrivateEffectRefusal as R;
    let all = [
        (R::NotDispatched, "private_read_not_done"),
        (R::AccessDenied, "connection_revoked"),
        (R::Revoked, "connection_revoked"),
        (R::WrongAccount, "connection_revoked"),
        (R::CredentialExpired, "connection_needs_sign_in"),
        (R::CredentialLocked, "credential_locked"),
        (R::CredentialMissing, "connection_needs_sign_in"),
        (R::ItemUnavailable, "item_unavailable"),
        (R::DestinationChanged, "item_changed"),
        (R::VendorRejected, "private_read_not_done"),
        (R::Unavailable, "private_read_not_done"),
    ];
    for (refusal, granted) in all {
        assert_eq!(refusal_code(refusal, true), granted, "{refusal:?}");
        // Allowed once: nothing distinguishes one refusal from another.
        assert_eq!(refusal_code(refusal, false), "private_read_not_done");
    }
    // Only a delivered read is content; every other outcome is a code.
    for (ended, told) in [
        (ReadOutcome::Withheld, "connection_revoked"),
        (ReadOutcome::Unknown, "private_outcome_unknown"),
        (ReadOutcome::CallerGone, "private_outcome_unknown"),
        (ReadOutcome::NotOurs, "private_outcome_unknown"),
        (ReadOutcome::ResultUnavailable, "private_result_unavailable"),
        (ReadOutcome::InProgress, "private_read_in_progress"),
    ] {
        assert_eq!(outcome(ended, true).unwrap_err(), code_of(told));
    }
}

#[test]
fn m5_no_secret_surface_a_read_leaves_no_secret_label_title_or_content_in_what_is_recorded() {
    let c = case();
    let content = c.read(false).unwrap();
    // The content is in the returned value and nowhere it can be printed.
    let printed = format!("{content:?}");
    assert!(!printed.contains("MARK-"), "{printed}");
    let refusal = format!("{:?}", c.read(false).unwrap_err());
    let effect = format!("{:?}", c.delegate.effect(c.proposal.operation_id).unwrap());
    let mut surfaces = vec![printed, refusal, effect, format!("{:?}", c.world.service)];
    surfaces.extend(c.world.journal.entries());
    surfaces.extend(c.delegate.log().iter().map(|entry| (*entry).to_owned()));
    let secrets = c.world.vendor.secrets();
    assert!(!secrets.is_empty(), "the scripted vendor issued secrets");
    for surface in &surfaces {
        for secret in &secrets {
            assert!(!surface.contains(secret.as_str()), "a secret was recorded");
        }
        for mark in [CONTENT_1, LABEL_A, TITLE_1] {
            assert!(!surface.contains(mark), "private text was recorded: {mark}");
        }
    }
}
