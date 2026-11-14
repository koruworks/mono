// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots::UserScopeId;
fn id(n: u8) -> uuid::Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}
fn fixture() -> (BotRoutineCreate, BotRoutineCreationSource) {
    let home = WorkspaceId::from_uuid(id(3));
    let command = BotRoutineCreate {
        request: BotRequestId::from_uuid(id(6)),
        bot: BotId::from_uuid(id(4)),
        expected_bot_revision: 2,
        name: ScheduleName::new("Private name").unwrap(),
        objective: Objective::new("Private objective").unwrap(),
        documents: vec![DocumentId::from_uuid(id(9))],
        recurrence: serde_json::from_str(r#"{"type":"floating","cron":"0 9 * * *"}"#).unwrap(),
    };
    let source = BotRoutineCreationSource {
        owner: BotOwner {
            user_scope: UserScopeId::from_uuid(id(1)),
            device: DeviceId::from_uuid(id(2)),
        },
        bot: command.bot,
        home,
        profile: ProfileId::from_uuid(id(8)),
        revision: 2,
        policy_revision: 3,
        model: crate::ModelSelection {
            provider: crate::AiProviderId::parse("agent-codex").unwrap(),
            requested_model: "fixture-model".into(),
            default_reasoning: crate::ReasoningChoice::Default,
        },
        // The neutral folder type accepts this absolute spelling on every OS.
        folder: "/fixture".into(),
        policy: CapturedBotPolicy {
            workspace: home,
            destination: home,
            access: BotAccess::Default,
            selected_pages: [DocumentId::from_uuid(id(10)), DocumentId::from_uuid(id(9))].into(),
        },
    };
    (command, source)
}
fn frozen() -> BotRoutineCreationIdentity {
    let (c, s) = fixture();
    c.freeze(
        &s,
        ScheduleId::from_uuid(id(5)),
        id(7),
        RoutineDigest32::from_bytes([11; 32]),
        0,
    )
    .unwrap()
}

#[test]
fn creation_seed_and_grant_have_digest_free_golden_bytes() {
    let frozen = frozen();
    let grant = r#"{"version":1,"source_bot_policy_revision":3,"grant_revision":1,"policy":{"workspace":"00000000-0000-4000-8000-000000000003","access":"default","selected_pages":["00000000-0000-4000-8000-000000000009","00000000-0000-4000-8000-00000000000a"],"destination":"00000000-0000-4000-8000-000000000003"}}"#;
    assert_eq!(
        String::from_utf8(frozen.grant.canonical_bytes().unwrap()).unwrap(),
        grant
    );
    let seed = r#"{"version":1,"name":"Private name","header":{"schedule":"00000000-0000-4000-8000-000000000005","created_at_ms":0,"initial_owner":"00000000-0000-4000-8000-000000000002","definition":{"revision":1,"effective_at_ms":0,"template":{"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000008"},"objective":"Private objective","agent":{"kind":"built_in","value":"codex"},"requested_model":"fixture-model","scope":{"kind":"bot_folder","folder":"FOLDER"},"grants":["00000000-0000-4000-8000-000000000003"],"context":[{"kind":"document","workspace":"00000000-0000-4000-8000-000000000003","document":"00000000-0000-4000-8000-000000000009"}]},"recurrence":{"type":"floating","cron":"0 9 * * *"},"lifecycle":{"paused":{"reason":{"reason":"manual"}}},"routine":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000004","home":"00000000-0000-4000-8000-000000000003","creation_request":"00000000-0000-4000-8000-000000000006"}}}}"#;
    let actual = String::from_utf8(frozen.seed.canonical_bytes().unwrap()).unwrap();
    assert_eq!(actual, seed.replace("FOLDER", &fixture().1.folder));
    println!(
        "grant golden hash {}",
        String::from(frozen.grant.canonical_digest().unwrap())
    );
    println!(
        "seed golden hash {}",
        String::from(frozen.seed.canonical_digest().unwrap())
    );
    frozen.validate_digests().unwrap();
    let mut changed = frozen.clone();
    changed.grant.digest = RoutineDigest32::from_bytes([0; 32]);
    assert_eq!(
        changed.grant.canonical_bytes().unwrap(),
        frozen.grant.canonical_bytes().unwrap()
    );
    assert!(changed.validate_digests().is_err());
    changed = frozen.clone();
    changed.seed.header.created_at_ms = 1;
    assert!(changed.validate_digests().is_err());
}

#[test]
fn creation_semantic_validation_and_canonical_source_matching_are_separate() {
    let (mut command, mut source) = fixture();
    let frozen = frozen();
    assert!(command.matches_retained(&frozen));
    assert!(frozen.matches_source(&source).unwrap());
    command.documents.push(command.documents[0]);
    assert_eq!(command.validate(), Err(BotError::Invalid));
    command = fixture().0;
    command.documents = (1..=17).map(|i| DocumentId::from_uuid(id(i))).collect();
    assert!(command.validate().is_err());
    source.model.requested_model = "other".into();
    assert!(!frozen.matches_source(&source).unwrap());
    source = fixture().1;
    source.profile = ProfileId::from_uuid(id(12));
    assert!(!frozen.matches_source(&source).unwrap());
    for encoded in [
        r#"{"request":"x","model":"override"}"#,
        r#"{"recurrence":{"type":"floating","cron":"bad"}}"#,
    ] {
        assert!(serde_json::from_str::<BotRoutineCreate>(encoded).is_err());
    }
    let mut encoded = serde_json::to_value(fixture().0).unwrap();
    encoded["model"] = serde_json::json!("override");
    assert!(serde_json::from_value::<BotRoutineCreate>(encoded).is_err());
    assert!(!format!("{:?}", fixture().0).contains("Private"));
    assert!(!format!("{:?}", source).contains("fixture"));
}
