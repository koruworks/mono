// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_api::bots::{BotCommand, BotDelegateAuthority};
use koru_delegate_core::{AttemptGeneration, DeviceId, RunId, RunToolAuthority};

#[test]
fn retirement_keeps_authenticated_ordinary_run_lookup_but_closes_bot_commands() {
    let capability =
        BotDelegateAuthority::new("fixture-capability-012345678901234567890123456789".into())
            .unwrap();
    let device = DeviceId::new();
    let identity = Identity {
        launch_id: "fixture".into(),
        authority: "stop-only".into(),
        device_id: device.to_storage(),
        bot_authority: Some(capability.clone()),
        run_authorities: Default::default(),
    };
    let lookup = BotCommand::ResolveRunAuthority {
        run: RunId::mint(),
        generation: AttemptGeneration::FIRST,
    };
    assert!(admits_bot_command(&identity, &capability, &lookup));
    assert!(permits_legacy_command(false, &lookup));
    assert!(permits_resolved_authority(
        false,
        &RunToolAuthority::Ordinary {}
    ));
    let wrong =
        BotDelegateAuthority::new("another-capability-012345678901234567890123456789".into())
            .unwrap();
    assert!(!admits_bot_command(&identity, &wrong, &lookup));
    let bot_command: BotCommand = serde_json::from_value(serde_json::json!({
        "command":"reconcile", "key":{"owner":{"user_scope":uuid::Uuid::new_v4(),"device":device.as_uuid()},
        "bot":uuid::Uuid::new_v4(),"request":uuid::Uuid::new_v4()}
    })).unwrap();
    assert!(admits_bot_command(&identity, &capability, &bot_command));
    assert!(!permits_legacy_command(false, &bot_command));
}
