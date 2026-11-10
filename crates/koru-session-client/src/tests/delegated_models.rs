// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_session_protocol::{CapabilitySet, DelegationLaunch, SessionOrigin, capability};

fn launch(model: bool) -> DelegationLaunch {
    DelegationLaunch {
        model: model.then(|| "fixture-model".parse().unwrap()),
        mcp_endpoint: "http://127.0.0.1:1/mcp".parse().unwrap(),
        credential: Some("fixture-bearer".parse().unwrap()),
        tools: koru_session_protocol::McpToolAllowlist::new(["read".parse().unwrap()]).unwrap(),
        resume: Some("fixture-resume".parse().unwrap()),
    }
}
fn client(version: u32, models: bool, selection: bool) -> (SessionClient, Arc<Mutex<Vec<u8>>>) {
    let (mut client, bytes) = approval_client(version, false, false);
    let mut capabilities = vec![capability::DELEGATION.parse().unwrap()];
    if models {
        capabilities.push(capability::PROVIDER_MODELS.parse().unwrap());
    }
    if selection {
        capabilities.push(capability::LAUNCH_SELECTION.parse().unwrap());
    }
    client.host.agreement.capabilities = CapabilitySet::new(capabilities);
    (client, bytes)
}
fn create(
    client: &SessionClient,
    key: IdempotencyKey,
    launch: DelegationLaunch,
) -> Result<SessionSummary, ClientError> {
    client.create_delegated(
        key,
        SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        "codex".parse().unwrap(),
        None,
        launch,
        PathBuf::from("C:/fixture"),
        GridSize::new(80, 24).unwrap(),
    )
}

#[test]
fn selected_model_refuses_before_writing_without_both_negotiated_families() {
    for (version, models, selection) in [
        (14, true, true),
        (15, true, true),
        (16, false, true),
        (16, true, false),
    ] {
        let (client, bytes) = client(version, models, selection);
        assert!(
            matches!(create(&client, IdempotencyKey::mint(), launch(true)), Err(ClientError::Session(ref failure)) if failure.code == ErrorCode::CapabilityNotNegotiated)
        );
        assert!(bytes.lock().unwrap().is_empty());
    }
}

#[test]
fn selected_model_replays_same_request_with_security_and_origin_preserved() {
    let (client, bytes) = client(16, true, true);
    let key = IdempotencyKey::mint();
    let original = launch(true);
    for _ in 0..2 {
        // The memory host's Ok response is intentionally not a Created reply.
        assert!(matches!(
            create(&client, key, original.clone()),
            Err(ClientError::UnexpectedResponse)
        ));
    }
    let bytes = bytes.lock().unwrap();
    let mut remaining = bytes.as_slice();
    let (_, first): (_, RequestFrame) = koru_ipc::frame::read_frame(&mut remaining).unwrap();
    let (_, second): (_, RequestFrame) = koru_ipc::frame::read_frame(&mut remaining).unwrap();
    assert_eq!(first.request, second.request);
    let Request::CreateProfileSession { create } = first.request else {
        panic!("delegated create");
    };
    assert_eq!(create.idempotency_key, key);
    assert_eq!(create.origin, SessionOrigin::Manual);
    assert_eq!(create.selection.unwrap().model, original.model);
    let mut expected = original;
    expected.model = None;
    assert_eq!(create.delegation, Some(expected));
    assert!(remaining.is_empty());
}

#[test]
fn provider_default_keeps_legacy_delegated_create_without_selection_capability() {
    let (client, bytes) = client(14, false, false);
    let original = launch(false);
    assert!(matches!(
        create(&client, IdempotencyKey::mint(), original.clone()),
        Err(ClientError::UnexpectedResponse)
    ));
    let bytes = bytes.lock().unwrap();
    let (_, frame): (_, RequestFrame) = koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    let Request::CreateProfileSession { create } = frame.request else {
        panic!("delegated create");
    };
    assert_eq!(create.origin, SessionOrigin::Manual);
    assert!(create.selection.is_none());
    assert_eq!(create.delegation, Some(original));
}
