// SPDX-License-Identifier: MPL-2.0
//! The positive classification alone: pure values, no owner, no Store.
use super::*;
use koru_delegate_core::{BotId, BotPolicyBarrier, BotPolicyBarrierKey, BotRequestId, DeviceId};
use uuid::Uuid;

struct Case {
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    proof: FreshBotPolicyProof,
    workspace: WorkspaceId,
    description: NativeWriteDescription,
}

fn case() -> Case {
    let workspace = WorkspaceId::from_uuid(Uuid::new_v4());
    let owner = BotOwner {
        user_scope: serde_json::from_value(json!(Uuid::new_v4())).unwrap(),
        device: DeviceId::new(),
    };
    let bot = BotId::mint();
    let captured = CapturedBotPolicy {
        workspace,
        destination: workspace,
        access: BotAccess::Default,
        selected_pages: Default::default(),
        connections: Default::default(),
    };
    Case {
        provenance: BotProvenance {
            routine: None,
            owner,
            bot,
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        proof: FreshBotPolicyProof {
            routine: None,
            global: BotPolicyBarrier {
                key: BotPolicyBarrierKey {
                    owner,
                    bot,
                    request: Uuid::new_v4().try_into().unwrap(),
                },
                config_revision: 1,
                policy_revision: 1,
                policy: captured.clone(),
                effects_allowed: true,
            },
        },
        captured,
        workspace,
        description: NativeWriteDescription {
            kind: NativeWriteKind::Edited,
            workspace,
            document: Some(DocumentId::new()),
            fingerprint: "00".repeat(32),
            expected_revision: Some(vec![1]),
            title: "Page".into(),
        },
    }
}

fn classify(c: &Case) -> Option<PageWriteDenial> {
    selected_page_denial(
        &c.provenance,
        &c.captured,
        &c.proof,
        c.workspace,
        &c.description,
    )
}

#[test]
fn only_an_existing_page_the_captured_or_current_policy_withholds_is_a_denial() {
    let c = case();
    let target = c.description.document.unwrap();
    assert_eq!(classify(&c), Some(PageWriteDenial { target }));
    // Narrowed now although granted at capture: still a denial (the owner's
    // predicate: both must grant).
    let mut c = case();
    let target = c.description.document.unwrap();
    c.captured.selected_pages.insert(target);
    assert!(classify(&c).is_some());
    // Granted by both, or whole-workspace access on both: not a denial.
    c.proof.global.policy.selected_pages.insert(target);
    assert!(classify(&c).is_none());
    let mut c = case();
    c.captured.access = BotAccess::Workspace;
    c.proof.global.policy.access = BotAccess::Workspace;
    assert!(classify(&c).is_none());
}

#[test]
fn every_other_refusal_keeps_its_existing_path() {
    let mutations: [fn(&mut Case); 6] = [
        |c| c.description.kind = NativeWriteKind::Created,
        |c| c.description.document = None,
        |c| c.description.expected_revision = None,
        |c| c.description.workspace = WorkspaceId::from_uuid(Uuid::new_v4()),
        |c| c.proof.global.effects_allowed = false,
        |c| c.proof.global.policy.destination = WorkspaceId::from_uuid(Uuid::new_v4()),
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let mut c = case();
        mutate(&mut c);
        assert!(classify(&c).is_none(), "case {index}");
    }
}

/// The same Run and edit as [`case`], started by a Bot Routine whose own
/// grant (captured and current) does not select the page.
fn routine_case() -> Case {
    use koru_delegate_core::{
        RoutineDefinitionIdentity, RoutineDigest32, RoutinePolicyBarrier, RoutineProvenance,
        ScheduleId,
    };
    let mut c = case();
    let schedule = ScheduleId::from_uuid(Uuid::new_v4());
    c.provenance.routine = Some(RoutineProvenance {
        schedule,
        occurrence_ms: 900,
        definition: RoutineDefinitionIdentity {
            revision: 2,
            digest: RoutineDigest32::from_bytes([5; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    });
    c.proof.routine = Some(RoutinePolicyBarrier {
        owner: c.provenance.owner,
        bot: c.provenance.bot,
        schedule,
        request: Uuid::new_v4().try_into().unwrap(),
        grant_revision: 1,
        policy: c.captured.clone(),
        effects_allowed: true,
    });
    c
}

/// P4.3 section 4.10: a Routine Run is judged against the Routine's own
/// grant, captured and current, never the Bot's ordinary-chat access.
#[test]
fn routine_decision_is_offered_only_when_the_routines_own_grant_withholds_the_page() {
    let c = routine_case();
    let target = c.description.document.unwrap();
    assert_eq!(classify(&c), Some(PageWriteDenial { target }));

    // The Bot's ordinary access selecting the page changes nothing: the
    // Routine's grant still withholds it, so the question is still asked.
    let mut c = routine_case();
    let target = c.description.document.unwrap();
    c.proof.global.policy.selected_pages.insert(target);
    assert!(classify(&c).is_some());
    c.proof.global.policy.access = BotAccess::Workspace;
    assert!(classify(&c).is_some());

    // The Routine's grant allows the page, now or at capture: no question.
    let mut c = routine_case();
    let target = c.description.document.unwrap();
    c.proof
        .routine
        .as_mut()
        .unwrap()
        .policy
        .selected_pages
        .insert(target);
    assert!(classify(&c).is_none(), "the current grant selects the page");
    let mut c = routine_case();
    let target = c.description.document.unwrap();
    c.captured.selected_pages.insert(target);
    assert!(
        classify(&c).is_none(),
        "the captured grant selected the page"
    );
    let mut c = routine_case();
    c.proof.routine.as_mut().unwrap().policy.access = BotAccess::Workspace;
    assert!(classify(&c).is_none());
}

#[test]
fn routine_decision_needs_the_routine_in_both_the_subject_and_the_proof() {
    use koru_delegate_core::ScheduleId;
    // A Routine Run offered without its Routine proof, and an interactive
    // Run offered with one, keep their existing refusal.
    let mut c = routine_case();
    c.proof.routine = None;
    assert!(classify(&c).is_none());
    let mut c = routine_case();
    c.provenance.routine = None;
    assert!(classify(&c).is_none());
    // Another Routine's grant, a grant whose effects are fenced, or a grant
    // homed elsewhere never qualifies either.
    let mutations: [fn(&mut Case); 4] = [
        |c| c.proof.routine.as_mut().unwrap().schedule = ScheduleId::from_uuid(Uuid::new_v4()),
        |c| c.proof.routine.as_mut().unwrap().effects_allowed = false,
        |c| c.proof.routine.as_mut().unwrap().bot = BotId::mint(),
        |c| {
            c.proof.routine.as_mut().unwrap().policy.destination =
                WorkspaceId::from_uuid(Uuid::new_v4())
        },
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let mut c = routine_case();
        mutate(&mut c);
        assert!(classify(&c).is_none(), "case {index}");
    }
    // Every other refusal keeps its path for a Routine Run too.
    let mut c = routine_case();
    c.proof.global.effects_allowed = false;
    assert!(classify(&c).is_none());
    let mut c = routine_case();
    c.description.kind = NativeWriteKind::Created;
    assert!(classify(&c).is_none());
}
