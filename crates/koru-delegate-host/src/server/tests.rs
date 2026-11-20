// SPDX-License-Identifier: MPL-2.0

//! The stale-worker guard rejects frames before any actor command exists.
use super::*;
use koru_delegate_api::ipc::Caller;

#[test]
fn stale_wire_versions_cannot_reach_dispatch() {
    let caller = Caller {
        product: "test".into(),
        trace_id: "test".into(),
    };
    let run = koru_delegate_core::RunId::mint();
    let request: koru_delegate_api::CreateRunRequest = serde_json::from_value(serde_json::json!({
        "run": run.to_storage(),
        "workspace": koru_delegate_core::WorkspaceId::from_uuid(uuid::Uuid::new_v4()).to_storage(),
        "requested_by": {"kind": "person", "profile": uuid::Uuid::new_v4().to_string()},
        "objective": "test",
        "agent": {"kind": "built_in", "value": "opencode"},
        "scope": {"kind": "temporary"},
        "requested_model": "deepseek/deepseek-chat"
    }))
    .unwrap();
    for request in [
        Request::Create {
            caller: caller.clone(),
            request,
        },
        Request::Resume {
            caller,
            run,
            confirm: false,
        },
    ] {
        for stale in [1, 3, 4, 6] {
            let legacy = frame::encode(stale, &request).unwrap();
            assert!(
                read_current_request(&mut legacy.as_slice())
                    .unwrap()
                    .is_none()
            );
            let invalid_legacy = frame::encode(stale, &serde_json::json!({"old": true})).unwrap();
            assert!(
                read_current_request(&mut invalid_legacy.as_slice())
                    .unwrap()
                    .is_none(),
                "a stale body is rejected by version before current-shape decoding"
            );
        }
        let current = frame::encode(WIRE_VERSION, &request).unwrap();
        assert_eq!(
            read_current_request(&mut current.as_slice()).unwrap(),
            Some(request)
        );
    }
}
