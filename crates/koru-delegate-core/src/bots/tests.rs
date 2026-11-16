// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_native_drain_reason_requires_exact_subject_and_request_fields() {
    let reason = NativeDrainReason::RoutinePolicyPending {
        owner: BotOwner {
            user_scope: UserScopeId::new(),
            device: crate::DeviceId::from_uuid(Uuid::new_v4()),
        },
        bot: BotId::mint(),
        schedule: crate::ScheduleId::from_uuid(Uuid::new_v4()),
        request: NativePolicyRequestId::from(BotRequestId::mint()),
    };
    let value = serde_json::to_value(&reason).unwrap();
    assert_eq!(value["reason"], "routine_policy_pending");
    assert_eq!(
        serde_json::from_value::<NativeDrainReason>(value.clone()).unwrap(),
        reason
    );
    for field in ["owner", "bot", "schedule", "request"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<NativeDrainReason>(missing).is_err());
    }
    let mut extra = value.clone();
    extra["trusted"] = true.into();
    assert!(serde_json::from_value::<NativeDrainReason>(extra).is_err());
    let mut invalid = value;
    invalid["request"] = Uuid::nil().to_string().into();
    assert!(serde_json::from_value::<NativeDrainReason>(invalid).is_err());
}

#[test]
fn private_submission_closes_nested_intent_and_input_without_changing_ordinary_types() {
    let mut request = crate::tests::request();
    let workspace = WorkspaceId::from_uuid(Uuid::new_v4());
    request.scope = crate::WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("/bot".into()).unwrap(),
    };
    request.grants = BTreeSet::from([workspace]);
    let submission = BotSubmission {
        provenance: BotProvenance {
            routine: None,
            owner: BotOwner {
                user_scope: UserScopeId::new(),
                device: request.executing_device,
            },
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: BTreeSet::new(),
            connections: Default::default(),
        },
        request,
        workspace,
        input: koru_session_protocol::PendingInput {
            idempotency_key: crate::IdempotencyKey::mint(),
            text: "CURRENT-TASK-SENTINEL".into(),
            attachment_ids: vec![],
            references: vec![],
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec![],
            model: None,
        },
    };
    let encoded = serde_json::to_value(&submission).unwrap();
    assert_eq!(
        serde_json::from_value::<BotSubmission>(encoded.clone()).unwrap(),
        submission
    );
    assert!(!format!("{submission:?}").contains("CURRENT-TASK-SENTINEL"));
    for key in ["request", "input"] {
        let mut changed = encoded.clone();
        changed[key]["undeclared_authority"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BotSubmission>(changed.clone()).is_err());
        // The reused public types deliberately retain their existing decoder.
        if key == "request" {
            assert!(
                serde_json::from_value::<crate::DelegationRequest>(changed[key].clone()).is_ok()
            );
        } else {
            assert!(
                serde_json::from_value::<koru_session_protocol::PendingInput>(changed[key].clone())
                    .is_ok()
            );
        }
    }
    for key in ["requested_by", "scope", "origin", "agent"] {
        let mut changed = encoded.clone();
        changed["request"][key]["undeclared_authority"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<BotSubmission>(changed).is_err(),
            "{key}"
        );
    }
    for noncanonical in [
        "ABCDEF01-2345-4678-8ABC-1234567890AB",
        "abcdef01234546788abc1234567890ab",
    ] {
        let mut changed = encoded.clone();
        changed["request"]["agent"] = serde_json::json!({"kind":"custom","value":noncanonical});
        assert!(
            serde_json::from_value::<BotSubmission>(changed).is_err(),
            "v1 profile {noncanonical}"
        );
    }
    let mut changed = encoded;
    changed["request"]["context"] = serde_json::json!([{
        "kind":"document", "workspace":workspace, "document":Uuid::new_v4(),
        "version":Uuid::new_v4()
    }]);
    assert!(serde_json::from_value::<BotSubmission>(changed.clone()).is_ok());
    changed["request"]["context"][0]["undeclared_authority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotSubmission>(changed).is_err());
}

#[test]
fn strict_private_profile_keeps_legacy_custom_uuid_domain_and_public_tolerance() {
    #[derive(Deserialize)]
    struct Private {
        #[serde(deserialize_with = "crate::bot_wire::deserialize_profile")]
        agent: crate::SessionProfileKey,
    }
    for value in [
        "abcdef01-2345-4678-8abc-1234567890ab",
        "ABCDEF01-2345-4678-8ABC-1234567890AB",
        "abcdef01234546788abc1234567890ab",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let public = serde_json::from_value::<crate::SessionProfileKey>(
            serde_json::json!({"kind":"custom","value":value}),
        );
        let private = serde_json::from_value::<Private>(
            serde_json::json!({"agent":{"kind":"custom","value":value}}),
        );
        assert_eq!(private.is_ok(), public.is_ok(), "{value}");
        if let (Ok(private), Ok(public)) = (private, public) {
            assert_eq!(private.agent, public)
        }
    }
    let unknown = serde_json::json!({"kind":"built_in","value":"codex","extra":true});
    assert!(serde_json::from_value::<crate::SessionProfileKey>(unknown.clone()).is_ok());
    assert!(serde_json::from_value::<Private>(serde_json::json!({"agent":unknown})).is_err());
}
#[test]
fn folders_are_absolute_portable_and_redacted() {
    for path in [
        "/owner/bot",
        "/",
        "D:/owner/bot",
        "C:\\",
        "\\\\server\\share",
        "\\\\?\\C:\\",
        "\\\\?\\UNC\\server\\share",
    ] {
        let folder = BotWorkingFolder::new(path.into()).unwrap();
        assert!(!format!("{folder:?}").contains(path));
        let scope = crate::WorkspaceScope::BotFolder { folder };
        assert!(!scope.is_koru_owned());
    }
    for path in [
        "relative",
        "",
        "C:relative",
        "/bad\0path",
        "\\\\",
        "\\\\server",
        "\\\\server\\",
        "\\relative",
    ] {
        assert!(BotWorkingFolder::new(path.into()).is_err());
    }
}
#[test]
fn provenance_refuses_wrong_ai_identity_and_unknown_fields() {
    let mut value = serde_json::json!({"owner":{"user_scope":koru_store_core::UserScopeId::new(),"device":Uuid::new_v4()},
        "bot":BotId::mint(),"submission":BotRequestId::mint(),"chat_id":Uuid::new_v4(),
        "ai_attempt_id":null,"assistant_message_id":null,"config_revision":1,"policy_revision":1,"continued_from":null});
    assert!(
        serde_json::from_value::<BotProvenance>(value.clone())
            .unwrap()
            .valid()
    );
    value["chat_id"] = serde_json::json!(Uuid::nil());
    assert!(
        !serde_json::from_value::<BotProvenance>(value.clone())
            .unwrap()
            .valid()
    );
    value["authority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotProvenance>(value).is_err());
}
