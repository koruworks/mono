// SPDX-License-Identifier: MPL-2.0
use super::*;

fn allocation_request(
    s: &BotSubmission,
    request: &PrepareNativeResult,
) -> AllocateMemoryEditRequest {
    AllocateMemoryEditRequest {
        key: request.key.clone(),
        caller_fingerprint: [50; 32],
        subject: MemoryEditSubjectV1 {
            version: 1,
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            submission: s.provenance.submission,
            captured_config_revision: s.provenance.config_revision,
            action: MemoryEditAction::Remember,
        },
        base: MemoryBaseSelector::OriginalSeed {},
        current_config_revision: request.config_revision,
        current_policy_revision: request.policy_revision,
        existing_fact_id: None,
    }
}
fn base(r: &AllocateMemoryEditRequest, workspace: WorkspaceId) -> MemoryBaseResolution {
    MemoryBaseResolution::Resolved {
        authority: MemoryEditAuthorityV1 {
            version: 1,
            owner: r.subject.owner,
            bot: r.subject.bot,
            submission: r.subject.submission,
            captured_config_revision: r.subject.captured_config_revision,
            creation_request: BotRequestId::mint(),
            workspace,
            memory_document: DocumentId::from_uuid(uuid::Uuid::new_v4()),
            action: MemoryEditAction::Remember,
        },
        version: DocumentVersionId::from_uuid(uuid::Uuid::new_v4()),
        expected_revision: vec![2, 1, 2, 2, 4],
    }
}

#[test]
fn memory_receipts_reconcile_after_effects_off_without_saved_result_publication() {
    let (mut h, s, fence, mut policy, mut edit) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy.clone(), device)
        .unwrap();
    let request = allocation_request(&s, &edit);
    let allocation = h
        .orchestrator
        .allocate_memory_edit(&request, device)
        .unwrap();
    assert_eq!(
        Some(allocation.source_run_document),
        h.documents.lock().unwrap().by_run.get(&fence.run).copied()
    );
    let resolution = base(&request, s.workspace);
    let saved = h
        .orchestrator
        .resolve_memory_edit_base(
            &request.key,
            request.caller_fingerprint,
            &resolution,
            policy.config_revision,
            policy.policy_revision,
            device,
        )
        .unwrap();
    let MemoryBaseResolution::Resolved {
        authority,
        expected_revision,
        ..
    } = &resolution
    else {
        panic!()
    };
    edit.kind = NativeResultOperation::EditPage;
    edit.target = Some(authority.memory_document);
    edit.expected_revision = Some(expected_revision.clone());
    let first = h
        .orchestrator
        .prepare_memory_edit(&edit, request.caller_fingerprint, authority, device)
        .unwrap();
    assert_eq!(first.disposition, NativeResultDisposition::Execute);
    policy.key.request = BotRequestId::mint().into();
    policy.config_revision += 1;
    policy.policy_revision += 1;
    policy.effects_allowed = false;
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(policy.clone(), device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Pending
    );
    assert_eq!(
        h.orchestrator
            .allocate_memory_edit(&request, device)
            .unwrap(),
        saved
    );
    let mut reordered = resolution.clone();
    let MemoryBaseResolution::Resolved {
        expected_revision, ..
    } = &mut reordered
    else {
        panic!()
    };
    *expected_revision = vec![2, 2, 4, 1, 2];
    assert_eq!(
        h.orchestrator
            .resolve_memory_edit_base(
                &request.key,
                request.caller_fingerprint,
                &reordered,
                1,
                1,
                device
            )
            .unwrap(),
        saved
    );
    assert_eq!(
        h.orchestrator
            .prepare_memory_edit(&edit, request.caller_fingerprint, authority, device)
            .unwrap()
            .disposition,
        NativeResultDisposition::ReconcileOnly
    );
    let evidence = NativeResultReference {
        version_id: None,
        operation_id: edit.key.operation_id,
        workspace: edit.destination,
        document: first.target,
        version: vec![1],
        title: "Memory".into(),
        confirmed_at_ms: 100,
        kind: NativeResultKind::EditedPage,
    };
    // Leave Verified in the shared ledger, as if confirmation's IPC response
    // or owner publication had been interrupted, then exercise owner recovery.
    h.schedule_store
        .confirm_native_result(&ConfirmNativeResult {
            key: edit.key.clone(),
            request_fingerprint: edit.request_fingerprint,
            observation: NativeResultObservation::Durable {
                evidence: evidence.clone(),
            },
        })
        .unwrap();
    assert!(
        h.orchestrator
            .reconcile_verified_native_for_run(fence.run)
            .unwrap()
    );
    let registered = h
        .orchestrator
        .reconcile_native_result(&edit.key, device)
        .unwrap();
    assert_eq!(registered.stage, NativeResultStage::Registered);
    assert_eq!(registered.verified, Some(evidence));
    assert_eq!(
        h.orchestrator
            .prepare_memory_edit(&edit, request.caller_fingerprint, authority, device)
            .unwrap(),
        registered
    );
    assert!(
        h.orchestrator
            .header(fence.run)
            .unwrap()
            .native_results
            .is_empty()
    );
    assert!(
        !h.schedule_store
            .created_native_page(
                &BotRunKey {
                    owner: fence.owner,
                    bot: fence.bot,
                    run: fence.run
                },
                s.workspace,
                first.target
            )
            .unwrap()
    );
    assert!(
        h.orchestrator
            .read_memory_edit_allocation(
                fence.run,
                request.key.operation_id,
                request.caller_fingerprint,
                DeviceId::new()
            )
            .is_err()
    );
    let mut fresh = request.clone();
    fresh.key.operation_id = uuid::Uuid::new_v4();
    fresh.current_config_revision = policy.config_revision;
    fresh.current_policy_revision = policy.policy_revision;
    assert!(h.orchestrator.allocate_memory_edit(&fresh, device).is_err());
}

fn no_cleanup(h: &Harness) {
    let authority = h.authority.lock().unwrap();
    assert!(authority.folder_lookups.is_empty());
    assert!(authority.forgotten.is_empty());
    assert!(h.registrations.lock().unwrap().retired.is_empty());
    assert!(!h.ledger.lock().unwrap().ops.contains(&"forget_run"));
}
fn clear_cleanup_observations(h: &Harness) {
    h.authority.lock().unwrap().folder_lookups.clear();
    h.ledger.lock().unwrap().ops.clear();
}
fn retained_code(error: RunError) -> String {
    let RunError::Port(error) = error else {
        panic!("expected retained-state port refusal: {error:?}")
    };
    error.code
}

#[test]
fn retained_bot_purge_refuses_before_any_external_cleanup_or_ledger_delete() {
    let (mut h, _, fence, _, _) = active();
    clear_cleanup_observations(&h);
    assert_eq!(
        retained_code(h.orchestrator.purge(fence.run).unwrap_err()),
        "bot_retained_state"
    );
    no_cleanup(&h);
    assert!(h.schedule_store.bot_admission(fence.run).unwrap().is_some());
    h.ledger.lock().unwrap().fail = Some("bot_admission_read");
    assert_eq!(
        retained_code(h.orchestrator.purge(fence.run).unwrap_err()),
        "bot_retention_unavailable"
    );
    no_cleanup(&h);
}

#[test]
fn bot_header_or_folder_without_admission_refuses_purge_and_synced_ordinary_remains_supported() {
    for bot_header in [true, false] {
        let mut h = bot_harness();
        let s = submission(72);
        let mut ordinary = s.request.clone();
        ordinary.scope = WorkspaceScope::None;
        h.orchestrator.create(ordinary, s.workspace).unwrap();
        if bot_header {
            let state = h.orchestrator.open(s.request.run).unwrap();
            let mut header = state.doc.document().unwrap().header().unwrap();
            header.bot = Some(s.provenance.clone());
            header.scope = s.request.scope.clone();
            state.doc = crate::orchestrator::RunState::new(
                state.workspace,
                state.document,
                Box::new(MemoryHandle {
                    id: state.document,
                    doc: RunDocument::create(&header).unwrap(),
                    script: h.documents.clone(),
                }),
            )
            .doc;
        } else {
            h.schedule_store.forget_run(s.request.run).unwrap();
            h.schedule_store
                .record_intent(&s.request, s.workspace)
                .unwrap();
        }
        clear_cleanup_observations(&h);
        assert_eq!(
            retained_code(h.orchestrator.purge(s.request.run).unwrap_err()),
            "bot_retention_inconsistent"
        );
        no_cleanup(&h);
    }
    let mut h = bot_harness();
    let run = run_id(73);
    h.orchestrator
        .create(request(run, WorkspaceScope::None), workspace())
        .unwrap();
    // A canonical synced ordinary document may have no local creation intent.
    // The supported Bot lookup positively returns None, rather than failing.
    h.schedule_store.forget_run(run).unwrap();
    clear_cleanup_observations(&h);
    let result = h.orchestrator.purge(run).unwrap();
    assert_eq!(result.folder, FolderOutcome::Kept);
    assert_eq!(h.authority.lock().unwrap().folder_lookups, vec![run]);
    assert_eq!(h.authority.lock().unwrap().forgotten, vec![run]);
    assert!(h.ledger.lock().unwrap().ops.contains(&"forget_run"));
}
