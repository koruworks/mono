// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn trusted_device_requires_existing_non_nil_uuid() {
    assert!(trusted_device("").is_none());
    assert!(trusted_device("caller-label").is_none());
    assert!(trusted_device("00000000-0000-0000-0000-000000000000").is_none());
    assert!(trusted_device("00000000000000000000000000000000").is_none());
    let id = uuid::Uuid::new_v4();
    assert_eq!(
        trusted_device(&id.to_string()),
        Some(DeviceId::from_uuid(id))
    );
}
#[test]
fn role_history_capture_checks_current_profile_owner_and_nontrash_membership() {
    use koru_ai_core::{DocumentDestination, StoreContextPort, Timestamp};
    use koru_store_core::Locale;
    use koru_store_server::memory::MemoryStore;
    let memory = MemoryStore::open("bot-capture-memory").unwrap();
    let (profile, other, workspace) = memory
        .with_store(|store| {
            let at = Timestamp::from_millis(1);
            let profile = store
                .create_profile("First", None, None, Locale::default(), at)
                .unwrap()
                .id;
            let other = store
                .create_profile("Second", None, None, Locale::default(), at)
                .unwrap()
                .id;
            let workspace = store.create_workspace(profile, "Work", at).unwrap().id;
            (profile, other, workspace)
        })
        .unwrap();
    let context = crate::store_context::StoreContextAdapter::new(memory.client());
    let document = context
        .create_document(
            uuid::Uuid::new_v4(),
            &DocumentDestination {
                profile_id: profile,
                workspace_id: workspace,
                parent_document_id: None,
                index: None,
                title: "Role".into(),
            },
            "Role content",
        )
        .unwrap();
    let adapter =
        BotAuthorityAdapter::from_device(memory.client(), &uuid::Uuid::new_v4().to_string())
            .unwrap();
    let owner = adapter.owner().unwrap();
    let page = BotPage {
        workspace,
        document,
    };
    let pins = adapter
        .capture(owner, workspace, profile, std::slice::from_ref(&page))
        .unwrap();
    assert!(
        matches!(pins.as_slice(),[koru_delegate_core::RunContextItem::Document{workspace:w,document:d,..}] if *w==workspace && *d==document)
    );
    assert!(
        adapter
            .capture(
                BotOwner {
                    user_scope: UserScopeId::new(),
                    ..owner
                },
                workspace,
                profile,
                std::slice::from_ref(&page)
            )
            .is_err()
    );
    memory
        .with_store(move |store| store.move_workspace(workspace, other).unwrap())
        .unwrap();
    assert!(
        adapter
            .capture(owner, workspace, profile, std::slice::from_ref(&page))
            .is_err()
    );
    assert!(
        adapter
            .capture(owner, workspace, other, std::slice::from_ref(&page))
            .is_ok()
    );
    memory
        .with_store(move |store| {
            store
                .trash_document_entry(workspace, document, Timestamp::from_millis(5), None)
                .unwrap()
        })
        .unwrap();
    assert!(adapter.capture(owner, workspace, other, &[page]).is_err());
}
