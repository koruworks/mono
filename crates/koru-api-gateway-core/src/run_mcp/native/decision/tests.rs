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
