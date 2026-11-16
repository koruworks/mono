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
            connections: Default::default(),
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
    let grant = r#"{"version":1,"source_bot_policy_revision":3,"grant_revision":1,"policy":{"workspace":"00000000-0000-4000-8000-000000000003","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-000000000003"}}"#;
    assert_eq!(
        String::from_utf8(frozen.grant.canonical_bytes().unwrap()).unwrap(),
        grant
    );
    let seed = r#"{"version":1,"name":"Private name","header":{"schedule":"00000000-0000-4000-8000-000000000005","created_at_ms":0,"initial_owner":"00000000-0000-4000-8000-000000000002","definition":{"revision":1,"effective_at_ms":0,"template":{"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000008"},"objective":"Private objective","agent":{"kind":"built_in","value":"codex"},"requested_model":"fixture-model","scope":{"kind":"bot_folder","folder":"FOLDER"},"grants":["00000000-0000-4000-8000-000000000003"],"context":[{"kind":"document","workspace":"00000000-0000-4000-8000-000000000003","document":"00000000-0000-4000-8000-000000000009"}]},"recurrence":{"type":"floating","cron":"0 9 * * *"},"lifecycle":{"paused":{"reason":{"reason":"manual"}}},"routine":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000004","home":"00000000-0000-4000-8000-000000000003","creation_request":"00000000-0000-4000-8000-000000000006"}}}}"#;
    let actual = String::from_utf8(frozen.seed.canonical_bytes().unwrap()).unwrap();
    assert_eq!(actual, seed.replace("FOLDER", &fixture().1.folder));
    assert_eq!(
        String::from(frozen.grant.canonical_digest().unwrap()),
        "e77bdbb88a8a93ba34e9ae8eb61a9ef26f168a8dcee20f80c1a74447ad6a0c6e"
    );
    assert_eq!(
        String::from(frozen.seed.canonical_digest().unwrap()),
        "d5719c02651cf67ab1851d6c6bea1d7a77f1318a35ec8a3c57fd908dff1863aa"
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

/// D4a: a new Routine starts from the approved default access (default, no
/// selected pages, the Bot's home as workspace and destination), whatever the
/// Bot's own access is. Only the Bot policy revision observed is recorded.
#[test]
fn creation_grant_is_the_default_access_and_never_a_copy_of_the_bots() {
    let (command, mut source) = fixture();
    let home = source.home;
    // The Bot itself may edit any page of the workspace, and has pages.
    source.policy.access = BotAccess::Workspace;
    assert_eq!(source.policy.selected_pages.len(), 2);
    let frozen = command
        .freeze(
            &source,
            ScheduleId::from_uuid(id(5)),
            id(7),
            RoutineDigest32::from_bytes([11; 32]),
            0,
        )
        .unwrap();
    assert_eq!(frozen.grant.policy.access, BotAccess::Default);
    assert!(frozen.grant.policy.selected_pages.is_empty());
    assert_eq!(frozen.grant.policy.workspace, home);
    assert_eq!(frozen.grant.policy.destination, home);
    assert_eq!(frozen.grant.grant_revision, 1);
    assert_eq!(frozen.grant.source_bot_policy_revision, 3);
    frozen.validate_digests().unwrap();
    assert!(frozen.matches_source(&source).unwrap());
    // A change of the Bot's access after creation changes the policy
    // revision, so an unfinished creation is still recognized as changed.
    source.policy_revision = 4;
    assert!(!frozen.matches_source(&source).unwrap());
    // A creation prepared before the change froze a copy of the Bot's
    // access. It keeps that grant and is still recognized while it finishes.
    source.policy_revision = 3;
    let mut copied = frozen.clone();
    copied.grant.policy = source.policy.clone();
    copied.grant.digest = copied.grant.canonical_digest().unwrap();
    assert!(copied.matches_source(&source).unwrap());
    // Any other grant is not this source's.
    copied.grant.policy.selected_pages.pop_first();
    copied.grant.digest = copied.grant.canonical_digest().unwrap();
    assert!(!copied.matches_source(&source).unwrap());
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

#[test]
fn m5_ai_a_new_routine_captures_no_connection_whatever_its_bot_holds() {
    let (command, mut source) = fixture();
    let without = source.grant().unwrap();
    let copied_without = source.copied_grant().unwrap();
    // The Bot now holds a connection grant.
    source.policy.connections = [koru_delegate_core::ConnectionGrantRef {
        connection: koru_delegate_core::ConnectionId::from_uuid(id(30)),
        operation: koru_delegate_core::PrivateOperationKind::ReadItem,
    }]
    .into();
    source.validate_for(&command).unwrap();
    // The grant a new Routine starts from is the same bytes and digest.
    let with = source.grant().unwrap();
    assert_eq!(with, without);
    assert!(with.policy.connections.is_empty());
    assert_eq!(
        with.canonical_bytes().unwrap(),
        without.canonical_bytes().unwrap()
    );
    // Not even the copy a pre-D4a creation is recognized by carries one.
    let copied = source.copied_grant().unwrap();
    assert!(copied.policy.connections.is_empty());
    assert_eq!(copied, copied_without);
    let frozen = command
        .freeze(
            &source,
            ScheduleId::from_uuid(id(5)),
            id(7),
            RoutineDigest32::from_bytes([11; 32]),
            0,
        )
        .unwrap();
    assert!(frozen.grant.policy.connections.is_empty());
}
