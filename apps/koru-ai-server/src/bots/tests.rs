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

/// **A Bot's new Role page reaches a window that has its workspace open**
/// (the first window run's finding 7): the window lists documents from its
/// manifest room, and the Role page is created inside the Store worker.
/// The room is told once, before the creation answers, with the entry the
/// Store published; a replay of the same creation tells it nothing.
#[test]
fn a_new_role_page_is_told_to_an_open_workspaces_manifest_room_once() {
    use koru_ai_core::Timestamp;
    use koru_store_core::Locale;
    use koru_store_server::memory::MemoryStore;
    let memory = MemoryStore::open("bot-role-announced-memory").unwrap();
    let (profile, workspace, manifest) = memory
        .with_store(|store| {
            let at = Timestamp::from_millis(1);
            let profile = store
                .create_profile("First", None, None, Locale::default(), at)
                .unwrap()
                .id;
            let workspace = store.create_workspace(profile, "Work", at).unwrap().id;
            let manifest = store.manifest_id(workspace).unwrap();
            (
                profile,
                workspace,
                store.snapshot(workspace, manifest).unwrap(),
            )
        })
        .unwrap();
    let mut replica = koru_store_core::manifest::Manifest::from_snapshot(&manifest).unwrap();
    let mut window = memory.join_manifest(workspace);
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
    let created = adapter.create_role_page(&seed).unwrap();
    assert_eq!(created.placement, BotRolePlacement::Live);
    let told = window.told();
    assert_eq!(told.len(), 1, "the room is told once");
    replica.import(&told[0]).unwrap();
    let entry = replica
        .entries()
        .into_iter()
        .find(|entry| entry.id == seed.candidate)
        .expect("the open window now lists the Role page");
    assert_eq!(entry.title, "Research role");
    assert_eq!(entry.kind, koru_core::explore::KIND_DOCUMENT);
    // A replay answers from the recorded origin and commits nothing.
    assert_eq!(
        adapter.create_role_page(&seed).unwrap().placement,
        BotRolePlacement::Live
    );
    assert!(window.told().is_empty(), "nothing new to tell");
}

#[test]
fn a_saved_reply_is_its_own_scope_and_a_reimported_retry_finds_its_page() {
    use koru_ai_core::Timestamp;
    use koru_store_core::Locale;
    use koru_store_server::memory::MemoryStore;
    let memory = MemoryStore::open("bot-reply-origin-memory").unwrap();
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
    let import = || {
        koru_import_markdown::document_snapshot(
            koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            "Launch plan",
            "# Plan\n\nShip it on Friday.",
        )
        .unwrap()
    };
    let seed = BotRoleSeed {
        owner: adapter.owner().unwrap(),
        workspace,
        profile,
        title: "Launch plan".into(),
        candidate: koru_ai_core::DocumentId::new(),
        request_key: uuid::Uuid::new_v4().to_string(),
        request_hash: "reply-hash".into(),
        seed: import(),
    };
    let first = adapter.create_reply_page(&seed).unwrap();
    assert!(first.saved_reply(&seed));
    assert!(!first.matches(&seed), "a reply page is never a Role page");
    assert_eq!(first.placement, BotRolePlacement::Live);
    let count = memory
        .with_store(move |store| store.documents(workspace).unwrap().len())
        .unwrap();
    // The retry imports the Markdown again; the recorded origin answers.
    let retry = BotRoleSeed {
        seed: import(),
        ..seed.clone()
    };
    let again = adapter.create_reply_page(&retry).unwrap();
    assert!(again.saved_reply(&retry));
    assert_eq!(again.placement, BotRolePlacement::Live);
    assert_eq!(
        memory
            .with_store(move |store| store.documents(workspace).unwrap().len())
            .unwrap(),
        count
    );
}
