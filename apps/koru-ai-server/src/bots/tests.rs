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

/// The strict Role creation against the real in-memory Store: a page the
/// person deleted forever is answered `Missing` and never registered again.
/// The Store's strict path alone would re-register it (the fake Store hides
/// that), because "delete forever" removes only the manifest entry.
#[test]
fn a_role_page_deleted_forever_is_answered_missing_and_never_registered_again() {
    use koru_ai_core::Timestamp;
    use koru_store_core::Locale;
    use koru_store_server::memory::MemoryStore;
    let memory = MemoryStore::open("bot-role-origin-memory").unwrap();
    let (profile, workspace) = memory
        .with_store(|store| {
            let at = Timestamp::from_millis(1);
            let profile = store
                .create_profile("First", None, None, Locale::default(), at)
                .unwrap()
                .id;
            let workspace = store.create_workspace(profile, "Work", at).unwrap().id;
            (profile, workspace)
        })
        .unwrap();
    let adapter =
        BotAuthorityAdapter::from_device(memory.client(), &uuid::Uuid::new_v4().to_string())
            .unwrap();
    let initial = koru_import_markdown::document_snapshot(
        koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
        "Research role",
        "Find primary sources.",
    )
    .unwrap();
    let seed = BotRoleSeed {
        owner: adapter.owner().unwrap(),
        workspace,
        profile,
        title: "Research role".into(),
        candidate: koru_ai_core::DocumentId::new(),
        request_key: uuid::Uuid::new_v4().to_string(),
        request_hash: blake3::hash(&initial).to_hex().to_string(),
        seed: initial,
    };
    let candidate = seed.candidate;
    let listed = move |memory: &MemoryStore| {
        memory
            .with_store(move |store| {
                (
                    store
                        .document_entry(workspace, candidate)
                        .unwrap()
                        .is_some(),
                    store.documents(workspace).unwrap().len(),
                )
            })
            .unwrap()
    };
    // No origin yet: created once, live.
    let first = adapter.create_role_page(&seed).unwrap();
    assert!(first.matches(&seed));
    assert_eq!(first.placement, BotRolePlacement::Live);
    let (present, count) = listed(&memory);
    assert!(present);
    // A replay answers from the recorded origin and creates nothing.
    let again = adapter.create_role_page(&seed).unwrap();
    assert!(again.matches(&seed));
    assert_eq!(again.placement, BotRolePlacement::Live);
    assert_eq!(listed(&memory), (true, count));
    // In Trash: answered as such.
    memory
        .with_store(move |store| {
            store
                .trash_document_entry(workspace, candidate, Timestamp::from_millis(5), None)
                .unwrap()
        })
        .unwrap();
    let trashed = adapter.create_role_page(&seed).unwrap();
    assert_eq!(trashed.placement, BotRolePlacement::Trashed);
    // Emptied from the bin, as a window does it: the manifest entry goes;
    // the Store keeps the document row and its origin.
    memory
        .with_store(move |store| {
            let manifest = store.manifest_id(workspace).unwrap();
            let mut remote = koru_store_core::manifest::Manifest::from_snapshot(
                &store.snapshot(workspace, manifest).unwrap(),
            )
            .unwrap();
            let before = remote.version();
            remote.remove_document(candidate).unwrap();
            let delta = remote.export_updates_since(&before).unwrap();
            let at = Timestamp::from_millis(6);
            store
                .apply_update(workspace, manifest, delta, at, None)
                .unwrap();
            store.flush(workspace, manifest, at).unwrap();
        })
        .unwrap();
    assert_eq!(listed(&memory), (false, count - 1));
    for _ in 0..2 {
        let gone = adapter.create_role_page(&seed).unwrap();
        assert!(gone.matches(&seed), "the origin is still the original");
        assert_eq!(gone.placement, BotRolePlacement::Missing);
        // Not re-registered: the person's deletion stands.
        assert_eq!(listed(&memory), (false, count - 1));
    }
    // Another request under the same identity is answered with the recorded
    // origin, which does not match it: a conflict for the writer, no write.
    let rival = BotRoleSeed {
        request_key: uuid::Uuid::new_v4().to_string(),
        ..seed.clone()
    };
    assert!(!adapter.create_role_page(&rival).unwrap().matches(&rival));
    assert_eq!(listed(&memory), (false, count - 1));
}
