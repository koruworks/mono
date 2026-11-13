// SPDX-License-Identifier: MPL-2.0
use super::*;
mod migration;

fn request(h: &Harness, seed: bool) -> AllocateMemoryEditRequest {
    let provenance =
        h.db.bot_admission(h.fence.run)
            .unwrap()
            .unwrap()
            .submission
            .provenance;
    AllocateMemoryEditRequest {
        key: h.request.key.clone(),
        caller_fingerprint: [40; 32],
        subject: MemoryEditSubjectV1 {
            version: 1,
            owner: provenance.owner,
            bot: provenance.bot,
            submission: provenance.submission,
            captured_config_revision: provenance.config_revision,
            action: MemoryEditAction::Remember,
        },
        base: if seed {
            MemoryBaseSelector::OriginalSeed {}
        } else {
            MemoryBaseSelector::History {
                version: DocumentVersionId::from_uuid(Uuid::new_v4()),
            }
        },
        current_config_revision: h.policy.config_revision,
        current_policy_revision: h.policy.policy_revision,
        existing_fact_id: None,
    }
}
fn allocate(h: &mut Harness, request: &AllocateMemoryEditRequest) -> MemoryEditAllocation {
    h.db.allocate_memory_edit(request, fixtures::document(0xd0), &h.fence, &h.policy)
        .unwrap()
}
fn authority(request: &AllocateMemoryEditRequest) -> MemoryEditAuthorityV1 {
    MemoryEditAuthorityV1 {
        version: 1,
        owner: request.subject.owner,
        bot: request.subject.bot,
        submission: request.subject.submission,
        captured_config_revision: request.subject.captured_config_revision,
        creation_request: BotRequestId::mint(),
        workspace: fixtures::workspace(0xa2),
        memory_document: fixtures::document(0xd8),
        action: request.subject.action,
    }
}
fn resolution(request: &AllocateMemoryEditRequest) -> MemoryBaseResolution {
    MemoryBaseResolution::Resolved {
        authority: authority(request),
        version: match request.base {
            MemoryBaseSelector::History { version } => version,
            MemoryBaseSelector::OriginalSeed {} => DocumentVersionId::from_uuid(Uuid::new_v4()),
        },
        expected_revision: vec![2, 1, 2, 2, 4],
    }
}
fn resolve(
    h: &mut Harness,
    request: &AllocateMemoryEditRequest,
    resolution: &MemoryBaseResolution,
) -> MemoryEditAllocation {
    h.db.resolve_memory_edit_base(
        &request.key,
        request.caller_fingerprint,
        resolution,
        &h.fence,
        &h.policy,
    )
    .unwrap()
}
fn prepare(
    h: &mut Harness,
    request: &AllocateMemoryEditRequest,
    resolution: &MemoryBaseResolution,
) -> NativeIntentRecord {
    let MemoryBaseResolution::Resolved {
        authority,
        expected_revision,
        ..
    } = resolution
    else {
        panic!()
    };
    h.request.kind = NativeResultOperation::EditPage;
    h.request.target = Some(authority.memory_document);
    h.request.expected_revision = Some(expected_revision.clone());
    h.db.prepare_memory_edit(&h.request, request.caller_fingerprint, authority)
        .unwrap()
}

#[test]
fn allocation_replay_preserves_ids_before_live_fences_and_rejects_namespace_collisions() {
    let mut h = Harness::new();
    let r = request(&h, true);
    let first = allocate(&mut h, &r);
    assert!(first.valid());
    assert_eq!(first.ensure.as_ref().unwrap().title, "Memory");
    assert_ne!(
        first.ensure.as_ref().unwrap().request_id.as_uuid(),
        r.key.operation_id
    );
    assert_eq!(
        h.db.prepare_native_result(&h.request),
        Err(BotLedgerError::Conflict)
    );
    let narrowed = h.narrow();
    h.db.install_native_policy(&narrowed).unwrap();
    h.db.restart_in_memory();
    assert_eq!(allocate(&mut h, &r), first);
    let mut changed = r.clone();
    changed.caller_fingerprint[0] ^= 1;
    assert_eq!(
        h.db.allocate_memory_edit(&changed, first.source_run_document, &h.fence, &h.policy),
        Err(BotLedgerError::Conflict)
    );
    let mut changed = r.clone();
    changed.key.turn = AgentTurnId::mint();
    assert_eq!(
        h.db.allocate_memory_edit(&changed, first.source_run_document, &h.fence, &h.policy),
        Err(BotLedgerError::Conflict)
    );
    let mut next = r.clone();
    next.key.operation_id = Uuid::new_v4();
    assert!(
        h.db.allocate_memory_edit(&next, first.source_run_document, &h.fence, &h.policy)
            .is_err()
    );

    let mut h = Harness::new();
    let original = h.prepare();
    let r = request(&h, false);
    assert_eq!(
        h.db.allocate_memory_edit(&r, fixtures::document(0xd0), &h.fence, &h.policy),
        Err(BotLedgerError::Conflict)
    );
    assert_eq!(h.prepare(), original); // Generic Result hash and retry body are unchanged.
    assert_eq!(original.purpose, NativeEffectPurpose::Result {});
}

#[test]
fn resolve_uses_semantic_revision_equality_and_retains_first_encoding_after_narrowing() {
    let mut h = Harness::new();
    let r = request(&h, false);
    allocate(&mut h, &r);
    let first = resolution(&r);
    let saved = resolve(&mut h, &r, &first);
    let narrowed = h.narrow();
    h.db.install_native_policy(&narrowed).unwrap();
    h.db.restart_in_memory();
    let mut reordered = first.clone();
    let MemoryBaseResolution::Resolved {
        expected_revision, ..
    } = &mut reordered
    else {
        panic!()
    };
    *expected_revision = vec![2, 2, 4, 1, 2];
    assert_eq!(resolve(&mut h, &r, &reordered), saved);
    for bytes in [vec![], vec![255; 32], vec![0; 65537], vec![1, 1, 6]] {
        let mut bad = first.clone();
        let MemoryBaseResolution::Resolved {
            expected_revision, ..
        } = &mut bad
        else {
            panic!()
        };
        *expected_revision = bytes;
        assert!(
            h.db.resolve_memory_edit_base(&r.key, r.caller_fingerprint, &bad, &h.fence, &h.policy)
                .is_err()
        );
        assert_eq!(
            h.db.memory_edit_allocation(r.key.run, r.key.operation_id, r.caller_fingerprint)
                .unwrap(),
            Some(saved.clone())
        );
    }
}

#[test]
fn memory_effect_has_one_lease_and_settles_without_created_page_authority() {
    let mut h = Harness::new();
    let r = request(&h, false);
    allocate(&mut h, &r);
    let resolution = resolution(&r);
    resolve(&mut h, &r, &resolution);
    let first = prepare(&mut h, &r, &resolution);
    assert!(matches!(first.purpose, NativeEffectPurpose::Memory { .. }));
    assert_eq!(
        h.db.prepare_native_result(&h.request),
        Err(BotLedgerError::Conflict)
    );
    assert!(h.lease().execute.is_some());
    h.db.restart_in_memory();
    assert!(h.lease().execute.is_none());
    assert_eq!(
        h.db.pending_native_effects(h.fence.owner, Some(h.fence.bot), None, 16)
            .unwrap()
            .targets
            .len(),
        1
    );
    let narrowed = h.narrow();
    assert_eq!(
        h.db.install_native_policy(&narrowed).unwrap().state,
        BotPolicyBarrierState::Pending
    );
    h.confirm(NativeResultObservation::Unconfirmed);
    assert!(h.db.native_effects_pending(h.fence.run).unwrap());
    let e = h.evidence();
    h.confirm(NativeResultObservation::Durable {
        evidence: e.clone(),
    });
    assert_eq!(
        h.db.register_native_result(&h.request.key, &e)
            .unwrap()
            .stage,
        NativeResultStage::Registered
    );
    let current = h.db.native_result(&h.request.key).unwrap().unwrap();
    assert_eq!(prepare(&mut h, &r, &resolution), current);
    assert!(
        !h.db
            .created_native_page(h.fence.owner, h.fence.bot, e.workspace, e.document)
            .unwrap()
    );
    assert_eq!(
        h.db.settle_native_policy(&narrowed.key).unwrap().state,
        BotPolicyBarrierState::Effective
    );
}

#[test]
fn memory_existing_fact_ids_keep_core_uuid_domain_and_prepare_requires_frozen_base() {
    for fact in [
        Uuid::nil(),
        Uuid::parse_str("12345678-1234-5234-8234-123456789abc").unwrap(),
    ] {
        let mut h = Harness::new();
        let mut r = request(&h, false);
        r.subject.action = MemoryEditAction::Correct;
        r.existing_fact_id = Some(fact);
        let a = allocate(&mut h, &r);
        assert_eq!(a.fact_id, fact);
        let resolved = resolution(&r);
        resolve(&mut h, &r, &resolved);
        let record = prepare(&mut h, &r, &resolved);
        let MemoryBaseResolution::Resolved { authority, .. } = &resolved else {
            panic!()
        };
        h.request.expected_revision = Some(vec![2, 2, 4, 1, 2]);
        assert_eq!(
            h.db.prepare_memory_edit(&h.request, r.caller_fingerprint, authority),
            Err(BotLedgerError::Conflict)
        );
        assert_eq!(h.db.native_result(&h.request.key).unwrap(), Some(record));
    }
}

#[test]
fn allocation_capacity_counts_unlinked_rows_and_exact_retry_precedes_limit() {
    let mut h = Harness::new();
    let first = request(&h, true);
    let saved = allocate(&mut h, &first);
    for _ in 1..MEMORY_EDIT_UNLINKED_LIMIT {
        let mut r = first.clone();
        r.key.operation_id = Uuid::new_v4();
        allocate(&mut h, &r);
    }
    let mut overflow = first.clone();
    overflow.key.operation_id = Uuid::new_v4();
    assert_eq!(
        h.db.allocate_memory_edit(&overflow, fixtures::document(0xd0), &h.fence, &h.policy),
        Err(BotLedgerError::Capacity)
    );
    assert_eq!(allocate(&mut h, &first), saved);
}

#[test]
fn allocation_total_capacity_includes_linked_effects_and_replay_never_spends_capacity() {
    let mut h = Harness::new();
    let first = request(&h, false);
    let mut retained = None;
    for i in 0..MEMORY_EDIT_ALLOCATION_LIMIT {
        let mut r = first.clone();
        if i != 0 {
            r.key.operation_id = Uuid::new_v4();
        }
        h.request.key = r.key.clone();
        allocate(&mut h, &r);
        let resolution = resolution(&r);
        let saved = resolve(&mut h, &r, &resolution);
        prepare(&mut h, &r, &resolution);
        if i == 0 {
            retained = Some(saved);
        }
    }
    let mut overflow = first.clone();
    overflow.key.operation_id = Uuid::new_v4();
    assert_eq!(
        h.db.allocate_memory_edit(&overflow, fixtures::document(0xd0), &h.fence, &h.policy),
        Err(BotLedgerError::Capacity)
    );
    assert_eq!(allocate(&mut h, &first), retained.unwrap());
}

#[test]
fn memory_fresh_lease_refuses_foreign_fences_stale_policies_and_effects_off() {
    let mut h = Harness::new();
    let r = request(&h, false);
    allocate(&mut h, &r);
    let resolved = resolution(&r);
    resolve(&mut h, &r, &resolved);
    let record = prepare(&mut h, &r, &resolved);
    let mut fences = Vec::new();
    let mut wrong = h.fence.clone();
    wrong.owner.device = DeviceId::new();
    fences.push(wrong);
    let mut wrong = h.fence.clone();
    wrong.bot = BotId::mint();
    fences.push(wrong);
    let mut wrong = h.fence.clone();
    wrong.attempt = RunAttemptId::mint();
    fences.push(wrong);
    let mut wrong = h.fence.clone();
    wrong.generation = AttemptGeneration::FIRST.next();
    fences.push(wrong);
    let mut wrong = h.fence.clone();
    wrong.turn = Some(AgentTurnId::mint());
    fences.push(wrong);
    for wrong in fences {
        assert!(
            h.db.lease_native_result(&r.key, h.request.request_fingerprint, &wrong, &h.policy)
                .is_err()
        );
        assert_eq!(h.db.native_result(&r.key).unwrap(), Some(record.clone()));
    }
    let mut stale = h.policy.clone();
    stale.config_revision += 1;
    assert!(
        h.db.lease_native_result(&r.key, h.request.request_fingerprint, &h.fence, &stale)
            .is_err()
    );
    let off = h.narrow();
    h.db.install_native_policy(&off).unwrap();
    assert!(
        h.db.lease_native_result(&r.key, h.request.request_fingerprint, &h.fence, &off)
            .is_err()
    );
    assert_eq!(h.db.native_result(&r.key).unwrap(), Some(record));
    let MemoryBaseResolution::Resolved { mut authority, .. } = resolved else {
        panic!()
    };
    authority.creation_request = BotRequestId::mint();
    assert_eq!(
        h.db.prepare_memory_edit(&h.request, r.caller_fingerprint, &authority),
        Err(BotLedgerError::Conflict)
    );
}

#[test]
fn allocation_corruption_refuses_impossible_seed_revision_and_broken_binding() {
    let mut h = Harness::new();
    // A later accepted configuration makes a positive but lower stored Ensure
    // revision an impossible value, without relying on a CHECK violation.
    h.db.conn().execute("UPDATE bot_admissions SET config_revision=2,payload_json=json_set(payload_json,'$.provenance.config_revision',2)",[]).unwrap();
    h.policy.key.request = BotRequestId::mint().into();
    h.policy.config_revision = 2;
    h.db.install_native_policy(&h.policy).unwrap();
    let mut r = request(&h, true);
    r.current_config_revision = 2;
    allocate(&mut h, &r);
    h.db.conn()
        .execute(
            "UPDATE bot_memory_edit_allocations SET ensure_expected_revision=1",
            [],
        )
        .unwrap();
    assert!(
        h.db.memory_edit_allocation(r.key.run, r.key.operation_id, r.caller_fingerprint)
            .is_err()
    );
    h.db.conn().execute("UPDATE bot_memory_edit_allocations SET ensure_expected_revision=2,source_run_document_id=?1",[fixtures::document(0xef).to_storage()]).unwrap();
    assert!(
        h.db.memory_edit_allocation(r.key.run, r.key.operation_id, r.caller_fingerprint)
            .is_err()
    );
    h.db.conn()
        .execute(
            "UPDATE bot_memory_edit_allocations SET source_run_document_id=?1,turn_id=?2",
            rusqlite::params![
                fixtures::document(0xd0).to_storage(),
                AgentTurnId::mint().to_string()
            ],
        )
        .unwrap();
    assert!(
        h.db.memory_edit_allocation(r.key.run, r.key.operation_id, r.caller_fingerprint)
            .is_err()
    );
}

#[test]
fn allocation_and_resolution_crashes_preserve_their_own_durable_boundaries() {
    for moment in Moment::ALL {
        let mut h = Harness::new();
        let r = request(&h, true);
        h.db.arm_crash(CrashPoint {
            op: WriteOp::AllocateMemoryEdit,
            at: moment,
        });
        assert!(
            h.db.allocate_memory_edit(&r, fixtures::document(0xd0), &h.fence, &h.policy)
                .is_err()
        );
        h.db.restart_in_memory();
        let retained =
            h.db.memory_edit_allocation(r.key.run, r.key.operation_id, r.caller_fingerprint)
                .unwrap();
        assert_eq!(retained.is_some(), moment == Moment::AfterCommit);
        let saved = allocate(&mut h, &r);
        if let Some(retained) = retained {
            assert_eq!(saved, retained);
        }
        let resolution = resolution(&r);
        h.db.arm_crash(CrashPoint {
            op: WriteOp::ResolveMemoryEditBase,
            at: moment,
        });
        assert!(
            h.db.resolve_memory_edit_base(
                &r.key,
                r.caller_fingerprint,
                &resolution,
                &h.fence,
                &h.policy
            )
            .is_err()
        );
        h.db.restart_in_memory();
        let retained =
            h.db.memory_edit_allocation(r.key.run, r.key.operation_id, r.caller_fingerprint)
                .unwrap()
                .unwrap();
        assert_eq!(
            matches!(retained.resolution, MemoryBaseResolution::Resolved { .. }),
            moment == Moment::AfterCommit
        );
        assert!(matches!(
            resolve(&mut h, &r, &resolution).resolution,
            MemoryBaseResolution::Resolved { .. }
        ));
    }
}
