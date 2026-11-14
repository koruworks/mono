// SPDX-License-Identifier: MPL-2.0
//! Real in-memory creation and occurrence rows; owner/Store ports are explicit
//! trusted boundary fixtures. No provider or Routine execution is enabled.
use super::*;
use koru_ai_core::bots::*;
use koru_ai_core::{DocumentId, ProfileId};
use koru_delegate_core::{
    DocumentVersionId, RoutineClaimIdentity, RoutineClaimRecord, RoutineDigest32, RoutineSlot,
};
use std::sync::{Mutex, atomic::AtomicUsize};

struct Fixture {
    repository: Repository,
    bot: Bot,
    profile: ProfileId,
    request: NativePolicyRequest,
    claim: RoutineClaimRecord,
}

/// Reuse the actual in-memory retained-row fixture for portable preclaim
/// continuation validation; no additional writer or Store authority seam.
pub(in crate::worker::bots) fn preclaim_fixture() -> (
    Repository,
    koru_delegate_api::routines::SynchronizeRoutinePreclaim,
) {
    let f = Fixture::new(false);
    let row = f
        .repository
        .routine_preparation_for_claim(&f.claim.claim)
        .unwrap()
        .unwrap();
    let request = koru_delegate_api::routines::SynchronizeRoutinePreclaim {
        slot: row.prepared.slot,
        semantic_hash: row.prepared.semantic_hash,
        preparation_hash: row.prepared.preparation_hash,
    };
    (f.repository, request)
}
impl Fixture {
    fn new(linked: bool) -> Self {
        let mut repository = Repository::in_memory().unwrap();
        let mut bot = super::super::tests::bot();
        bot.revision = 1;
        let profile = ProfileId::new();
        repository
            .save_bot(&bot, profile, BotRequestId::mint(), &"a".repeat(64), None)
            .unwrap();
        let command: BotRoutineCreate = serde_json::from_value(serde_json::json!({
            "request":BotRequestId::mint(),"bot":bot.id,"expected_bot_revision":1,
            "name":"Proof fixture","objective":"Inspect captured task","documents":[DocumentId::new()],
            "recurrence":{"type":"floating","cron":"0 9 * * *"}
        })).unwrap();
        repository
            .prepare_bot_routine_creation(
                owner(&bot),
                &command,
                RoutineDigest32::from_bytes([7; 32]),
                10,
            )
            .unwrap();
        let creation = repository
            .bot_routine_creation_by_request(owner(&bot), bot.id, command.request)
            .unwrap()
            .unwrap();
        repository
            .transition_bot_routine_creation(
                &creation.identity,
                creation.observed(),
                BotRoutineCreationTransition::Confirmed(BotRoutineStoreConfirmation {
                    identity: creation.identity.clone(),
                    origin_edit_revision: vec![1, 2, 3],
                }),
                11,
            )
            .unwrap();
        let slot = RoutineSlot {
            owner: creation.identity.owner,
            bot: bot.id,
            schedule: creation.identity.schedule,
            occurrence_ms: 60_000,
        };
        let (identity, canonical, profile) = repository.routine_occurrence_source(&slot).unwrap();
        let mut definition = identity.seed.header.definition.clone();
        definition.revision = 2;
        definition.lifecycle = serde_json::from_str("\"active\"").unwrap();
        let input = BotRoutineCapturedSources {
            slot,
            definition,
            owner_handoff: None,
            creation: identity.route(),
            role: BotKnowledgeCapture {
                page: canonical.config.role.clone(),
                version: DocumentVersionId::new(),
            },
            task: command
                .documents
                .iter()
                .map(|document| BotKnowledgeCapture {
                    page: BotPage {
                        workspace: canonical.home,
                        document: *document,
                    },
                    version: DocumentVersionId::new(),
                })
                .collect(),
            canonical,
            profile,
            grant: identity.grant,
            knowledge: vec![],
            memory: BotCapturedMemory::Absent {},
            index_digest: [1; 32],
        };
        let BotRoutinePreparationAdmission::Ready(row) = repository
            .prepare_routine_occurrence(&input, 60_001)
            .unwrap()
        else {
            panic!("fixture capacity");
        };
        let mut claim = RoutineClaimRecord {
            claim: RoutineClaimIdentity {
                slot,
                definition: row.prepared.definition,
                ids: row.prepared.ids,
                preparation_hash: row.prepared.preparation_hash,
                claimed_at_ms: 60_123,
            },
            submission_version: 2,
            submission: row.prepared.submission.clone(),
            link_state: RoutineLinkState::Held {},
            updated_at_ms: 60_123,
        };
        if linked {
            repository
                .finalize_routine_occurrence(
                    &claim,
                    &BotRoutineFinalizeAuthority {
                        owner: slot.owner,
                        home: bot.home_workspace,
                        profile,
                    },
                    "koru-chat-v1",
                    60_200,
                )
                .unwrap();
            claim.link_state = RoutineLinkState::Linked {};
        }
        let request = NativePolicyRequest {
            run: row.prepared.ids.run,
            provenance: row.prepared.submission.provenance.clone(),
            captured: row.prepared.submission.policy.clone(),
            target: None,
        };
        Self {
            repository,
            bot,
            profile,
            request,
            claim,
        }
    }
    fn snapshot(&self) -> Box<RoutineLookup> {
        snapshot(&self.repository, &self.bot, &self.request)
            .unwrap()
            .unwrap()
    }
}

struct Delegate {
    reply: BotReply,
    budgets: Mutex<Vec<Duration>>,
    expire: bool,
}
impl Delegate {
    fn new(record: RoutineClaimRecord) -> Self {
        Self {
            reply: BotReply::RoutineClaim {
                observation: RoutineClaimObservation::Claimed { record },
            },
            budgets: Mutex::new(Vec::new()),
            expire: false,
        }
    }
}
impl BotDelegatePort for Delegate {
    fn execute(&self, _: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("Routine proof requires finite owner read")
    }
    fn execute_within(
        &self,
        command: DelegateCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(matches!(command, DelegateCommand::ReadRoutineClaim { .. }));
        assert!(!within.is_zero());
        self.budgets.lock().unwrap().push(within);
        if self.expire {
            std::thread::sleep(within);
        }
        Ok(self.reply.clone())
    }
}

#[test]
fn routine_proof_binds_real_finalized_capture_to_exact_independent_link() {
    let f = Fixture::new(true);
    let lookup = f.snapshot();
    let delegate = Delegate::new(f.claim.clone());
    lookup
        .verify_delegate(&delegate, Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(delegate.budgets.lock().unwrap().len(), 1);
    let current = f.repository.bot(f.bot.id).unwrap();
    recheck(
        &f.repository,
        current.as_ref(),
        &f.bot,
        &f.request,
        Some(&lookup),
    )
    .unwrap();
    let independent = lookup.barrier();
    assert_eq!(
        independent.request,
        lookup.creation.identity.creation_request.into()
    );
    assert_eq!(independent.grant_revision, 1);
    assert_eq!(independent.policy, lookup.creation.identity.grant.policy);
    assert!(independent.effects_allowed);
}

#[test]
fn routine_proof_refuses_unmaterialized_and_caller_identity_substitution() {
    let draft = Fixture::new(false);
    assert!(snapshot(&draft.repository, &draft.bot, &draft.request).is_err());
    let f = Fixture::new(true);
    for change in 0..8 {
        let mut r = f.request.clone();
        match change {
            0 => r.provenance.routine = None,
            1 => r.provenance.routine.as_mut().unwrap().grant_revision += 1,
            2 => r.provenance.submission = BotRequestId::mint(),
            3 => r.run = RunId::mint(),
            4 => r.provenance.owner.device = DeviceId::new(),
            5 => r.captured.access = koru_delegate_core::BotAccess::Workspace,
            6 => r.provenance.config_revision += 1,
            _ => r.provenance.policy_revision += 1,
        }
        assert!(
            snapshot(&f.repository, &f.bot, &r).is_err(),
            "substitution {change}"
        );
    }
}

#[test]
fn routine_proof_refuses_held_absent_terminal_and_changed_owner_claim() {
    let f = Fixture::new(true);
    let lookup = f.snapshot();
    for change in 0..7 {
        let mut record = f.claim.clone();
        match change {
            0 => record.link_state = RoutineLinkState::Held {},
            1 => record.claim.ids.run = RunId::mint(),
            2 => record.claim.preparation_hash = RoutineDigest32::from_bytes([9; 32]),
            3 => record.submission.provenance.config_revision += 1,
            4 => record.claim.slot.owner.device = DeviceId::new(),
            5 => record.submission_version = 1,
            _ => record.claim.ids.assistant_message = Uuid::new_v4(),
        }
        assert!(
            lookup
                .verify_delegate(
                    &Delegate::new(record),
                    Instant::now() + Duration::from_secs(1)
                )
                .is_err(),
            "claim {change}"
        );
    }
    let mut delegate = Delegate::new(f.claim.clone());
    delegate.reply = BotReply::RoutineClaim {
        observation: RoutineClaimObservation::Absent {
            slot: f.claim.claim.slot,
            observed_at_ms: 60_400,
        },
    };
    assert!(
        lookup
            .verify_delegate(&delegate, Instant::now() + Duration::from_secs(1))
            .is_err()
    );
    delegate.reply = BotReply::Missing;
    assert!(
        lookup
            .verify_delegate(&delegate, Instant::now() + Duration::from_secs(1))
            .is_err()
    );
    // Terminal is valid retained evidence, but cannot grant fresh permission.
    let mut terminal = f.claim;
    terminal.link_state = RoutineLinkState::Terminal {
        reason: koru_delegate_core::RoutineTerminalReason::Stopped,
    };
    assert!(
        lookup
            .verify_delegate(
                &Delegate::new(terminal),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
    );
}

#[test]
fn routine_proof_writer_rechecks_entire_binding_grant_and_occurrence_snapshot() {
    let f = Fixture::new(true);
    let original = f.snapshot();
    for change in 0..6 {
        let mut prior = original.clone();
        match change {
            0 => prior.creation.origin_edit_revision = Some(vec![9]),
            1 => prior.creation.identity.grant.source_bot_policy_revision += 1,
            2 => prior.creation.state = BotRoutineCreationState::Prepared,
            3 => prior.occurrence.updated_at_ms += 1,
            4 => prior.occurrence.stage = BotRoutineStoredStage::Prepared,
            _ => prior.occurrence.prepared.ids.chat = Uuid::new_v4(),
        }
        assert!(
            recheck(
                &f.repository,
                Some(&f.bot),
                &f.bot,
                &f.request,
                Some(&prior)
            )
            .is_err(),
            "snapshot {change}"
        );
    }
    assert!(recheck(&f.repository, Some(&f.bot), &f.bot, &f.request, None).is_err());
    assert!(recheck(&f.repository, None, &f.bot, &f.request, Some(&original)).is_err());
}

#[test]
fn routine_proof_current_bot_change_refuses_inflight_but_keeps_initial_grant_epoch() {
    let mut f = Fixture::new(true);
    let prior = f.snapshot();
    let old = f.bot.clone();
    f.bot.revision += 1;
    f.bot.config.name = "Later name".into();
    f.repository
        .save_bot(
            &f.bot,
            f.profile,
            BotRequestId::mint(),
            &"b".repeat(64),
            Some(old.revision),
        )
        .unwrap();
    let current = f.repository.bot(f.bot.id).unwrap().unwrap();
    assert!(
        recheck(
            &f.repository,
            Some(&current),
            &old,
            &f.request,
            Some(&prior)
        )
        .is_err()
    );
    let later = snapshot(&f.repository, &current, &f.request)
        .unwrap()
        .unwrap();
    assert_eq!(later.barrier(), prior.barrier());
    assert_ne!(barrier(&current).unwrap().key, barrier(&old).unwrap().key);
    let mut paused = current.clone();
    paused.paused = true;
    assert_eq!(
        barrier(&paused).unwrap(),
        barrier(&current).unwrap(),
        "O88 Pause is admission-only"
    );
    assert!(same_canonical(&paused, &current));
    for change in 0..4 {
        let mut changed = current.clone();
        match change {
            0 => changed.home_workspace = WorkspaceId::new(),
            1 => changed.config.model.requested_model = "different".into(),
            2 => changed.access = BotAccess::Workspace,
            _ => changed.selected_pages.push(changed.config.role.clone()),
        }
        assert!(!same_canonical(&changed, &current));
    }
}

#[test]
fn routine_proof_owner_read_respects_absolute_phase_deadline() {
    let f = Fixture::new(true);
    let lookup = f.snapshot();
    let mut delegate = Delegate::new(f.claim);
    assert!(lookup.verify_delegate(&delegate, Instant::now()).is_err());
    assert!(delegate.budgets.lock().unwrap().is_empty());
    delegate.expire = true;
    assert!(
        lookup
            .verify_delegate(&delegate, Instant::now() + Duration::from_millis(30))
            .is_err()
    );
    assert!(
        delegate
            .budgets
            .lock()
            .unwrap()
            .iter()
            .all(|b| *b <= Duration::from_millis(30))
    );
}

struct Authority {
    owner: BotOwner,
    workspace: WorkspaceId,
    profile: ProfileId,
    allowed: bool,
    pages: AtomicUsize,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, w: WorkspaceId) -> Result<ProfileId, BotError> {
        if self.allowed && w == self.workspace {
            Ok(self.profile)
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.workspace(p.workspace).map(|_| ())
    }
}

#[test]
fn routine_proof_job_checks_human_target_before_independent_owner_read() {
    let f = Fixture::new(true);
    let delegate = Arc::new(Delegate::new(f.claim.clone()));
    for allowed in [false, true] {
        let authority = Arc::new(Authority {
            owner: owner(&f.bot),
            workspace: f.bot.home_workspace,
            profile: f.profile,
            allowed,
            pages: AtomicUsize::new(0),
        });
        let (reply, _) = sync_channel(1);
        let mut request = f.request.clone();
        request.target = Some(DocumentId::new());
        let job = BotPolicyJob {
            bot: f.bot.clone(),
            authority: authority.clone(),
            delegate: Some(delegate.clone()),
            work: PolicyWork::Lookup {
                request,
                routine: Some(f.snapshot()),
                reply,
                deadline: Instant::now() + Duration::from_secs(1),
            },
        };
        let outcome = job.run();
        assert_eq!(outcome.result.is_ok(), allowed);
        assert_eq!(delegate.budgets.lock().unwrap().len(), usize::from(allowed));
        if allowed {
            assert_eq!(authority.pages.load(Ordering::SeqCst), 1);
        }
    }
}
