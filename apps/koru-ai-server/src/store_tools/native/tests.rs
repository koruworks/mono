// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_server::memory::MemoryStore;

fn setup() -> (MemoryStore, NativeStoreAuthority, ProfileId) {
    let memory = MemoryStore::open("native-authority-memory").unwrap();
    let (authority, other) = memory
        .with_store(|store| {
            let at = Timestamp::now();
            let profile = store
                .create_profile("First", None, None, koru_store_core::Locale::default(), at)
                .unwrap()
                .id;
            let other = store
                .create_profile("Second", None, None, koru_store_core::Locale::default(), at)
                .unwrap()
                .id;
            let workspace = store.create_workspace(profile, "Work", at).unwrap().id;
            (
                NativeStoreAuthority {
                    owner_scope: store.local_user_scope().unwrap(),
                    expected_profile: profile,
                    workspace,
                },
                other,
            )
        })
        .unwrap();
    (memory, authority, other)
}

#[test]
fn native_adapter_checks_current_placement_inside_effect_owner_and_refuses_before_seed() {
    let (memory, authority, other) = setup();
    let backend = native_store_backend(memory.client(), authority.clone());
    let prepared = backend
        .prepare_native_write(
            "create_document",
            serde_json::json!({"workspace_id":authority.workspace,"title":"Result"}),
        )
        .unwrap();
    let workspace = authority.workspace;
    memory
        .with_store(move |store| store.move_workspace(workspace, other).unwrap())
        .unwrap();
    let identity = NativeStoreIdentity {
        candidate: DocumentId::new(),
        scope: "native-test".into(),
        request_key: uuid::Uuid::new_v4().to_string(),
    };
    assert!(matches!(
        backend.execute_native_write(&prepared, identity),
        Err(NativeWriteError::Refused(_))
    ));
    assert!(
        memory
            .with_store(move |store| store.documents(workspace).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_target_visibility_inherits_ancestor_trash_and_denies_other_owner() {
    let (memory, authority, _) = setup();
    let workspace = authority.workspace;
    let (parent, child) = memory
        .with_store(move |store| {
            let (parent, _) = store
                .create_document(workspace, "Parent", "page", None, None, Timestamp::now())
                .unwrap();
            let (child, _) = store
                .create_document(
                    workspace,
                    "Child",
                    "page",
                    Some(parent),
                    None,
                    Timestamp::now(),
                )
                .unwrap();
            (parent, child)
        })
        .unwrap();
    let native = NativeStore {
        client: memory.client(),
        authority: authority.clone(),
    };
    assert!(native.document_entry(workspace, child).unwrap().is_some());
    let bot = crate::bots::BotAuthorityAdapter::from_device(
        memory.client(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let page = koru_ai_core::bots::BotPage {
        workspace,
        document: child,
    };
    use koru_ai_core::bots::BotAuthority;
    assert!(
        bot.inspect(&[workspace], std::slice::from_ref(&page))
            .unwrap()
            .page(&page)
            .is_ok()
    );
    memory
        .with_store(move |store| {
            store
                .trash_document_entry(workspace, parent, Timestamp::now(), None)
                .unwrap()
        })
        .unwrap();
    assert!(native.document_entry(workspace, child).unwrap().is_none());
    assert!(native.snapshot(workspace, child).is_err());
    assert!(
        bot.inspect(&[workspace], std::slice::from_ref(&page))
            .unwrap()
            .page(&page)
            .is_err()
    );
    let stranger = NativeStore {
        client: memory.client(),
        authority: NativeStoreAuthority {
            owner_scope: UserScopeId::new(),
            ..authority
        },
    };
    assert!(stranger.workspace(workspace).is_err());
    assert!(native.persist_update(workspace, child, Vec::new()).is_err());
}
