// SPDX-License-Identifier: MPL-2.0

//! A decision-chain builder for composition tests above this crate, compiled
//! for this crate's own tests and for a dependent that turns on the dev-only
//! `test-support` feature in its `[dev-dependencies]` (nothing shipped
//! enables it; AGENTS.md, Testing, says why a `cfg(test)` guard is not
//! enough).
//!
//! [`decision_chain`] builds an [`Orchestrator`] over the real in-memory
//! ledger (`RunsDb` on `:memory:`, behind a pass-through store that only
//! leaves out the process-global worker counters, which other tests in the
//! same binary assert deltas of) and the minimal ports in this module, and
//! drives it **only through public orchestrator methods** — Bot submission,
//! boot replay, dispatch, observation, the initial input, policy sync, Koru
//! edit decision preparation and the initial projection's publication — to a
//! Koru edit decision a person must allow. No ledger row is written directly,
//! no proof is minted that the ledger would refuse from a trusted caller, and
//! no private field is touched: a dependent crate could do every step itself.
//!
//! The session host is scripted ([`SessionScript`]): it answers only what a
//! test queued and refuses everything else. The other ports are inert: Run
//! documents in memory (their handle stages decision projections with the
//! real `koru-delegate-document` staging), a capture that pins and writes
//! nothing, a bus that keeps nothing, a fixed preamble, Bot folders resolved
//! to their pure fixture identity (no folder is canonicalized), a run
//! authority that records the working folder, no registrations, one shared
//! clock that never moves by itself, and a sleep that never waits. Nothing
//! touches the disk, a Store, a provider, a browser or the network.
//!
//! Scripted runtime composition, not provider qualification.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};

use koru_delegate_core::decisions::{
    BotWaitEnrollmentReply, DecisionAnswerChoice, DecisionAnswerId, DecisionDigest,
    DecisionEditAction, DecisionId, EnrollBotWaitCleanup,
};
use koru_delegate_core::{
    ActorRef, AgentTurnId, AttemptGeneration, BotAccess, BotAdmissionBarrier, BotBootScan,
    BotBootState, BotExecutionFence, BotId, BotOwner, BotPolicyBarrier, BotPolicyBarrierKey,
    BotProvenance, BotRequestId, BotRunKey, BotSubmission, BotSubmissionKey, BotWorkingFolder,
    CapturedBotPolicy, DelegateBootId, DelegationRequest, DeviceId, DocumentId,
    FreshBotPolicyProof, HostInstanceId, IdempotencyKey, Objective, PendingInput, ProfileId,
    RoutinePolicyBarrier, RunAttemptId, RunId, SessionId, SessionProfileKey, WorkspaceId,
    WorkspaceScope,
};
use koru_delegate_sqlite::{
    AnswerDecision, DecisionAcknowledgedContinuation, DecisionRecord, DecisionWaitExecution, RunsDb,
};
use koru_session_protocol::CreationLookup;

use crate::{
    AppliedRunProjection, DecisionEditRequest, DecisionEnrollmentBridge, Options, Orchestrator,
    Port, PortError, PortResult, Ports, RunDocumentHandle, RunError, RunPublicationDocument,
    RunPublicationOperation, RunPublicationStart, RunPublicationTicket,
};

mod inert;
mod ledger;
mod sessions;
#[cfg(test)]
mod tests;

pub use inert::{DocumentFaults, PublishedWaits};
pub use sessions::{SessionCall, SessionScript};

/// The instant the shared clock starts at (epoch milliseconds). It never
/// moves unless a test stores a new value through [`DecisionChain::clock`].
pub const FIXTURE_NOW_MS: i64 = 1_700_000_000_000;

/// Text no real objective carries, so a leak is unmistakable.
const FIXTURE_OBJECTIVE: &str = "OBJECTIVE-SENTINEL-write the quarterly summary";

/// How many owed projections [`publish_owed`] drives before it refuses.
const PUBLISH_BOUND: usize = 4;

fn fixture_uuid(n: u8) -> uuid::Uuid {
    // `00000000-0000-4000-8000-0000000000nn`: a v4-shaped id, stable per `n`.
    uuid::Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0000 | u128::from(n))
}

fn fixture_session(n: u8) -> SessionId {
    fixture_uuid(0xc0 + n)
        .hyphenated()
        .to_string()
        .parse()
        .expect("a fixture session id")
}

fn fixture_turn(n: u8) -> AgentTurnId {
    fixture_uuid(0xb0 + n)
        .hyphenated()
        .to_string()
        .parse()
        .expect("a fixture turn id")
}

// --------------------------------------------------------------- fixtures

/// An interactive (non-Routine) Bot submission for run `n`, valid as
/// `bot_submit` requires: a person's request on device `…e1` in Bot folder
/// `C:/fixture/bot-{n}`, no context items, captured policy over one
/// workspace, an AI attempt id, config and policy revision 1, and the input
/// text `task` keyed by the run's own id. Pure data; nothing is written.
pub fn bot_submission(n: u8) -> BotSubmission {
    let workspace = WorkspaceId::from_uuid(fixture_uuid(0xa0));
    let request = DelegationRequest {
        run: RunId::from_uuid(fixture_uuid(n)),
        requested_by: ActorRef::Person {
            profile: ProfileId::from_uuid(fixture_uuid(0xe0)),
        },
        objective: Objective::new(FIXTURE_OBJECTIVE).expect("a fixture objective"),
        agent: SessionProfileKey::BuiltIn("codex".parse().expect("a built-in profile")),
        requested_model: None,
        scope: WorkspaceScope::BotFolder {
            folder: BotWorkingFolder::new(format!("C:/fixture/bot-{n}"))
                .expect("a fixture Bot folder"),
        },
        context: Vec::new(),
        grants: BTreeSet::from([workspace]),
        executing_device: DeviceId::from_uuid(fixture_uuid(0xe1)),
        origin: Default::default(),
    };
    BotSubmission {
        provenance: BotProvenance {
            routine: None,
            owner: BotOwner {
                user_scope: serde_json::from_value(serde_json::json!(fixture_uuid(0xf0)))
                    .expect("a fixture user scope"),
                device: request.executing_device,
            },
            bot: BotId::from_uuid(fixture_uuid(0xe0)),
            submission: BotRequestId::from_uuid(fixture_uuid(n)),
            chat_id: fixture_uuid(0xe1),
            ai_attempt_id: Some(fixture_uuid(0xe2)),
            assistant_message_id: Some(fixture_uuid(0xe3)),
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        },
        workspace,
        request,
        input: PendingInput {
            idempotency_key: fixture_uuid(n)
                .to_string()
                .parse()
                .expect("a fixture input key"),
            text: "task".into(),
            attachment_ids: vec![],
            references: vec![],
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec![],
            model: None,
        },
    }
}

/// The submission's global policy barrier at `revision`: its captured
/// policy, effects allowed, keyed by a request id stable per revision. Pure
/// data; nothing is written.
pub fn bot_policy(submission: &BotSubmission, revision: u64) -> BotPolicyBarrier {
    BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: submission.provenance.owner,
            bot: submission.provenance.bot,
            request: BotRequestId::from_uuid(fixture_uuid(89 + revision as u8)).into(),
        },
        config_revision: revision,
        policy_revision: submission.provenance.policy_revision,
        policy: submission.policy.clone(),
        effects_allowed: true,
    }
}

/// The submission's unpaused admission barrier at its config revision. Pure
/// data; nothing is written.
pub fn admission_barrier(submission: &BotSubmission) -> BotAdmissionBarrier {
    BotAdmissionBarrier {
        owner: submission.provenance.owner,
        bot: submission.provenance.bot,
        request: BotRequestId::from_uuid(fixture_uuid(0xef)),
        epoch: submission.provenance.config_revision,
        paused: false,
    }
}

/// Acknowledge `barrier` and `policy` for this process's boot through the
/// public boot handshake (`bot_hello` probe, then `synchronize_boot_bot`).
///
/// Ledger after: the barrier and the policy barrier are installed and the
/// pair is the Bot's current boot acknowledgment. Memory after: the Bot is
/// Ready for this boot. Panics if the handshake refuses or is not Ready.
pub fn replay_boot_pair(
    orchestrator: &mut Orchestrator,
    barrier: BotAdmissionBarrier,
    policy: BotPolicyBarrier,
) {
    let device = barrier.owner.device;
    let boot = orchestrator
        .bot_hello(barrier.owner, BotBootScan::Probe {}, device)
        .expect("bot_hello probe")
        .boot;
    let receipt = orchestrator
        .synchronize_boot_bot(boot, barrier, policy, device)
        .expect("synchronize_boot_bot");
    assert_eq!(receipt.state, BotBootState::Ready);
}

/// [`replay_boot_pair`] with the submission's own [`admission_barrier`] and
/// its [`bot_policy`] at its config revision.
pub fn replay_boot(orchestrator: &mut Orchestrator, submission: &BotSubmission) {
    replay_boot_pair(
        orchestrator,
        admission_barrier(submission),
        bot_policy(submission, submission.provenance.config_revision),
    );
}

/// The Koru edit the fixture decision protects: an `EditDocument` on a fresh
/// target with a real semantic version vector (consume compares it
/// semantically) and the UTF-8 payload `PRIVATE-EDIT`. Pure data; its
/// `Debug` is redacted.
pub fn edit_request() -> DecisionEditRequest {
    DecisionEditRequest {
        target: DocumentId::new(),
        action: DecisionEditAction::EditDocument,
        operation_id: uuid::Uuid::new_v4(),
        edit_fingerprint: [3; 32],
        expected_revision: vec![2, 1, 2, 2, 4],
        payload: b"PRIVATE-EDIT".to_vec(),
    }
}

// ----------------------------------------------------------- the builder

/// An orchestrator holding a prepared Koru edit decision in memory, the
/// scripted session host it talks to, and the identities a test needs.
///
/// Move [`Self::orchestrator`] wherever the composition under test needs it
/// (a host actor, say); [`Self::sessions`] and [`Self::clock`] stay readable
/// and [`Self::prepared`] finishes the story against whichever orchestrator
/// the test can reach.
pub struct DecisionChain {
    pub orchestrator: Orchestrator,
    pub sessions: Arc<Mutex<SessionScript>>,
    /// The one instant both the orchestrator and the ledger read.
    pub clock: Arc<AtomicI64>,
    /// The in-memory documents' fault script and preparation count. Nothing
    /// is refused unless a test asks.
    pub documents: DocumentFaults,
    /// The decision waits published for each Run's tool classification
    /// (O147), as the host's projection receives them.
    pub waits: PublishedWaits,
    pub prepared: PreparedDecision,
}

/// The identities of the prepared decision, and the helpers that answer it
/// and observe its resume. Every helper takes the orchestrator explicitly and
/// reads or writes only through public methods.
#[derive(Clone)]
pub struct PreparedDecision {
    /// The admitted submission ([`bot_submission`]`(1)`).
    pub submission: BotSubmission,
    pub owner: BotOwner,
    pub run: RunId,
    /// The origin consumer's execution fence, with its acknowledged
    /// InitialSend turn.
    pub fence: BotExecutionFence,
    pub decision: DecisionId,
    /// This orchestrator's delegate boot; the decision was prepared under it.
    pub boot: DelegateBootId,
    /// The fresh global policy proof the decision was prepared with.
    pub proof: FreshBotPolicyProof,
    /// The binding's human (the submission's requesting person).
    pub human: ProfileId,
    /// The admission's AI attempt id, which an Enrolled reply carries.
    pub ai_attempt: uuid::Uuid,
    /// The scripted host every attempt is bound on.
    pub host: HostInstanceId,
    /// The origin attempt's creation key.
    pub origin_creation: IdempotencyKey,
}

impl std::fmt::Debug for PreparedDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Identities only: the submission carries the task text.
        f.debug_struct("PreparedDecision")
            .field("run", &self.run)
            .field("decision", &self.decision)
            .field("attempt", &self.fence.attempt)
            .field("generation", &self.fence.generation)
            .field("session", &self.fence.session)
            .field("turn", &self.fence.turn)
            .field("host", &self.host)
            .field("origin_creation", &self.origin_creation)
            .finish_non_exhaustive()
    }
}

/// The successor the ledger reserved for the decision, read from its open
/// wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuccessorKeys {
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    /// The bound session; `None` until the creation's positive answer bound it.
    pub session: Option<SessionId>,
    /// The one creation key the successor may be created under.
    pub creation: IdempotencyKey,
    /// The key the continuation input is sent under.
    pub input: IdempotencyKey,
}

/// Build a Koru edit decision waiting for its person, through public
/// orchestrator methods only. Panics if any step refuses (a fixture defect,
/// not a product outcome).
///
/// Steps: `bot_submit` of [`bot_submission`]`(1)`, [`replay_boot`],
/// `bot_dispatch`, `bot_reconcile` (for the fence), `bot_observe`,
/// `bot_begin_turn` with the submission's input, `synchronize_native_policy`
/// with [`bot_policy`]`(…, 1)`, `bot_prepare_edit_decision` with
/// [`edit_request`] under `delegate_boot()`, then [`publish_owed`].
///
/// **Ledger after:** the intent is satisfied with its Run document; the
/// admission is Executing; the boot pair (admission barrier epoch 1,
/// unpaused; policy barrier revision 1, effects allowed) is acknowledged for
/// this boot and the native policy revision 1 is installed and current; the
/// origin attempt (first generation) is Running, not retired, bound to
/// `{origin_creation, fence.session}` on `host`; the InitialSend is
/// acknowledged with `fence.turn`; the Koru edit decision is Pending/Holding
/// with its wait Fenced (no enrollment request, no resume), and its initial
/// Control projection is Applied with nothing else owed. The wait is the one
/// open decision wait `bot_decision_resume_runs` offers.
///
/// **Memory after:** the Run's document is attached through the staging
/// handle and its publication is owned here (`run_publication_pending(run)`
/// is true; no ticket is out); the attempt is the origin, on `fence.session`,
/// observed, holding the controller, with `fence.turn` as its turn; the
/// origin holds the Bot folder's claim. No event stream is pumped.
///
/// **Session script after:** one creation (`origin_creation` → the origin
/// session, live and attached), its subscribe and observe, one input under
/// the submission's key acknowledged as `fence.turn`. Nothing else is
/// scripted: retirement, the successor creation and the continuation turn
/// must be queued ([`PreparedDecision::script_resume`]) before the driver
/// can stop the origin or start a successor.
pub fn decision_chain() -> DecisionChain {
    chain(false)
}

/// [`decision_chain`] for a **Routine Run**: the same prepared Koru edit
/// decision, on a Run a Bot Routine's occurrence started, bound to the
/// Routine and to the grant it runs under (P4.3 section 4.10).
///
/// The Run is admitted the way a Routine occurrence is, through the ledger's
/// own public owners rather than `bot_submit`: the Bot's admission barrier,
/// the Routine's route, the global policy and the Routine's grant (revision
/// 1, the same page access as the Bot's: no selected page) are installed; the
/// occurrence is claimed with a claim authority minted from a live Schedule
/// document (`claim_bot_routine`, as the scheduler does) and its link is
/// acknowledged Linked (as the AI bridge does). No row is written directly.
/// From there every step is [`decision_chain`]'s, plus
/// `synchronize_routine_policy` with the grant. [`PreparedDecision::proof`]
/// carries the Routine's barrier, and [`PreparedDecision::submission`] its
/// Routine provenance.
pub fn routine_decision_chain() -> DecisionChain {
    chain(true)
}

/// Claim one occurrence of a Bot Routine for `submission` (made a Routine
/// submission here) and acknowledge its link. Returns the Routine's grant.
fn admit_routine(store: &mut RunsDb, submission: &mut BotSubmission) -> RoutinePolicyBarrier {
    use koru_delegate_core::{
        RoutineClaimObservation, RoutineDefinitionIdentity, RoutineDigest32, RoutineExecutionIds,
        RoutineLinkAcknowledgement, RoutineLinkOutcome, RoutinePrepared, RoutineProvenance,
        RoutineRouteIdentity, RoutineSlot, RunOrigin, ScheduleId,
    };
    use koru_delegate_document::{ScheduleClaimPlan, ScheduleDocument};
    use koru_delegate_schedule::{
        DelegationTemplate, RoutineAssociation, ScheduleDefinition, ScheduleHeader,
        ScheduleLifecycle, SchedulePauseReason, routine_definition_digest,
    };
    use koru_delegate_sqlite::{BotLedger, NativeResultLedger};
    use koru_recurrence::{CronExpression, RecurrenceRule};

    // Just due on the fixture clock, as a claimed occurrence is.
    const OCCURRENCE_MS: i64 = FIXTURE_NOW_MS - 100;
    submission.input.text = submission.request.objective.as_str().to_owned();
    // A Routine occurrence's execution ids are pairwise distinct; the
    // interactive fixture reuses the run's id for its submission.
    submission.provenance.submission = BotRequestId::from_uuid(fixture_uuid(0x53));
    let owner = submission.provenance.owner;
    let bot = submission.provenance.bot;
    let association = RoutineAssociation::new(
        owner,
        bot,
        submission.workspace,
        BotRequestId::from_uuid(fixture_uuid(0x51)),
    )
    .expect("a fixture Routine association");
    let schedule = ScheduleId::from_uuid(fixture_uuid(0x50));
    let header = ScheduleHeader {
        schedule,
        created_at_ms: 0,
        initial_owner: owner.device,
        definition: ScheduleDefinition {
            revision: 1,
            effective_at_ms: 0,
            template: DelegationTemplate {
                requested_by: submission.request.requested_by.clone(),
                objective: submission.request.objective.clone(),
                agent: submission.request.agent.clone(),
                requested_model: None,
                scope: WorkspaceScope::Temporary,
                grants: Default::default(),
                context: vec![],
            },
            recurrence: RecurrenceRule::floating(
                CronExpression::parse("* * * * *").expect("a fixture recurrence"),
            ),
            lifecycle: ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            },
            routine: Some(association.clone()),
        },
    };
    let document = ScheduleDocument::create(&header).expect("a fixture Schedule document");
    let mut definition = header.definition;
    definition.revision = 2;
    definition.effective_at_ms = 1;
    definition.lifecycle = ScheduleLifecycle::Active;
    document
        .update_definition(owner.device, &definition)
        .expect("enable the fixture Routine");
    let identity = RoutineDefinitionIdentity {
        revision: 2,
        digest: routine_definition_digest(&definition, submission.workspace, None)
            .expect("a fixture definition digest"),
        owner_handoff: None,
    };
    let ids = RoutineExecutionIds {
        submission: submission.provenance.submission,
        run: submission.request.run,
        chat: submission.provenance.chat_id,
        attempt: submission
            .provenance
            .ai_attempt_id
            .expect("the fixture admission has an AI attempt"),
        user_message: fixture_uuid(0xe4),
        assistant_message: submission
            .provenance
            .assistant_message_id
            .expect("the fixture admission has an assistant message"),
    };
    submission.provenance.routine = Some(RoutineProvenance {
        schedule,
        occurrence_ms: OCCURRENCE_MS,
        definition: identity,
        grant_revision: 1,
    });
    submission.request.origin = RunOrigin::Scheduled {
        schedule,
        occurrence_ms: OCCURRENCE_MS,
        revision: 2,
    };
    let prepared = RoutinePrepared {
        slot: RoutineSlot {
            owner,
            bot,
            schedule,
            occurrence_ms: OCCURRENCE_MS,
        },
        definition: identity,
        ids,
        semantic_hash: RoutineDigest32::from_bytes([7; 32]),
        preparation_hash: RoutineDigest32::from_bytes([8; 32]),
        submission_version: 2,
        submission: submission.clone(),
    };
    assert!(prepared.valid(), "a valid fixture Routine preparation");
    let global = bot_policy(submission, submission.provenance.config_revision);
    let grant = RoutinePolicyBarrier {
        owner,
        bot,
        schedule,
        request: BotRequestId::from_uuid(fixture_uuid(0x52)).into(),
        grant_revision: 1,
        policy: submission.policy.clone(),
        effects_allowed: true,
    };
    let barrier = admission_barrier(submission);
    store
        .apply_bot_barrier(&barrier)
        .expect("apply the admission barrier");
    store
        .register_routine_route(&RoutineRouteIdentity {
            owner,
            bot,
            schedule,
            workspace: association.home,
            creation_request: association.creation_request,
            seed_hash: RoutineDigest32::from_bytes([9; 32]),
            created_at_ms: 0,
        })
        .expect("register the Routine route");
    store
        .install_native_policy(&global)
        .expect("install the global policy");
    store
        .install_routine_policy(&grant)
        .expect("install the Routine grant");
    let authority = document
        .claim_authority(
            submission.workspace,
            owner.device,
            ScheduleClaimPlan::BotRoutine {
                prepared: prepared.clone(),
                representative_miss: None,
            },
        )
        .expect("mint the claim authority")
        .expect("the occurrence may be claimed");
    let proof = FreshBotPolicyProof {
        global,
        routine: Some(grant.clone()),
    };
    let (outcome, _) = crate::RoutineLedger::claim_bot_routine(store, authority, &barrier, &proof)
        .expect("claim the Routine occurrence");
    let RoutineClaimObservation::Claimed { record } = outcome else {
        panic!("the fixture occurrence is claimed: {outcome:?}");
    };
    store
        .acknowledge_routine_link(&RoutineLinkAcknowledgement {
            claim: record.claim,
            outcome: RoutineLinkOutcome::Linked {},
        })
        .expect("acknowledge the Routine link");
    grant
}

fn chain(routine: bool) -> DecisionChain {
    let clock = Arc::new(AtomicI64::new(FIXTURE_NOW_MS));
    let host = HostInstanceId::from_uuid(fixture_uuid(0xd1));
    let sessions = Arc::new(Mutex::new(SessionScript::new(host)));
    let mut store = RunsDb::open_with_clock(
        Path::new(":memory:"),
        Box::new(inert::SharedClock(Arc::clone(&clock))),
    )
    .expect("open the in-memory ledger");
    let mut submission = bot_submission(1);
    let grant = routine.then(|| admit_routine(&mut store, &mut submission));
    let documents = inert::MemoryRunDocuments::new();
    let faults = documents.faults();
    let waits = PublishedWaits::default();
    let ports = Ports {
        store: Box::new(ledger::UncountedLedger(store)),
        documents: Box::new(documents),
        sessions: Box::new(sessions::ScriptedSessions {
            script: Arc::clone(&sessions),
        }),
        capture: Box::new(inert::InertCapture),
        events: Box::new(inert::InertEvents),
        preamble: Box::new(inert::FixedPreamble),
        roots: Box::new(inert::FixtureRoots),
        mcp: Box::new(inert::RecordingMcp::new(waits.clone())),
        registrations: Box::new(inert::NoRegistrations),
        clock: Box::new(inert::SharedClock(Arc::clone(&clock))),
        sleep: Box::new(inert::NoSleep),
    };
    let mut orchestrator = Orchestrator::new(
        ports,
        Options {
            reask_attempts: 3,
            ..Options::default()
        },
    );

    let owner = submission.provenance.owner;
    let device = owner.device;
    let run = submission.request.run;
    {
        let mut script = sessions.lock().expect("the session script is not poisoned");
        script.set_delegation(true);
        script.script_creation(fixture_session(1));
        script.script_turn(fixture_turn(1));
    }

    if grant.is_none() {
        // A Routine occurrence was admitted by its claim instead.
        orchestrator
            .bot_submit(submission.clone(), device)
            .expect("bot_submit");
    }
    replay_boot(&mut orchestrator, &submission);
    if let Some(grant) = &grant {
        orchestrator
            .synchronize_routine_policy(grant.clone(), device)
            .expect("synchronize_routine_policy");
    }
    let run_key = BotRunKey {
        owner,
        bot: submission.provenance.bot,
        run,
    };
    orchestrator
        .bot_dispatch(&run_key, device)
        .expect("bot_dispatch")
        .expect("the admission dispatches");
    let submission_key = BotSubmissionKey {
        owner,
        bot: submission.provenance.bot,
        request: submission.provenance.submission,
    };
    let mut fence = orchestrator
        .bot_reconcile(&submission_key, device)
        .expect("bot_reconcile")
        .expect("the admission exists")
        .fence
        .expect("the dispatched attempt has a fence");
    orchestrator
        .bot_observe(&fence, device, None)
        .expect("bot_observe");
    fence.turn = Some(
        orchestrator
            .bot_begin_turn(&fence, device, submission.input.clone())
            .expect("bot_begin_turn"),
    );

    let policy = bot_policy(&submission, 1);
    orchestrator
        .synchronize_native_policy(policy.clone(), device)
        .expect("synchronize_native_policy");
    let proof = FreshBotPolicyProof {
        global: policy,
        routine: grant,
    };
    let boot = orchestrator.delegate_boot();
    let decision = orchestrator
        .bot_prepare_edit_decision(&fence, device, &edit_request(), &proof, boot)
        .expect("bot_prepare_edit_decision")
        .receipt
        .decision;
    publish_owed(&mut orchestrator, run).expect("publish the initial projection");

    let origin = orchestrator
        .ports_mut()
        .store
        .current_attempt(run)
        .expect("read the origin attempt")
        .expect("the origin attempt exists");
    let origin_creation = origin
        .provider_session
        .expect("the origin attempt is bound")
        .idempotency;
    let ActorRef::Person { profile: human } = submission.request.requested_by else {
        panic!("the fixture submission is a person's request");
    };
    let ai_attempt = submission
        .provenance
        .ai_attempt_id
        .expect("the fixture admission has an AI attempt");
    let prepared = PreparedDecision {
        owner,
        run,
        fence,
        decision,
        boot,
        proof,
        human,
        ai_attempt,
        host,
        origin_creation,
        submission,
    };
    DecisionChain {
        orchestrator,
        sessions,
        clock,
        documents: faults,
        waits,
        prepared,
    }
}

/// Drive every owed decision projection of `run` through the single physical
/// ticket on this thread, as the host's fixed worker would: select, prepare
/// on the handle, record the preparation, apply, return the handle, then
/// acknowledge. Returns how many were published (0 when nothing is owed).
///
/// Ledger after: each owed projection Applied, in order. Memory after: the
/// handle is back and attached, publication owned here; no ticket is out.
/// A refusal is returned after the handle has been returned; a source
/// observation (no SQL row) is refused, not performed.
pub fn publish_owed(orchestrator: &mut Orchestrator, run: RunId) -> Result<usize, RunError> {
    for published in 0..PUBLISH_BOUND {
        let Some(start) = orchestrator.next_run_publication(run)? else {
            return Ok(published);
        };
        let RunPublicationStart {
            ticket,
            document,
            operation,
        } = start;
        let mut handle = match document {
            RunPublicationDocument::Attached(handle) => handle,
            RunPublicationDocument::Cold(job) => match job.open() {
                Ok(handle) => handle,
                Err(error) => {
                    // The worker's open failed: reopen is owed, as after a loss.
                    let _ = orchestrator.lost_run_publication(ticket);
                    return Err(error.into());
                }
            },
        };
        let applied = publish_one(orchestrator, ticket, &mut *handle, operation);
        if orchestrator.return_run_publication(ticket, handle).is_err() {
            return Err(PortError::new(
                Port::Documents,
                "publication_return_refused",
                "the orchestrator did not take its handle back",
            )
            .into());
        }
        orchestrator.acknowledge_run_projection(&applied?)?;
    }
    Err(PortError::new(
        Port::Documents,
        "publication_unbounded",
        "owed projections did not drain within the bound",
    )
    .into())
}

fn publish_one(
    orchestrator: &mut Orchestrator,
    ticket: RunPublicationTicket,
    handle: &mut dyn RunDocumentHandle,
    operation: RunPublicationOperation,
) -> Result<AppliedRunProjection, RunError> {
    let prepared = match operation {
        RunPublicationOperation::Prepare(intent) => {
            let candidate = handle.prepare_decision_projection(&intent)?;
            match orchestrator.record_run_preparation(ticket, &candidate) {
                Ok(prepared) => prepared,
                Err(error) => {
                    // An unrecorded candidate is discarded before the handle
                    // goes back, as the host's worker does.
                    let _ = handle.discard_prepared_projection(candidate.identity.transition);
                    return Err(error);
                }
            }
        }
        RunPublicationOperation::Apply(prepared) => prepared,
        RunPublicationOperation::ObserveSource(_) => {
            return Err(PortError::new(
                Port::Documents,
                "observation_unsupported",
                "the decision-chain builder publishes projections only",
            )
            .into());
        }
    };
    Ok(handle.apply_prepared_projection(&prepared)?)
}

fn bot_ledger(
    orchestrator: &mut Orchestrator,
) -> Result<&mut dyn koru_delegate_sqlite::BotLedger, RunError> {
    orchestrator.ports_mut().store.bots().ok_or_else(|| {
        PortError::new(
            Port::Store,
            "bot_execution_unavailable",
            "the store has no Bot ledger",
        )
        .into()
    })
}

impl PreparedDecision {
    /// The AI worker's Enrolled reply for exactly `request`, carrying the
    /// admission's AI attempt id, as the AI side returns it.
    pub fn enrolled_reply(&self, request: &EnrollBotWaitCleanup) -> BotWaitEnrollmentReply {
        BotWaitEnrollmentReply::Enrolled {
            receipt_digest: request.key.receipt_digest,
            key: request.key.clone(),
            ai_attempt: self.ai_attempt,
        }
    }

    /// A bridge that answers every request with [`Self::enrolled_reply`]
    /// and counts them. A host test uses its own mailbox instead.
    pub fn enrollment_bridge(&self) -> ScriptedEnrollment {
        ScriptedEnrollment {
            ai_attempt: self.ai_attempt,
            requests: 0,
        }
    }

    /// Queue on `script` everything the resume chain asks of the session
    /// host, in the order it asks: the origin creation's retirement answered
    /// `NoEvidence` (the host holds nothing for it once terminated), the
    /// successor's creation as `successor`, and the continuation input's
    /// positive acknowledgment as `turn`. Writes nothing to the ledger.
    pub fn script_resume(
        &self,
        script: &mut SessionScript,
        successor: SessionId,
        turn: AgentTurnId,
    ) {
        script.script_retirement(self.origin_creation, CreationLookup::NoEvidence);
        script.script_creation(successor);
        script.script_turn(turn);
    }

    /// The person's AllowOnce at the decision's current revision and binding
    /// digest, read back from the ledger (a decision is answerable only after
    /// its initial projection applied, at the revision it then carries). The
    /// authority digest stands for the human route's off-writer proof.
    /// Read-only.
    pub fn allow_once_answer(
        &self,
        orchestrator: &mut Orchestrator,
    ) -> Result<AnswerDecision, RunError> {
        let record = bot_ledger(orchestrator)?
            .bot_decision(self.owner, self.run, self.decision)?
            .ok_or_else(|| PortError::new(Port::Store, "bot_fenced", "no such decision"))?;
        Ok(AnswerDecision {
            owner: self.owner,
            run: self.run,
            decision: self.decision,
            expected_revision: record.receipt.state.revision,
            binding_digest: record.receipt.binding_digest,
            request: DecisionAnswerId::mint(),
            human: self.human,
            choice: DecisionAnswerChoice::AllowOnce {},
            authority_digest: DecisionDigest::from_bytes([9; 32]),
        })
    }

    /// Answer [`Self::allow_once_answer`] through `bot_answer_decision` as
    /// the owner's device, with the preparation's proof and boot.
    ///
    /// Ledger after (from a held decision: origin stopped): decision
    /// Granted/Held, wait Retired; the driver's next step reserves the
    /// successor. Answered earlier, the ledger's own rules decide.
    pub fn allow_once(&self, orchestrator: &mut Orchestrator) -> Result<DecisionRecord, RunError> {
        let input = self.allow_once_answer(orchestrator)?;
        orchestrator.bot_answer_decision(&input, self.owner.device, &self.proof, self.boot)
    }

    /// The Run's open decision wait as the ledger holds it now, if any.
    pub fn wait(
        &self,
        orchestrator: &mut Orchestrator,
    ) -> Result<Option<DecisionWaitExecution>, RunError> {
        Ok(bot_ledger(orchestrator)?.decision_wait_execution(self.owner, self.run)?)
    }

    /// The reserved successor's identity and keys, once reserved.
    pub fn successor_keys(
        &self,
        orchestrator: &mut Orchestrator,
    ) -> Result<Option<SuccessorKeys>, RunError> {
        Ok(self
            .wait(orchestrator)?
            .and_then(|execution| execution.resume)
            .map(|resume| {
                let (attempt, generation, session, creation) = resume.consumer_identity();
                SuccessorKeys {
                    attempt,
                    generation,
                    session,
                    creation,
                    input: resume.input_key,
                }
            }))
    }

    /// The durably acknowledged continuation (successor attempt, generation,
    /// session and accepted turn), once the ledger recorded the host's
    /// positive acknowledgment; `None` before.
    pub fn acknowledged_continuation(
        &self,
        orchestrator: &mut Orchestrator,
    ) -> Result<Option<DecisionAcknowledgedContinuation>, RunError> {
        Ok(bot_ledger(orchestrator)?.decision_acknowledged_continuation(self.owner, self.run)?)
    }
}

/// An enrollment bridge answering Enrolled for every request (see
/// [`PreparedDecision::enrollment_bridge`]).
#[derive(Debug)]
pub struct ScriptedEnrollment {
    ai_attempt: uuid::Uuid,
    requests: usize,
}

impl ScriptedEnrollment {
    /// How many enrollment requests reached this bridge.
    pub fn requests(&self) -> usize {
        self.requests
    }
}

impl DecisionEnrollmentBridge for ScriptedEnrollment {
    fn enroll(&mut self, request: &EnrollBotWaitCleanup) -> PortResult<BotWaitEnrollmentReply> {
        self.requests += 1;
        Ok(BotWaitEnrollmentReply::Enrolled {
            receipt_digest: request.key.receipt_digest,
            key: request.key.clone(),
            ai_attempt: self.ai_attempt,
        })
    }
}
