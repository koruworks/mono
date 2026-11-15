// SPDX-License-Identifier: MPL-2.0
//! A decision card's answer over scripted authority, wait read and delegate
//! ports. Scripted composition: no real GatewayState, Store, provider, actor
//! or session host is involved, and nothing here qualifies a platform.
use super::*;
use crate::worker::bots::decision_wait::fixture;
use koru_delegate_core as d;
use koru_delegate_core::decisions::{
    BotDecisionWaitObservation, DecisionNativeChoice, DecisionRevision,
};
use std::sync::Mutex;

struct Human {
    owner: BotOwner,
    home: WorkspaceId,
    profile: koru_ai_core::ProfileId,
}
impl BotAuthority for Human {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, w: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        (w == self.home)
            .then_some(self.profile)
            .ok_or(BotError::Forbidden)
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.workspace(p.workspace).map(|_| ())
    }
}
struct Wait(
    Result<
        Option<BotDecisionWaitObservation>,
        crate::worker::bots::decision_wait::DecisionWaitPortError,
    >,
);
impl DecisionWaitDelegate for Wait {
    fn read_decision_wait(
        &self,
        _: &DecisionWaitSubject,
        _: std::time::Duration,
    ) -> Result<
        Option<BotDecisionWaitObservation>,
        crate::worker::bots::decision_wait::DecisionWaitPortError,
    > {
        self.0.clone()
    }
    fn read_decision_resume(
        &self,
        _: &DecisionWaitSubject,
        _: std::time::Duration,
    ) -> Result<
        Option<koru_delegate_core::decisions::DecisionResume>,
        crate::worker::bots::decision_wait::DecisionWaitPortError,
    > {
        Ok(None)
    }
}
#[derive(Default)]
struct Port {
    sent: Mutex<Vec<DelegateCommand>>,
    reply: Mutex<Option<Result<BotReply, koru_delegate_api::Failure>>>,
}
impl BotDelegatePort for Port {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        self.sent.lock().unwrap().push(command);
        self.reply.lock().unwrap().take().unwrap_or_else(|| {
            Err(koru_delegate_api::Failure::new(
                "bot_unavailable",
                "x",
                true,
            ))
        })
    }
}

struct Case {
    human: DecisionHuman,
    command: DelegateCommand,
    held: BotDecisionWaitObservation,
}
fn case(profile_matches: bool, replay: bool) -> Case {
    let fence = fixture::fence();
    let held = fixture::origin_stopped(&fixture::holding(&fence));
    let key = held.key.clone();
    let owner = BotOwner {
        user_scope: fence.owner.user_scope,
        device: fence.owner.device,
    };
    let home = WorkspaceId::new();
    let human = koru_ai_core::ProfileId::new();
    let captured = koru_delegate_core::CapturedBotPolicy {
        workspace: d::WorkspaceId::from_uuid(home.as_uuid()),
        access: d::BotAccess::Default,
        selected_pages: Default::default(),
        destination: d::WorkspaceId::from_uuid(home.as_uuid()),
    };
    let global =
        crate::worker::bots::policy::global_barrier(owner, fence.bot, 3, 4, captured).unwrap();
    let choice = DecisionAnswerChoice::AllowOnce {};
    let answer = DecisionAnswerRequest {
        owner: fence.owner,
        run: fence.run,
        decision: key.decision_id,
        expected_revision: DecisionRevision::new(2).unwrap(),
        binding_digest: DecisionDigest::from_bytes([5; 32]),
        request: answer_request_id(key.decision_id, &choice).unwrap(),
        human,
        choice,
        authority_digest: DecisionDigest::from_bytes([0; 32]),
    };
    Case {
        human: DecisionHuman {
            authority: Arc::new(Human {
                owner,
                home,
                profile: if profile_matches {
                    human
                } else {
                    koru_ai_core::ProfileId::new()
                },
            }),
            owner,
            home,
            human,
            reread: (!replay).then(|| {
                (
                    Arc::new(Wait(Ok(Some(held.clone())))) as Arc<dyn DecisionWaitDelegate>,
                    DecisionWaitSubject::of(&key),
                )
            }),
            card: BotDecisionCard {
                decision: key.decision_id,
                run: fence.run,
                ai_attempt: InferenceAttemptId::new(),
                invocation: ToolInvocationId::new(),
                revision: key.decision_revision,
            },
            replay,
        },
        command: DelegateCommand::AnswerDecision {
            answer,
            proof: FreshBotPolicyProof {
                global,
                routine: None,
            },
            boot: d::DelegateBootId::mint(),
        },
        held,
    }
}
fn sent_answer(port: &Port) -> DecisionAnswerRequest {
    match port.sent.lock().unwrap().last().cloned() {
        Some(DelegateCommand::AnswerDecision { answer, .. }) => answer,
        _ => panic!("no decision answer sent"),
    }
}

#[test]
fn decision_answer_reaches_the_delegate_with_human_proof_reread_revision_and_stable_key() {
    let c = case(true, false);
    let port = Port::default();
    let current = match &c.held.disposition {
        DecisionDisposition::Open { state } => state.revision,
        _ => unreachable!(),
    };
    let (result, sent) = answer(&c.human, c.command.clone(), &port);
    assert!(result.is_err(), "the scripted delegate answers unknown");
    let first = sent_answer(&port);
    // The positive reread's current revision replaces the receipt's one.
    assert_eq!(first.expected_revision, current);
    assert_ne!(first.authority_digest, DecisionDigest::from_bytes([0; 32]));
    assert_eq!(first.choice, DecisionAnswerChoice::AllowOnce {});
    let sent = sent.expect("sent");
    assert!(sent.koru);
    assert_eq!(sent.answer, first);
    // The same human answer again: the same request id and proof digest.
    let (_, _) = answer(&c.human, c.command, &port);
    let second = sent_answer(&port);
    assert_eq!(second.request, first.request);
    assert_eq!(second.authority_digest, first.authority_digest);
}

#[test]
fn decision_stale_human_or_unanswerable_wait_is_never_sent() {
    // The current person is not the decision's human: refused locally.
    let c = case(false, false);
    let port = Port::default();
    let (result, sent) = answer(&c.human, c.command, &port);
    let error = result.unwrap_err();
    assert_eq!(error.code, "decision_human_changed");
    assert!(sent.is_none());
    assert!(port.sent.lock().unwrap().is_empty());
    let shown = person_failure(&error.code, error.retryable);
    assert_eq!(shown.code, tool_not_resumable().code);
    // A wait that is still holding (not Held) is not answerable yet.
    let mut c = case(true, false);
    let holding = fixture::holding(&fixture::fence());
    let subject = c.human.reread.as_ref().expect("koru reread").1;
    c.human.reread = Some((
        Arc::new(Wait(Ok(Some(holding)))) as Arc<dyn DecisionWaitDelegate>,
        subject,
    ));
    let (result, _) = answer(&c.human, c.command, &port);
    assert_eq!(result.unwrap_err().code, "bot_stale");
    assert!(port.sent.lock().unwrap().is_empty());
    // An unavailable reread is unknown, not a refusal.
    let mut c = case(true, false);
    let subject = c.human.reread.as_ref().expect("koru reread").1;
    c.human.reread = Some((
        Arc::new(Wait(Err(
            crate::worker::bots::decision_wait::DecisionWaitPortError::Unavailable,
        ))) as Arc<dyn DecisionWaitDelegate>,
        subject,
    ));
    let (result, _) = answer(&c.human, c.command, &port);
    assert!(result.unwrap_err().retryable);
}

#[test]
fn decision_exact_replay_resends_the_first_form_without_fresh_checks() {
    // A replay must not depend on today's human proof or reread.
    let c = case(false, true);
    let port = Port::default();
    let original = match &c.command {
        DelegateCommand::AnswerDecision { answer, .. } => answer.clone(),
        _ => unreachable!(),
    };
    let (_, sent) = answer(&c.human, c.command, &port);
    assert!(sent.is_some());
    assert_eq!(sent_answer(&port), original);
}

#[test]
fn decision_non_grant_refusals_appear_in_existing_wording() {
    let not_applied = failure("tool_not_resumable", ANSWER_NOT_APPLIED, false);
    for code in [
        "bot_decision_owned", // O136: the legacy native answer while a decision owns the Run
        "bot_stale",
        "bot_conflict",
        "bot_invalid",
        "bot_approval_stale",
        "bot_capacity",
    ] {
        let shown = person_failure(code, false);
        assert_eq!(
            (shown.code.as_str(), shown.message.as_str(), shown.retryable),
            (
                not_applied.code.as_str(),
                not_applied.message.as_str(),
                false
            ),
            "{code}"
        );
    }
    for code in [
        "bot_fenced",
        "bot_owner_mismatch",
        "bot_boot_stale",
        "decision_human_changed",
    ] {
        let shown = person_failure(code, false);
        let inactive = tool_not_resumable();
        assert_eq!(
            (shown.code, shown.message),
            (inactive.code, inactive.message),
            "{code}"
        );
    }
    // Unknown completion: the latest state follows, and a retry is offered.
    let unknown = person_failure("bot_unavailable", true);
    assert_eq!(unknown.message, ANSWER_NOT_APPLIED);
    assert!(unknown.retryable);
}

#[test]
fn decision_request_ids_are_stable_per_answer_and_distinct_across_choices() {
    let decision = DecisionId::mint();
    let allow = answer_request_id(decision, &DecisionAnswerChoice::AllowOnce {}).unwrap();
    let deny = answer_request_id(decision, &DecisionAnswerChoice::Deny {}).unwrap();
    assert_eq!(
        allow,
        answer_request_id(decision, &DecisionAnswerChoice::AllowOnce {}).unwrap()
    );
    assert_ne!(allow, deny);
    assert_ne!(
        allow,
        answer_request_id(DecisionId::mint(), &DecisionAnswerChoice::AllowOnce {}).unwrap()
    );
    assert_eq!(allow.as_uuid().get_version(), Some(uuid::Version::Random));
    // O138: an ordinary native answer's key is stable, and a reject then an
    // allow on the same prompt never share it.
    let prompt = koru_session_protocol::ApprovalId::mint();
    let reject = native_answer_key(
        prompt,
        koru_session_protocol::ApprovalDecision::Reject,
        None,
    )
    .unwrap();
    let approve = native_answer_key(
        prompt,
        koru_session_protocol::ApprovalDecision::Approve,
        None,
    )
    .unwrap();
    assert_eq!(
        reject,
        native_answer_key(
            prompt,
            koru_session_protocol::ApprovalDecision::Reject,
            None
        )
        .unwrap()
    );
    assert_ne!(reject, approve);
}

#[test]
fn native_card_answers_only_an_offered_choice_and_continues_on_allow_or_reject() {
    let allow = DecisionNativeChoice {
        id: koru_session_protocol::ApprovalChoiceId::mint(),
        intent: ApprovalIntent::Allow,
    };
    let always = DecisionNativeChoice {
        id: koru_session_protocol::ApprovalChoiceId::mint(),
        intent: ApprovalIntent::AllowRemembered,
    };
    let reject = DecisionNativeChoice {
        id: koru_session_protocol::ApprovalChoiceId::mint(),
        intent: ApprovalIntent::Reject,
    };
    let offered = [allow.clone(), always.clone(), reject.clone()];
    // Allow and Reject keep the native prompt's own offered identities.
    assert_eq!(native_choice(&offered, None, true).unwrap(), allow);
    assert_eq!(native_choice(&offered, None, false).unwrap(), reject);
    assert_eq!(
        native_choice(&offered, Some(&always.id.to_string()), true).unwrap(),
        always
    );
    // A choice that does not match the button, or one never offered, refuses.
    assert!(native_choice(&offered, Some(&reject.id.to_string()), true).is_err());
    assert!(native_choice(&offered, Some(&DecisionId::mint().to_string()), true).is_err());
    // Two plain allows are ambiguous without an explicit choice.
    assert!(native_choice(&[allow.clone(), allow.clone()], None, true).is_err());
}

#[test]
fn koru_card_settles_once_allow_answered_and_deny_denied() {
    let at = Timestamp::from_millis(2_000);
    let row = crate::worker::agents::agent_row(
        ToolInvocationId::new(),
        InferenceAttemptId::new(),
        ChatId::new(),
        MessageId::new(),
        AgentToolKind::Approval,
        ToolApproval::Pending,
        Timestamp::from_millis(1_000),
    );
    let mut open = row.clone();
    open.status = ToolActivityStatus::ApprovalRequired;
    let allowed = settled_koru_card(open.clone(), DecisionAuthorization::Granted, at).unwrap();
    assert_eq!(
        (allowed.status, allowed.approval),
        (ToolActivityStatus::Complete, ToolApproval::Approved)
    );
    assert_eq!(allowed.completed_at, Some(at));
    let denied = settled_koru_card(open.clone(), DecisionAuthorization::Denied, at).unwrap();
    assert_eq!(
        (denied.status, denied.approval, denied.error_code),
        (
            ToolActivityStatus::Denied,
            ToolApproval::Denied,
            Some(ToolErrorCode::PermissionDenied)
        )
    );
    // One use: an answered card is never answered again, and a pending
    // authorization is not an answer.
    assert!(settled_koru_card(allowed, DecisionAuthorization::Denied, at).is_none());
    assert!(settled_koru_card(open, DecisionAuthorization::Pending, at).is_none());
}

#[test]
fn decision_card_without_its_receipt_is_no_longer_active_and_sends_nothing() {
    // After an AI restart the in-memory receipt is gone: the card refuses in
    // the existing "no longer active" words before any job or delegate call.
    let waits = crate::worker::bots::decision_wait::DecisionWaitDriver::new(None);
    let card = BotDecisionCard {
        decision: DecisionId::mint(),
        run: d::RunId::mint(),
        ai_attempt: InferenceAttemptId::new(),
        invocation: ToolInvocationId::new(),
        revision: DecisionRevision::new(2).unwrap(),
    };
    let refused = card_receipt(&waits, &card).unwrap_err();
    let inactive = tool_not_resumable();
    assert_eq!(
        (refused.code, refused.message, refused.retryable),
        (inactive.code, inactive.message, false)
    );
}
