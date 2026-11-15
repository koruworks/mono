// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::*;
use std::collections::BTreeSet;
use uuid::Uuid;

mod binding;
mod codec;
mod golden;
mod lifecycle;
mod projection;
mod publication;

fn uuid(n: u32) -> Uuid {
    format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap()
}
fn id<T: std::str::FromStr>(n: u32) -> T {
    uuid(n).to_string().parse().ok().unwrap()
}
fn rev(n: u64) -> DecisionRevision {
    DecisionRevision::new(n).unwrap()
}
fn digest(n: u8) -> DecisionDigest {
    DecisionDigest::from_bytes([n; 32])
}
fn edit() -> DecisionEditBinding {
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::from_uuid(uuid(1)),
        device: DeviceId::from_uuid(uuid(2)),
    };
    let workspace = WorkspaceId::from_uuid(uuid(10));
    DecisionEditBinding {
        version: DecisionVersionV1,
        owner,
        bot: id(3),
        run: id(4),
        origin_attempt: id(5),
        origin_generation: AttemptGeneration::FIRST,
        origin_session: id(6),
        origin_turn: id(7),
        submission: id(8),
        routine: None,
        human_profile: ProfileId::from_uuid(uuid(9)),
        workspace,
        target: DocumentId::from_uuid(uuid(11)),
        action: DecisionEditAction::EditDocument,
        operation_id: uuid(12),
        edit_fingerprint: [0; 32],
        expected_revision: DecisionBytes::new(vec![1, 2]).unwrap(),
        captured_policy: CapturedBotPolicy {
            workspace,
            access: BotAccess::Default,
            selected_pages: BTreeSet::new(),
            destination: workspace,
        },
        global_policy: DecisionGlobalPolicy {
            key: BotPolicyBarrierKey {
                owner,
                bot: id(3),
                request: id::<BotRequestId>(13).into(),
            },
            config_revision: rev(1),
            policy_revision: rev(2),
            digest: digest(0),
        },
        routine_policy: None,
        payload_version: DecisionVersionV1,
        payload: DecisionBytes::new(b"{}".to_vec()).unwrap(),
    }
}
fn binding() -> DecisionBindingV1 {
    DecisionBindingV1::KoruEdit(edit())
}
fn native() -> DecisionNativeBinding {
    let e = edit();
    DecisionNativeBinding {
        version: e.version,
        owner: e.owner,
        bot: e.bot,
        run: e.run,
        origin_attempt: e.origin_attempt,
        origin_generation: e.origin_generation,
        origin_session: e.origin_session,
        origin_turn: e.origin_turn,
        submission: e.submission,
        routine: e.routine,
        human_profile: e.human_profile,
        workspace: e.workspace,
        captured_policy: e.captured_policy,
        global_policy: e.global_policy,
        routine_policy: e.routine_policy,
        prompt: id(20),
        choices: DecisionList::new(vec![
            DecisionNativeChoice {
                id: id(21),
                intent: koru_session_protocol::ApprovalIntent::Allow,
            },
            DecisionNativeChoice {
                id: id(22),
                intent: koru_session_protocol::ApprovalIntent::Reject,
            },
            DecisionNativeChoice {
                id: id(23),
                intent: koru_session_protocol::ApprovalIntent::AllowRemembered,
            },
            DecisionNativeChoice {
                id: id(24),
                intent: koru_session_protocol::ApprovalIntent::RejectRemembered,
            },
        ])
        .unwrap(),
        native_deadline_at_ms: Some(90),
    }
}
fn intent() -> BotWaitCleanupIntentV1 {
    let f = edit().origin();
    BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: f.owner,
        bot: f.bot,
        run: f.run,
        wait_id: id(14),
        decision_id: id(15),
        decision_revision: rev(2),
        wait_revision: rev(3),
        origin_attempt: f.attempt,
        origin_generation: f.generation,
        origin_session: f.session,
        origin_turn: f.turn,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    }
}
fn resume(successor: bool) -> DecisionResume {
    DecisionResume {
        decision: id(15),
        origin: edit().origin(),
        consumer: if successor {
            DecisionConsumer::RecordedSuccessor {
                attempt: id(30),
                generation: AttemptGeneration::from_raw(2).unwrap(),
                creation_key: id(31),
                session: Some(id(32)),
            }
        } else {
            DecisionConsumer::SameSession {
                creation_key: id(31),
            }
        },
        input_key: id(33),
        input_fingerprint: [4; 32],
        state: DecisionResumeState::InputAcknowledged {
            acknowledgment: DecisionInputAcknowledgment {
                turn: id(34),
                receipt_digest: digest(5),
            },
        },
    }
}
fn receipt() -> DecisionReceipt {
    let b = binding();
    DecisionReceipt {
        decision: id(15),
        binding_digest: b.digest().unwrap(),
        binding: b,
        deadline: DecisionDeadline {
            created_boot: id(35),
            created_at_ms: -10,
            deadline_at_ms: 599_990,
            native_deadline_at_ms: None,
        },
        state: DecisionState {
            kind: DecisionKind::KoruEdit,
            authorization: DecisionAuthorization::Pending,
            phase: DecisionPhase::Publishing,
            revision: rev(1),
        },
        answer: None,
        consumed_at_ms: None,
        closure: None,
        last_observed_at_ms: -10,
        updated_at_ms: -10,
    }
}
fn json<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap()
}
fn routine_subject() -> DecisionRoutineSubject {
    let e = edit();
    let definition = RoutineDefinitionIdentity {
        revision: 1,
        digest: digest(1),
        owner_handoff: None,
    };
    DecisionRoutineSubject {
        provenance: RoutineProvenance {
            schedule: id(81),
            occurrence_ms: -1,
            definition,
            grant_revision: 1,
        },
        claim: RoutineClaimIdentity {
            slot: RoutineSlot {
                owner: e.owner,
                bot: e.bot,
                schedule: id(81),
                occurrence_ms: -1,
            },
            definition,
            ids: RoutineExecutionIds {
                submission: e.submission,
                run: e.run,
                chat: uuid(101),
                attempt: uuid(102),
                user_message: uuid(103),
                assistant_message: uuid(104),
            },
            preparation_hash: digest(2),
            claimed_at_ms: -20,
        },
        route: RoutineRouteIdentity {
            owner: e.owner,
            bot: e.bot,
            schedule: id(81),
            workspace: e.workspace,
            creation_request: id(82),
            seed_hash: digest(3),
            created_at_ms: -100,
        },
    }
}
fn routine_binding() -> DecisionBindingV1 {
    let mut e = edit();
    e.routine = Some(routine_subject());
    e.routine_policy = Some(DecisionRoutinePolicy {
        owner: e.owner,
        bot: e.bot,
        schedule: id(81),
        request: id::<BotRequestId>(83).into(),
        grant_revision: rev(1),
        digest: digest(4),
    });
    DecisionBindingV1::KoruEdit(e)
}
fn decode<T: serde::de::DeserializeOwned + DecisionValidate>(
    v: &serde_json::Value,
) -> Result<T, DecisionError> {
    decode_decision_frame(&serde_json::to_vec(v).unwrap())
}
