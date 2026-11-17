// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn pinned_history_revision_checks_owner_profile_and_exact_immutable_identity() {
    use koru_ai_core::{DocumentDestination, StoreContextPort, Timestamp};
    use koru_store_core::Locale;
    let memory =
        koru_store_server::memory::MemoryStore::open("bot-pinned-revision-memory").unwrap();
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
    let destination = DocumentDestination {
        profile_id: profile,
        workspace_id: workspace,
        parent_document_id: None,
        index: None,
        title: "Task".into(),
    };
    let document = context
        .create_document(uuid::Uuid::new_v4(), &destination, "Original task page")
        .unwrap();
    let other_document = context
        .create_document(uuid::Uuid::new_v4(), &destination, "Another page")
        .unwrap();
    let adapter =
        BotAuthorityAdapter::from_device(memory.client(), &uuid::Uuid::new_v4().to_string())
            .unwrap();
    let owner = adapter.owner().unwrap();
    let pins = adapter
        .capture(
            owner,
            workspace,
            profile,
            &[BotPage {
                workspace,
                document,
            }],
        )
        .unwrap();
    let version = pins[0].document_version().unwrap().1;
    let original = adapter
        .pinned_document_revision(owner, workspace, profile, document, version)
        .unwrap();
    memory
        .with_store(move |store| {
            let snapshot = store.snapshot(workspace, document).unwrap();
            let mut changed = koru_core::WorkspaceDoc::from_snapshot(&snapshot).unwrap();
            changed
                .insert(
                    koru_core::Parent::Root,
                    koru_core::Node::paragraph("Later edit").unwrap(),
                )
                .unwrap();
            store
                .apply_update(
                    workspace,
                    document,
                    changed.export_all_updates().unwrap(),
                    Timestamp::from_millis(2),
                    None,
                )
                .unwrap();
            store
                .flush(workspace, document, Timestamp::from_millis(2))
                .unwrap();
        })
        .unwrap();
    let live = context.resolve_document(document, Some(profile)).unwrap();
    assert_ne!(live.source_revision, original);
    assert_eq!(
        adapter
            .pinned_document_revision(owner, workspace, profile, document, version)
            .unwrap(),
        original
    );
    assert_eq!(
        adapter.pinned_document_revision(
            BotOwner {
                user_scope: UserScopeId::new(),
                ..owner
            },
            workspace,
            profile,
            document,
            version
        ),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        adapter.pinned_document_revision(
            BotOwner {
                device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
                ..owner
            },
            workspace,
            profile,
            document,
            version
        ),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        adapter.pinned_document_revision(owner, workspace, other, document, version),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        adapter.pinned_document_revision(owner, workspace, profile, other_document, version),
        Err(BotError::Invalid)
    );
    assert_eq!(
        adapter.pinned_document_revision(
            owner,
            workspace,
            profile,
            document,
            koru_delegate_core::DocumentVersionId::new()
        ),
        Err(BotError::Unavailable)
    );
    memory
        .with_store(move |store| store.move_workspace(workspace, other).unwrap())
        .unwrap();
    assert_eq!(
        adapter.pinned_document_revision(owner, workspace, profile, document, version),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        adapter
            .pinned_document_revision(owner, workspace, other, document, version)
            .unwrap(),
        original
    );
    memory
        .with_store(move |store| {
            store
                .trash_document_entry(workspace, document, Timestamp::from_millis(3), None)
                .unwrap()
        })
        .unwrap();
    assert_eq!(
        adapter.pinned_document_revision(owner, workspace, other, document, version),
        Err(BotError::Forbidden)
    );
    use koru_store_core::errors::Code;
    assert_eq!(
        history_revision_error(Code::PayloadTooLarge),
        BotError::ContextTooLarge
    );
    assert_eq!(
        history_revision_error(Code::CorruptState),
        BotError::Invalid
    );
    assert_eq!(
        history_revision_error(Code::StorageFailed),
        BotError::Unavailable
    );
}

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

/// Cohort `m8_feed_withheld`: the rule itself. A Bot is copied unless the
/// lower of its home workspace's stored mode and that workspace's Profile's
/// is Local only.
#[test]
fn m8_feed_withheld_home_copy_is_the_lower_of_the_two_stored_modes() {
    use koru_store_core::transport::TransportMode::{Cloud, LocalOnly, P2p};
    for (workspace, profile, expected) in [
        (Cloud, Cloud, HomeCopy::Copied),
        (P2p, Cloud, HomeCopy::Copied),
        (Cloud, P2p, HomeCopy::Copied),
        (P2p, P2p, HomeCopy::Copied),
        (LocalOnly, Cloud, HomeCopy::LocalOnly),
        (LocalOnly, P2p, HomeCopy::LocalOnly),
        // The Profile's choice caps every workspace in it.
        (Cloud, LocalOnly, HomeCopy::LocalOnly),
        (P2p, LocalOnly, HomeCopy::LocalOnly),
        (LocalOnly, LocalOnly, HomeCopy::LocalOnly),
    ] {
        assert_eq!(
            home_copy_of(workspace, profile),
            expected,
            "{workspace:?} {profile:?}"
        );
    }
}

/// Cohort `m8_feed_withheld`: the real adapter over an in-memory Store reads
/// the two stored rows, and answers `Unavailable` when a row is not there.
#[test]
fn m8_feed_withheld_the_adapter_reads_the_stored_modes_of_the_workspace_and_its_profile() {
    use koru_ai_core::Timestamp;
    use koru_store_core::Locale;
    use koru_store_core::transport::{SetTransportMode, TransportMode, TransportSubject};
    let memory = koru_store_server::memory::MemoryStore::open("bot-home-copy-memory").unwrap();
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
    let stored = |subject: TransportSubject| {
        memory
            .with_store(move |store| store.transport_mode(subject).unwrap())
            .unwrap()
    };
    let set = |subject: TransportSubject, mode: TransportMode| {
        let revision = stored(subject).revision;
        memory
            .with_store(move |store| {
                store
                    .set_transport_mode(SetTransportMode {
                        subject,
                        expected_revision: revision,
                        mode,
                    })
                    .unwrap();
            })
            .unwrap();
    };
    let own = TransportSubject::Workspace(workspace);
    let above = TransportSubject::Profile(profile);
    // As the Store seeded them, whatever that is: the answer is the rule on
    // the two stored rows, and the workspace's row names its Profile.
    assert_eq!(stored(own).owner, profile);
    assert_eq!(
        adapter.home_copy(workspace),
        Ok(home_copy_of(stored(own).mode, stored(above).mode))
    );
    // A workspace cannot sit above its Profile, so the Profile is set first.
    set(above, TransportMode::Cloud);
    set(own, TransportMode::P2p);
    assert_eq!(adapter.home_copy(workspace), Ok(HomeCopy::Copied));
    set(own, TransportMode::Cloud);
    assert_eq!(adapter.home_copy(workspace), Ok(HomeCopy::Copied));
    // Kept on this computer, by the person's own choice.
    set(own, TransportMode::LocalOnly);
    assert_eq!(stored(own).mode, TransportMode::LocalOnly);
    assert_eq!(adapter.home_copy(workspace), Ok(HomeCopy::LocalOnly));
    set(own, TransportMode::P2p);
    assert_eq!(adapter.home_copy(workspace), Ok(HomeCopy::Copied));
    // A workspace the Store does not know: no row, so no answer, and the
    // caller withholds.
    assert_eq!(
        adapter.home_copy(WorkspaceId::new()),
        Err(BotError::Unavailable)
    );
}
