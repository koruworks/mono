// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn metadata_epoch_and_bot_wrapper_preserve_the_original_golden_bytes() {
    let bot = super::super::tests::bot();
    let metadata = BotPolicyMetadata {
        may_ask: true,
        owner: owner(&bot),
        id: bot.id,
        home_workspace: bot.home_workspace,
        main_chat: bot.main_chat,
        revision: bot.revision,
        policy_revision: bot.policy_revision,
        lifecycle: bot.lifecycle,
        paused: bot.paused,
        access: bot.access,
        selected_pages: bot.selected_pages.clone(),
        pending_lifecycle: None,
        pending_change: None,
        connections: Default::default(),
    };
    let actual = key(metadata.owner, metadata.id, metadata.revision).unwrap();
    assert_eq!(actual.to_storage(), "702efbc8-a8b3-8a7f-b84a-8bdd51ac4385");
    assert_eq!(actual, epoch_key(&bot).unwrap());
    assert_eq!(metadata.policy(), bot_native_policy(&bot));
    assert!(key(metadata.owner, metadata.id, 0).is_err());
    assert!(key(metadata.owner, metadata.id, i64::MAX as u64 + 1).is_err());
    assert_ne!(
        actual,
        key(metadata.owner, metadata.id, metadata.revision + 1).unwrap()
    );
    assert_ne!(
        actual,
        key(
            BotOwner {
                device: DeviceId::new(),
                ..metadata.owner
            },
            metadata.id,
            metadata.revision
        )
        .unwrap()
    );
}
