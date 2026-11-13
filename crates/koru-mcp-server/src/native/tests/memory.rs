// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::native::memory::*;
use uuid::Uuid;

fn setup() -> (
    Arc<Embedded>,
    LocalBackend,
    NativeStoreIdentity,
    MemoryEditRecipe,
    MemoryWriteBinding,
) {
    let store = Embedded::new();
    let backend = store.backend();
    let key = identity();
    backend
        .execute_native_write(&create(&backend, store.workspace), key.clone())
        .unwrap();
    let doc = WorkspaceDoc::from_snapshot(&store.snapshot(store.workspace, key.candidate).unwrap())
        .unwrap();
    let root = doc.children(Parent::Root).unwrap()[0];
    let history = Uuid::new_v4();
    let recipe = MemoryEditRecipe {
        workspace: store.workspace,
        document: key.candidate,
        history,
        root: root.as_uuid(),
        change: MemoryRecipeChange::Remember {
            after: doc
                .children(Parent::Node(root))
                .unwrap()
                .last()
                .map(|n| n.as_uuid()),
            summary: "A remembered fact".into(),
            detail: "Supporting detail".into(),
        },
    };
    let owner = store.store.lock().unwrap().local_user_scope().unwrap();
    let reader = store.workspace(store.workspace).unwrap().owner;
    let binding = MemoryWriteBinding {
        authority: MemoryWriteAuthority {
            version: 1,
            owner,
            device: koru_ai_core::bots::DeviceId::new(),
            bot: Uuid::new_v4(),
            submission: Uuid::new_v4(),
            captured_config_revision: 1,
            creation_request: Uuid::new_v4(),
            workspace: store.workspace,
            memory_document: key.candidate,
            action: MemoryWriteAction::Remember,
        },
        history,
        expected_revision: doc.version().encode(),
        fact: Uuid::new_v4(),
        detail: Some(Uuid::new_v4()),
        source: Some(Uuid::new_v4()),
        source_run_document: StoreDocumentId::new(),
        created_at_ms: at().millis(),
        reader,
    };
    (store, backend, key, recipe, binding)
}
fn current(store: &Embedded, key: &NativeStoreIdentity) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&store.snapshot(store.workspace, key.candidate).unwrap()).unwrap()
}
fn fresh_key(key: &NativeStoreIdentity) -> NativeStoreIdentity {
    NativeStoreIdentity {
        request_key: Uuid::new_v4().to_string(),
        ..key.clone()
    }
}

#[test]
fn memory_remember_correct_forget_and_restore_preserve_fact_and_old_children() {
    let (store, backend, key, recipe, mut binding) = setup();
    let fact = NodeId::from_uuid(binding.fact);
    let old_detail = NodeId::from_uuid(binding.detail.unwrap());
    let old_source = NodeId::from_uuid(binding.source.unwrap());
    let prepared = backend
        .prepare_memory_write(recipe.clone(), binding.clone())
        .unwrap();
    backend
        .execute_native_write(&prepared, fresh_key(&key))
        .unwrap();
    let doc = current(&store, &key);
    assert_eq!(doc.read(fact).unwrap().meta.heading, Some(Heading::H2));
    assert_eq!(
        doc.children(Parent::Node(fact)).unwrap(),
        [old_detail, old_source]
    );
    assert_eq!(
        doc.read(old_detail).unwrap().content.as_text(),
        Some("Supporting detail")
    );
    let source = doc.read_node_content_strict(old_source, 1024).unwrap();
    let Content::Text { attrs, text } = source.content else {
        panic!()
    };
    assert_eq!(text, koru_core::ANCHOR.to_string());
    assert!(attrs.iter().any(|attr| matches!(&attr.value, AttrValue::Document(reference) if reference.document.as_uuid() == binding.source_run_document.as_uuid())));

    binding.authority.action = MemoryWriteAction::Correct;
    binding.expected_revision = doc.version().encode();
    binding.detail = Some(Uuid::new_v4());
    binding.source = Some(Uuid::new_v4());
    let corrected = MemoryEditRecipe {
        change: MemoryRecipeChange::Correct {
            fact: binding.fact,
            expected: "A remembered fact".into(),
            children: vec![
                MemoryExistingChild {
                    node: old_detail.as_uuid(),
                    expected: "Supporting detail".into(),
                },
                MemoryExistingChild {
                    node: old_source.as_uuid(),
                    expected: koru_core::ANCHOR.to_string(),
                },
            ],
            summary: "Corrected fact".into(),
            detail: "Replacement detail".into(),
        },
        ..recipe.clone()
    };
    backend
        .execute_native_write(
            &backend
                .prepare_memory_write(corrected, binding.clone())
                .unwrap(),
            fresh_key(&key),
        )
        .unwrap();
    let doc = current(&store, &key);
    assert_eq!(
        doc.read(fact).unwrap().content.as_text(),
        Some("Corrected fact")
    );
    assert!(doc.in_trash(old_detail));
    assert!(doc.in_trash(old_source));
    assert!(!doc.in_trash(fact));
    assert_eq!(
        doc.read(old_detail).unwrap().content.as_text(),
        Some("Supporting detail")
    );

    binding.authority.action = MemoryWriteAction::Forget;
    binding.expected_revision = doc.version().encode();
    binding.detail = None;
    binding.source = None;
    let forgotten = MemoryEditRecipe {
        change: MemoryRecipeChange::Forget {
            fact: binding.fact,
            expected: "Corrected fact".into(),
        },
        ..recipe
    };
    backend
        .execute_native_write(
            &backend.prepare_memory_write(forgotten, binding).unwrap(),
            fresh_key(&key),
        )
        .unwrap();
    let mut doc = current(&store, &key);
    assert!(doc.in_trash(fact));
    assert_eq!(doc.children(Parent::Node(fact)).unwrap().len(), 2);
    doc.restore(fact, None).unwrap();
    assert!(!doc.in_trash(fact));
    assert_eq!(
        doc.read(fact).unwrap().content.as_text(),
        Some("Corrected fact")
    );
}

#[test]
fn memory_fingerprint_normalizes_vv_but_keeps_first_bytes_and_separates_identity() {
    let (_store, backend, _key, recipe, mut binding) = setup();
    binding.expected_revision = vec![2, 1, 2, 2, 4];
    let first = backend
        .prepare_memory_write(recipe.clone(), binding.clone())
        .unwrap();
    binding.expected_revision = vec![2, 2, 4, 1, 2];
    let second = backend
        .prepare_memory_write(recipe.clone(), binding.clone())
        .unwrap();
    assert_eq!(
        first.description().fingerprint,
        second.description().fingerprint
    );
    assert_eq!(
        first.description().expected_revision.as_deref(),
        Some(&[2, 1, 2, 2, 4][..])
    );
    assert_eq!(
        second.description().expected_revision.as_deref(),
        Some(&[2, 2, 4, 1, 2][..])
    );
    let hash = second.description().fingerprint.clone();
    let mut changed = binding.clone();
    changed.source_run_document = StoreDocumentId::new();
    assert_ne!(
        backend
            .prepare_memory_write(recipe.clone(), changed)
            .unwrap()
            .description()
            .fingerprint,
        hash
    );
    let mut changed = binding.clone();
    changed.authority.bot = Uuid::new_v4();
    assert_ne!(
        backend
            .prepare_memory_write(recipe.clone(), changed)
            .unwrap()
            .description()
            .fingerprint,
        hash
    );
    let mut changed = binding.clone();
    changed.expected_revision = vec![2, 1, 4, 2, 4];
    assert_ne!(
        backend
            .prepare_memory_write(recipe.clone(), changed)
            .unwrap()
            .description()
            .fingerprint,
        hash
    );
    let mut changed = recipe.clone();
    let MemoryRecipeChange::Remember { detail, .. } = &mut changed.change else {
        panic!()
    };
    *detail = "Changed".into();
    assert_ne!(
        backend
            .prepare_memory_write(changed, binding.clone())
            .unwrap()
            .description()
            .fingerprint,
        hash
    );
    for invalid in [vec![], vec![255], vec![0; 65_537]] {
        binding.expected_revision = invalid;
        assert!(
            backend
                .prepare_memory_write(recipe.clone(), binding.clone())
                .is_err()
        );
    }
}

#[test]
fn memory_constructor_refuses_forged_cohorts_lf_and_escaped_overflow() {
    let (_store, backend, _key, recipe, binding) = setup();
    let mut wrong = binding.clone();
    wrong.authority.action = MemoryWriteAction::Forget;
    assert!(backend.prepare_memory_write(recipe.clone(), wrong).is_err());
    let mut wrong = binding.clone();
    wrong.detail = wrong.source;
    assert!(backend.prepare_memory_write(recipe.clone(), wrong).is_err());
    let mut wrong = binding.clone();
    wrong.history = Uuid::new_v4();
    assert!(backend.prepare_memory_write(recipe.clone(), wrong).is_err());
    let mut wrong = binding.clone();
    wrong.authority.memory_document = StoreDocumentId::new();
    assert!(backend.prepare_memory_write(recipe.clone(), wrong).is_err());
    for (summary, detail) in [
        ("bad\nsummary".to_owned(), "detail".to_owned()),
        ("summary".into(), "bad\ndetail".into()),
        ("summary".into(), "\u{1}".repeat(16 * 1024)),
    ] {
        let changed = MemoryEditRecipe {
            change: MemoryRecipeChange::Remember {
                after: None,
                summary,
                detail,
            },
            ..recipe.clone()
        };
        let error = backend
            .prepare_memory_write(changed, binding.clone())
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_request");
    }
}

#[test]
fn memory_execution_conflict_profile_refusal_and_lost_reply_stay_truthful() {
    let (store, backend, key, recipe, binding) = setup();
    let mut foreign = binding.clone();
    foreign.reader = koru_store_core::ProfileId::new();
    let wrong = backend
        .prepare_memory_write(recipe.clone(), foreign)
        .unwrap();
    assert!(matches!(
        backend.execute_native_write(&wrong, fresh_key(&key)),
        Err(NativeWriteError::Refused(_))
    ));
    assert_eq!(
        current(&store, &key).version(),
        strict_version(&binding.expected_revision).unwrap()
    );
    let prepared = backend.prepare_memory_write(recipe, binding).unwrap();
    store.race_edit.store(true, Ordering::SeqCst);
    assert!(matches!(
        backend.execute_native_write(&prepared, fresh_key(&key)),
        Err(NativeWriteError::Refused(_))
    ));

    let (store, backend, key, recipe, binding) = setup();
    let fact = binding.fact;
    let prepared = backend.prepare_memory_write(recipe, binding).unwrap();
    store.lose_edit_reply.store(true, Ordering::SeqCst);
    let operation = fresh_key(&key);
    assert!(matches!(
        backend.execute_native_write(&prepared, operation.clone()),
        Err(NativeWriteError::Unconfirmed(_))
    ));
    assert!(current(&store, &key).read(NodeId::from_uuid(fact)).is_ok());
    assert!(matches!(
        backend
            .reconcile_native_write(&prepared, operation.clone())
            .unwrap(),
        NativeReconciliation::Unconfirmed
    ));
    assert!(matches!(
        backend.execute_native_write(&prepared, operation),
        Err(NativeWriteError::Refused(_))
    ));
}

#[test]
fn memory_snapshot_bound_and_generic_native_permission_are_separate() {
    assert!(crate::native::memory::validate_snapshot(&[]).is_err());
    assert!(crate::native::memory::validate_snapshot(&vec![0; 16 * 1024 * 1024 + 1]).is_err());
    let (store, backend, key, recipe, binding) = setup();
    let result = backend.prepare_native_write("edit_document", json!({"workspace_id":store.workspace.to_storage(),"document_id":key.candidate.to_storage(),"base_revision":hex(&binding.expected_revision),"operations":[{"op":"insert_inline","node_id":recipe.root.to_string(),"expected":"Result","at_utf8":0,"inline":{"type":"document","document_id":binding.source_run_document.to_storage(),"node_id":null,"label":null}}]}));
    assert!(
        result
            .err()
            .unwrap()
            .message
            .contains("supported body operations")
    );
    assert!(
        backend
            .prepare_native_write("remember_memory", json!({}))
            .is_err()
    );
    let prepared = backend
        .prepare_memory_write(recipe, binding.clone())
        .unwrap();
    for bytes in [Vec::new(), vec![0; 16 * 1024 * 1024 + 1], vec![1, 2, 3]] {
        *store.snapshot_override.lock().unwrap() = Some(bytes);
        assert!(matches!(
            backend.execute_native_write(&prepared, fresh_key(&key)),
            Err(NativeWriteError::Refused(_))
        ));
        assert_eq!(
            store
                .store
                .lock()
                .unwrap()
                .version(store.workspace, key.candidate)
                .unwrap(),
            strict_version(&binding.expected_revision).unwrap()
        );
    }
}

#[test]
fn memory_internal_title_stays_stable_across_page_rename() {
    let (store, backend, key, recipe, binding) = setup();
    let before = backend
        .prepare_memory_write(recipe.clone(), binding.clone())
        .unwrap();
    assert_eq!(before.description().title, "Memory");
    store
        .store
        .lock()
        .unwrap()
        .rename_document_entry(store.workspace, key.candidate, "Renamed by owner", at())
        .unwrap();
    let after = backend.prepare_memory_write(recipe, binding).unwrap();
    assert_eq!(after.description().title, before.description().title);
    assert_eq!(
        after.description().fingerprint,
        before.description().fingerprint
    );
    let saved = backend
        .execute_native_write(&after, fresh_key(&key))
        .unwrap();
    assert_eq!(saved.title, "Renamed by owner");
}

fn add_collision_nodes(
    store: &Embedded,
    key: &NativeStoreIdentity,
    root: NodeId,
    beneath: NodeId,
) -> [NodeId; 3] {
    let mut doc = current(store, key);
    let before = doc.version();
    let unrelated = doc
        .insert(Parent::Node(root), Node::paragraph("unrelated").unwrap())
        .unwrap();
    doc.move_node(unrelated, Parent::Node(root), 0).unwrap();
    let nested = doc
        .insert(Parent::Node(beneath), Node::paragraph("nested").unwrap())
        .unwrap();
    let trashed = doc
        .insert(Parent::Node(root), Node::paragraph("trashed").unwrap())
        .unwrap();
    doc.trash(trashed, at().millis(), None).unwrap();
    let mut owner = store.store.lock().unwrap();
    owner
        .apply_update(
            store.workspace,
            key.candidate,
            doc.export_updates_since(&before).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    owner.flush(store.workspace, key.candidate, at()).unwrap();
    [unrelated, nested, trashed]
}

#[test]
fn memory_all_generated_ids_refuse_live_nested_and_trashed_collisions_before_write() {
    let (store, backend, key, recipe, mut binding) = setup();
    let root = NodeId::from_uuid(recipe.root);
    let body = current(&store, &key).children(Parent::Node(root)).unwrap()[0];
    let collisions = add_collision_nodes(&store, &key, root, body);
    binding.expected_revision = current(&store, &key).version().encode();
    let before = current(&store, &key).version();
    for collision in collisions {
        for slot in 0..3 {
            let mut changed = binding.clone();
            match slot {
                0 => changed.fact = collision.as_uuid(),
                1 => changed.detail = Some(collision.as_uuid()),
                _ => changed.source = Some(collision.as_uuid()),
            }
            let prepared = backend
                .prepare_memory_write(recipe.clone(), changed)
                .unwrap();
            assert!(matches!(
                backend.execute_native_write(&prepared, fresh_key(&key)),
                Err(NativeWriteError::Refused(_))
            ));
            assert_eq!(current(&store, &key).version(), before);
        }
    }
    backend
        .execute_native_write(
            &backend
                .prepare_memory_write(recipe.clone(), binding.clone())
                .unwrap(),
            fresh_key(&key),
        )
        .unwrap();
    let fact = NodeId::from_uuid(binding.fact);
    let old_detail = NodeId::from_uuid(binding.detail.unwrap());
    let old_source = NodeId::from_uuid(binding.source.unwrap());
    let collisions = add_collision_nodes(&store, &key, root, old_detail);
    let corrected = MemoryEditRecipe {
        change: MemoryRecipeChange::Correct {
            fact: binding.fact,
            expected: "A remembered fact".into(),
            children: vec![
                MemoryExistingChild {
                    node: old_detail.as_uuid(),
                    expected: "Supporting detail".into(),
                },
                MemoryExistingChild {
                    node: old_source.as_uuid(),
                    expected: koru_core::ANCHOR.to_string(),
                },
            ],
            summary: "new summary".into(),
            detail: "new detail".into(),
        },
        ..recipe
    };
    binding.authority.action = MemoryWriteAction::Correct;
    binding.expected_revision = current(&store, &key).version().encode();
    binding.detail = Some(Uuid::new_v4());
    binding.source = Some(Uuid::new_v4());
    let before = current(&store, &key).version();
    for collision in collisions {
        for slot in 0..2 {
            let mut changed = binding.clone();
            if slot == 0 {
                changed.detail = Some(collision.as_uuid());
            } else {
                changed.source = Some(collision.as_uuid());
            }
            let prepared = backend
                .prepare_memory_write(corrected.clone(), changed)
                .unwrap();
            assert!(matches!(
                backend.execute_native_write(&prepared, fresh_key(&key)),
                Err(NativeWriteError::Refused(_))
            ));
            let doc = current(&store, &key);
            assert_eq!(doc.version(), before);
            assert_eq!(
                doc.read(fact).unwrap().content.as_text(),
                Some("A remembered fact")
            );
            assert!(!doc.in_trash(old_detail));
        }
    }
}
