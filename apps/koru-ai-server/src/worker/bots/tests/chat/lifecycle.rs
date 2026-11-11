// SPDX-License-Identifier: MPL-2.0
//! Archive and Unarchive through the full worker (P1.7 S4, S4b, and the
//! reversal of O27/O37), with the fake delegate owner the conversation tests
//! use, extended with what Archive needs: the admission barrier, the policy
//! barrier, and the per-Bot unsettled-work read (private IPC 12) over the
//! conversation task and any tasks started through the public API, which
//! have no conversation. Archive stops work as the person's Stop, so a
//! session a person holds is ended, never waited for.
//!
//! The fake owner applies the delegate's own rules wherever lifecycle
//! depends on them, so a test cannot pass on a permission the real owner
//! never gives: an admission barrier's epoch never goes back
//! (`apply_bot_barrier`); a Submit of a request already admitted answers
//! that record, and only a new one under a newer barrier epoch is refused
//! `bot_stale` and under a paused barrier `bot_fenced` (`admit_bot`, which
//! looks the request up before the barrier); the
//! unsettled-work read returns the live admission and at most `limit` queued
//! ones with `more`; held native effects are paged by cursor with `next`;
//! and a drain is refused unless the delegate would allow it
//! (`check_native_effect_drain`).
use super::*;
use koru_ai_core::bots::replay::BotReplayStop;
use koru_ai_wire::bot_lifecycle_codes::{ARCHIVED, BOT_TRASHED, PENDING, TRASHED};
use std::sync::atomic::AtomicUsize;
mod answered;
mod context_window;
mod defaults;
mod history;
mod lanes;
mod listing;
mod owed;
mod pending;
mod probe;
mod reverse;
mod sibling;
mod task_stop;
mod trash;
mod unadmitted;

/// The delegate's refusal of a command, as its host answers it.
fn refused(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The Bot operation cannot be admitted.", false)
}

/// A held effect's position in the delegate's recovery order
/// (`created_at_ms`, Run, operation), as its owner query orders it.
fn effect_order(cursor: &d::NativeRecoveryCursor) -> (i64, String, Uuid) {
    (
        cursor.created_at_ms,
        cursor.run.to_storage(),
        cursor.operation_id,
    )
}

/// A task the fake owner runs that no conversation of this writer drives.
struct ApiTask {
    /// Whether the owner admitted it. One prepared in AI whose submission
    /// never reached the owner is not admitted: Reconcile answers Missing,
    /// and Submit admits it only as the delegate's rules allow.
    admitted: bool,
    view: d::BotAdmissionView,
    /// Stops the owner still answers as unconfirmed before it settles.
    pending: usize,
    /// A person takes this task's session the moment the first Stop reaches
    /// it. The owner's Stop is lease-free, so it ends the session anyway.
    take_on_stop: bool,
    /// The session owner reports a person holding this session.
    held: bool,
    /// The owner's dispatcher would start this queued Run when it next
    /// reserves it (`reserve_bot`): under a paused barrier it answers Paused
    /// and the Run stays queued with no fence; otherwise it starts executing.
    /// The fake applies it whenever it is next asked about the Bot's work.
    dispatch: bool,
    /// The dispatcher started it: it was reserved while no paused barrier
    /// held the Bot.
    started: bool,
    /// The owner admits this task's Submit, then fails to complete its Run
    /// intent and answers `bot_context_unavailable` (`bot_submit`): admitted
    /// although the reply is a refusal.
    context_unavailable: bool,
}

#[derive(Default)]
struct Archiving {
    native: Native,
    /// The session owner reports a person holding the conversation task's
    /// session (or asking for it).
    holding: AtomicBool,
    /// The conversation task's Stops answer unconfirmed and never reach the
    /// owner, so the Archive stays unfinished.
    stop_pending: AtomicBool,
    /// The next policy installs answer Pending (effects still in flight).
    policy_pending: Mutex<usize>,
    /// Every admission barrier offered, applied or refused.
    barriers: Mutex<Vec<d::BotAdmissionBarrier>>,
    /// The barrier the owner holds for each Bot: the newest it applied.
    barrier: Mutex<std::collections::HashMap<d::BotId, d::BotAdmissionBarrier>>,
    /// Every Submit that reached the owner, by Run, admitted or refused. A
    /// lifecycle operation never submits a captured subject's work.
    submits: Mutex<Vec<d::RunId>>,
    /// Held-effect pages read.
    effect_pages: AtomicUsize,
    policies: Mutex<Vec<d::BotPolicyBarrier>>,
    api: Mutex<Vec<ApiTask>>,
    /// Unsettled-work reads.
    reads: AtomicUsize,
    /// Every Stop, by Run, whoever sent it.
    stops: Mutex<Vec<d::RunId>>,
    /// Every Interrupt, by Run, whoever sent it.
    interrupts: Mutex<Vec<d::RunId>>,
    /// Native effects the owner still holds for the Bot (one page).
    effects: Mutex<Vec<d::NativePendingEffect>>,
    /// Delegate-side Run `Trash`/`Restore` commands. Lifecycle sends none in
    /// this version (O41); each test asserts this stays zero.
    run_lifecycle: AtomicUsize,
    /// Calls that reached the Store through the authority's capture (see
    /// [`StoreSpy`]); lifecycle makes none.
    store_calls: AtomicUsize,
    /// The owner is unreachable: every command fails as the host does when
    /// the delegate is down, retryable, and changes nothing.
    unavailable: AtomicBool,
    /// The owner refuses every admission barrier (`bot_conflict`), so no
    /// refusal fence can be acknowledged.
    refuse_barriers: AtomicBool,
    /// Every command offered, reachable or not: when, which, and the request
    /// it names, so a test can bound how often one thing is asked.
    calls: Mutex<Vec<(Instant, &'static str, Option<BotRequestId>)>>,
    /// The one device this host serves: the first one a command names.
    device: Mutex<Option<d::DeviceId>>,
    /// The authority captures page history (as `attachments::resend`'s
    /// `Capturing` does), so a Retry the AI side prepares by itself can be
    /// prepared. Lifecycle never needs it; each capture is still counted.
    capture_history: AtomicBool,
}
/// The owner a command names, where it names one, for the device check
/// every Bot command passes at the host (`owner_device`).
fn owner_named(command: &D) -> Option<d::BotOwner> {
    match command {
        D::Submit { submission } => Some(submission.provenance.owner),
        D::Reconcile { key } => Some(key.owner),
        D::Barrier { barrier } => Some(barrier.owner),
        D::Stop { key, .. } | D::Interrupt { key, .. } => Some(key.owner),
        D::UnsettledWork { owner, .. } | D::PendingNativeEffects { owner, .. } => Some(*owner),
        _ => None,
    }
}
/// The delegate's limit on a Bot's queued admissions (`BOT_PENDING_LIMIT`).
const PENDING_LIMIT: usize = 50;
/// A command's name and the request it is about, for [`Archiving::calls`].
fn call_of(command: &D) -> (&'static str, Option<BotRequestId>) {
    match command {
        D::Submit { submission } => ("submit", Some(submission.provenance.submission)),
        D::Reconcile { key } => ("reconcile", Some(key.request)),
        D::Barrier { barrier } => ("barrier", Some(barrier.request)),
        D::Stop { .. } => ("stop", None),
        D::UnsettledWork { .. } => ("unsettled", None),
        D::SynchronizePolicy { .. } => ("policy", None),
        D::PendingNativeEffects { .. } => ("effects", None),
        _ => ("other", None),
    }
}
impl Archiving {
    /// How often `request` was asked as `what` in the window of `span`
    /// ending at `end`.
    fn calls_in(&self, what: &str, request: BotRequestId, end: Instant, span: Duration) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, name, key)| {
                *name == what && *key == Some(request) && *at <= end && end - *at <= span
            })
            .count()
    }
    /// The owner restarts: its actor drains its Bot queue at once
    /// (`wake_bots` after the restart classification), before the AI owner
    /// has contacted it, starting every queued Run its barrier does not hold
    /// paused.
    fn restart_owner(&self) {
        self.dispatch();
    }
    /// The owner's dispatcher (`reserve_bot`), run before it answers about
    /// the Bot's work: a queued Run it would start stays queued with no fence
    /// while the Bot's barrier is paused, and starts otherwise.
    fn dispatch(&self) {
        let barriers = self.barrier.lock().unwrap();
        for task in self.api.lock().unwrap().iter_mut() {
            if !task.admitted
                || !task.dispatch
                || task.view.state != d::BotAdmissionState::Queued
                || barriers.get(&task.view.key.bot).is_some_and(|b| b.paused)
            {
                continue;
            }
            task.view.state = d::BotAdmissionState::Executing;
            task.view.fence = Some(d::BotExecutionFence {
                owner: task.view.key.owner,
                bot: task.view.key.bot,
                run: task.view.run,
                attempt: d::RunAttemptId::mint(),
                generation: d::AttemptGeneration::from_raw(1).unwrap(),
                session: s::SessionId::mint(),
                turn: Some(s::AgentTurnId::mint()),
            });
            task.started = true;
        }
    }
    fn unsettled(&self, bot: d::BotId) -> Vec<d::BotUnsettledAdmission> {
        let mut admissions = Vec::new();
        {
            let state = self.native.state.lock().unwrap();
            if state
                .submission
                .as_ref()
                .is_some_and(|s| s.provenance.bot == bot)
                && let BotReply::Admission { admission } = admission(&state)
                && !matches!(
                    admission.state,
                    d::BotAdmissionState::Settled | d::BotAdmissionState::Refused
                )
            {
                admissions.push(d::BotUnsettledAdmission {
                    admission,
                    held: self.holding.load(Ordering::SeqCst),
                });
            }
        }
        for task in self.api.lock().unwrap().iter() {
            if task.admitted
                && task.view.key.bot == bot
                && !matches!(
                    task.view.state,
                    d::BotAdmissionState::Settled | d::BotAdmissionState::Refused
                )
            {
                admissions.push(d::BotUnsettledAdmission {
                    admission: task.view.clone(),
                    held: task.held,
                });
            }
        }
        admissions
    }
    /// The delegate's read of it (`unsettled_bot`): the reserved or
    /// executing admission, then at most `limit` queued ones, and whether
    /// more are queued past them.
    fn unsettled_page(&self, bot: d::BotId, limit: u16) -> d::BotUnsettledWork {
        let (mut admissions, queued): (Vec<_>, Vec<_>) = self
            .unsettled(bot)
            .into_iter()
            .partition(|a| a.admission.state != d::BotAdmissionState::Queued);
        let limit = usize::from(limit.clamp(1, 64));
        let more = queued.len() > limit;
        admissions.extend(queued.into_iter().take(limit));
        d::BotUnsettledWork { admissions, more }
    }
    /// Whether the owner's barrier for this Bot refuses the submission, and
    /// with which code (`admit_bot`): a newer epoch than its configuration
    /// is stale, a paused barrier fences it.
    fn admission_refusal(&self, submission: &d::BotSubmission) -> Option<&'static str> {
        let barrier = self.barrier.lock().unwrap();
        let current = barrier.get(&submission.provenance.bot)?;
        if submission.provenance.config_revision < current.epoch {
            Some("bot_stale")
        } else if current.paused {
            Some("bot_fenced")
        } else {
            None
        }
    }
    /// Submits of `run` that reached the owner.
    fn submits_of(&self, run: d::RunId) -> usize {
        self.submits
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == run)
            .count()
    }
    fn interrupts_of(&self, run: d::RunId) -> usize {
        self.interrupts
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == run)
            .count()
    }
    fn stops_of(&self, run: d::RunId) -> usize {
        self.stops
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == run)
            .count()
    }
    /// The conversation task's Run, once it was submitted.
    fn conversation_run(&self) -> d::RunId {
        self.native
            .state
            .lock()
            .unwrap()
            .submission
            .as_ref()
            .unwrap()
            .request
            .run
    }
}
fn unconfirmed() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new("bot_stop_unconfirmed", "fixture stop unconfirmed", true)
}
impl BotDelegatePort for Archiving {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(
            std::thread::current()
                .name()
                .unwrap_or_default()
                .starts_with("koru-ai-provider"),
            "an owner call ran on the writer"
        );
        let (name, request) = call_of(&command);
        self.calls
            .lock()
            .unwrap()
            .push((Instant::now(), name, request));
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(koru_delegate_api::Failure::new(
                "bot_unavailable",
                "fixture owner unavailable",
                true,
            ));
        }
        // `owner_device`: the host serves one device; a command naming
        // another is refused before anything is read.
        if let Some(owner) = owner_named(&command) {
            let mut device = self.device.lock().unwrap();
            if *device.get_or_insert(owner.device) != owner.device {
                return Err(refused("bot_owner_mismatch"));
            }
        }
        if matches!(
            &command,
            D::UnsettledWork { .. } | D::Reconcile { .. } | D::Stop { .. }
        ) {
            self.dispatch();
        }
        match &command {
            D::Barrier { .. } if self.refuse_barriers.load(Ordering::SeqCst) => {
                Err(refused("bot_conflict"))
            }
            D::Barrier { barrier } => {
                self.barriers.lock().unwrap().push(barrier.clone());
                // `apply_bot_barrier`: an epoch never goes back, and one
                // epoch names exactly one barrier.
                let mut held = self.barrier.lock().unwrap();
                if let Some(old) = held.get(&barrier.bot) {
                    if barrier.epoch < old.epoch {
                        return Err(refused("bot_stale"));
                    }
                    if barrier.epoch == old.epoch && old != barrier {
                        return Err(refused("bot_conflict"));
                    }
                }
                held.insert(barrier.bot, barrier.clone());
                Ok(BotReply::Barrier {
                    barrier: barrier.clone(),
                })
            }
            // Every Submit is admitted only as `admit_bot` would admit it,
            // in its order: the request first, then the Run identity, then
            // the barrier, then the queue's capacity.
            D::Submit { submission } => {
                self.submits.lock().unwrap().push(submission.request.run);
                let api = self.api.lock().unwrap();
                // `admit_bot` (`reconcile` first): a repeated Submit of a
                // request it already admitted answers the record it holds,
                // whatever its barrier says now. `bot_submit` then completes
                // the Run intent again while it is still pending, so a task
                // whose context could not be completed still answers that.
                if let Some(task) = api
                    .iter()
                    .find(|t| t.admitted && t.view.key.request == submission.provenance.submission)
                {
                    if task.context_unavailable {
                        return Err(refused("bot_context_unavailable"));
                    }
                    return Ok(BotReply::Admission {
                        admission: task.view.clone(),
                    });
                }
                if self
                    .native
                    .state
                    .lock()
                    .unwrap()
                    .submission
                    .as_ref()
                    .is_some_and(|held| {
                        held.provenance.submission == submission.provenance.submission
                    })
                {
                    drop(api);
                    // The conversation fake's own repeat arm (`admit_bot`).
                    return self.native.execute(command);
                }
                // `admit_bot`: a Run identity already held by another request
                // conflicts (`run_intents`).
                if api.iter().any(|t| {
                    t.admitted
                        && t.view.run == submission.request.run
                        && t.view.key.request != submission.provenance.submission
                }) {
                    return Err(refused("bot_conflict"));
                }
                // `admit_bot`: the barrier, then the capacity. The barrier is
                // read with the task list let go: the dispatcher takes them
                // in the other order.
                drop(api);
                if let Some(code) = self.admission_refusal(submission) {
                    return Err(refused(code));
                }
                let mut api = self.api.lock().unwrap();
                let queued = api
                    .iter()
                    .filter(|t| {
                        t.admitted
                            && t.view.key.bot == submission.provenance.bot
                            && t.view.state == d::BotAdmissionState::Queued
                            && t.view.key.request != submission.provenance.submission
                    })
                    .count();
                if queued >= PENDING_LIMIT {
                    return Err(refused("bot_capacity"));
                }
                if let Some(task) = api
                    .iter_mut()
                    .find(|t| t.view.run == submission.request.run)
                {
                    // A task started through the API, admitted by this owner.
                    task.admitted = true;
                    if task.context_unavailable {
                        // Admitted, but its Run intent could not be completed.
                        return Err(refused("bot_context_unavailable"));
                    }
                    return Ok(BotReply::Admission {
                        admission: task.view.clone(),
                    });
                }
                drop(api);
                self.native.execute(command)
            }
            D::CheckNativeEffectDrain { key, reason } => {
                // `check_native_effect_drain`: only a held effect, and only
                // once its Run has ended for the drain's reason. Nothing in
                // these tests configures native recovery, so no drain job
                // asks; this keeps the fake from allowing one if it did.
                let ended = self.api.lock().unwrap().iter().any(|t| {
                    t.view.run == key.run && t.view.state == d::BotAdmissionState::Settled
                }) || {
                    let state = self.native.state.lock().unwrap();
                    state
                        .submission
                        .as_ref()
                        .is_some_and(|s| s.request.run == key.run)
                        && state.stopped
                        && !state.stop_unconfirmed
                };
                let allowed = matches!(
                    reason,
                    d::NativeDrainReason::RunRecovery | d::NativeDrainReason::OwnerStop
                ) && ended;
                Ok(BotReply::NativeDrainEligibility {
                    target: self
                        .effects
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|e| e.key == *key && allowed)
                        .cloned(),
                })
            }
            D::SynchronizePolicy { barrier } => {
                self.policies.lock().unwrap().push(barrier.clone());
                let pending = {
                    let mut left = self.policy_pending.lock().unwrap();
                    let pending = *left > 0;
                    *left = left.saturating_sub(1);
                    pending
                };
                Ok(BotReply::PolicyBarrier {
                    receipt: d::BotPolicyBarrierReceipt {
                        barrier: barrier.clone(),
                        state: if pending {
                            d::BotPolicyBarrierState::Pending
                        } else {
                            d::BotPolicyBarrierState::Effective
                        },
                    },
                })
            }
            D::Trash { .. } | D::Restore { .. } => {
                self.run_lifecycle.fetch_add(1, Ordering::SeqCst);
                Err(koru_delegate_api::Failure::new(
                    "fixture_forbidden",
                    "lifecycle must not trash or restore a Run",
                    false,
                ))
            }
            D::PendingNativeEffects {
                bot, after, limit, ..
            } => {
                assert!(*limit >= 1 && *limit <= 64);
                self.effect_pages.fetch_add(1, Ordering::SeqCst);
                // The owner's recovery order, from just past the cursor, at
                // most `limit` a page, with the next cursor while more remain.
                let mut held: Vec<_> = self
                    .effects
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| Some(e.bot) == *bot)
                    .filter(|e| {
                        after
                            .as_ref()
                            .is_none_or(|a| effect_order(&e.cursor) > effect_order(a))
                    })
                    .cloned()
                    .collect();
                held.sort_by_key(|e| effect_order(&e.cursor));
                let more = held.len() > usize::from(*limit);
                held.truncate(usize::from(*limit));
                let next = more.then(|| held.last().unwrap().cursor.clone());
                Ok(BotReply::NativePendingEffects {
                    page: d::NativePendingEffects {
                        targets: held,
                        next,
                    },
                })
            }
            D::Reconcile { key }
                if self
                    .api
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|t| t.view.key.request == key.request) =>
            {
                let api = self.api.lock().unwrap();
                let task = api
                    .iter()
                    .find(|t| t.view.key.request == key.request)
                    .unwrap();
                Ok(if task.admitted {
                    BotReply::Admission {
                        admission: task.view.clone(),
                    }
                } else {
                    BotReply::Missing
                })
            }
            D::UnsettledWork { bot, limit, .. } => {
                assert!(*limit >= 1 && *limit <= 64);
                self.reads.fetch_add(1, Ordering::SeqCst);
                Ok(BotReply::UnsettledWork {
                    work: self.unsettled_page(*bot, *limit),
                })
            }
            D::Interrupt { key, .. } => {
                self.interrupts.lock().unwrap().push(key.run);
                // Lifecycle never interrupts; only a conversation's own
                // automatic lane does, and that is the conversation fake's.
                assert!(
                    !self
                        .api
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|t| t.view.run == key.run),
                    "lifecycle interrupted an API task"
                );
                self.native.execute(command)
            }
            D::Stop { key, fence } => {
                self.stops.lock().unwrap().push(key.run);
                let mut api = self.api.lock().unwrap();
                if let Some(task) = api.iter_mut().find(|t| t.view.run == key.run) {
                    // A Run the owner never admitted is unknown to it.
                    if !task.admitted {
                        return Err(refused("run_unknown"));
                    }
                    // Only the owner's current fence is accepted: one read
                    // before the dispatcher started the Run is refused, and
                    // the next read reports the new fence.
                    // As `bot_stop` checks it: a queued Run takes no fence,
                    // a running one its owner, Bot, Run, attempt, generation
                    // and session, never its turn.
                    let same = |a: &d::BotExecutionFence, b: &d::BotExecutionFence| {
                        (a.owner, a.bot, a.run, a.attempt, a.generation, a.session)
                            == (b.owner, b.bot, b.run, b.attempt, b.generation, b.session)
                    };
                    match (fence, &task.view.fence) {
                        (None, None) => {}
                        (Some(sent), Some(current)) if same(sent, current) => {}
                        (Some(_), None) => return Err(refused("bot_fenced")),
                        (None, Some(_)) => return Err(refused("bot_fence_required")),
                        _ => return Err(refused("bot_fenced")),
                    }
                    if task.take_on_stop {
                        // A person takes the session mid-Archive; the
                        // owner's Stop needs no lease, so it ends it anyway.
                        task.take_on_stop = false;
                        task.held = true;
                    }
                    if task.pending > 0 {
                        task.pending -= 1;
                        return Err(unconfirmed());
                    }
                    task.view.state = d::BotAdmissionState::Settled;
                    task.held = false;
                    return Ok(BotReply::Applied);
                }
                drop(api);
                if self.stop_pending.load(Ordering::SeqCst) {
                    return Err(unconfirmed());
                }
                drop(self.native.execute(command));
                Ok(BotReply::Applied)
            }
            _ => self.native.execute(command),
        }
    }
}

struct Running {
    worker: WorkerHandle,
    client: AiWorkerClient,
    returned: Receiver<Repository>,
    port: Arc<Archiving>,
    bot: Bot,
    attempt: InferenceAttemptId,
}

/// The worker's only way to the Store in these tests: the authority port.
/// Its reads (owner, workspace, page, inspect) are authority checks; its
/// `capture` reads Store history and is counted, since lifecycle never
/// needs it. No native-recovery Store client is configured, so nothing
/// else can reach a Store at all.
struct StoreSpy {
    inner: Arc<Authority>,
    port: Arc<Archiving>,
}
impl BotAuthority for StoreSpy {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn capture(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _profile: koru_ai_core::ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        self.port.store_calls.fetch_add(1, Ordering::SeqCst);
        if !self.port.capture_history.load(Ordering::SeqCst) {
            return Err(BotError::Unavailable);
        }
        Ok(pages
            .iter()
            .map(|page| d::RunContextItem::Document {
                workspace: page.workspace,
                document: page.document,
                version: d::DocumentVersionId::new(),
            })
            .collect())
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<koru_ai_core::bots::BotAuthoritySnapshot, BotError> {
        self.inner.inspect(workspaces, pages)
    }
}

fn spawn(
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
) -> (WorkerHandle, Receiver<Repository>) {
    spawn_with(repository, authority, port, None)
}

/// [`spawn`], with a Store context reader for the Sends' context jobs.
fn spawn_with(
    repository: Repository,
    authority: Arc<Authority>,
    port: Arc<Archiving>,
    store_context: Option<Arc<dyn StoreContextPort>>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    // A worker that prepares Bot commands itself (a re-send's Retry) gets
    // the test seam that answers model discovery ready, as `attachments`'
    // harness does.
    let prepares = port.capture_history.load(Ordering::SeqCst);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(Arc::new(StoreSpy {
        inner: authority,
        port: port.clone(),
    }))
    .with_bot_delegate(port);
    if let Some(store_context) = store_context {
        config = config.with_store_context(store_context);
    }
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    config.test_bot_runtime_ready = prepares;
    (AiWorker::new(config).spawn().unwrap(), returned)
}

/// A Bot whose conversation task is running natively: its turn began and
/// never ends.
fn running(holding: bool) -> Running {
    started_with(
        Native {
            hold_observation: true,
            ..Native::default()
        },
        holding,
    )
}

fn started_with(native: Native, holding: bool) -> Running {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let context = proof(authority.clone(), &bot, chat);
    let (begin_tx, begins) = sync_channel(4);
    *native.begin_reports.lock().unwrap() = Some(begin_tx);
    let port = Arc::new(Archiving {
        native,
        holding: AtomicBool::new(holding),
        ..Archiving::default()
    });
    let (worker, returned) = spawn(repository, authority, port.clone());
    let (reply, response) = sync_channel(1);
    worker
        .client()
        .send(WorkerCommand::SendTurn {
            context,
            request: request(&bot),
            reply,
        })
        .unwrap();
    let stream = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    begins.recv_timeout(Duration::from_secs(10)).unwrap();
    let client = worker.client();
    Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt: stream.accepted.attempt_id,
    }
}

fn archive_at(
    client: &AiWorkerClient,
    bot: &Bot,
    request: BotRequestId,
    expected_revision: u64,
) -> ApiResult<BotLifecycleProgress> {
    match client.bots(
        context(true),
        BotCommand::Archive {
            request_id: request,
            bot_id: bot.id,
            expected_revision,
        },
    )? {
        BotResponse::Lifecycle { operation } => Ok(operation),
        _ => panic!("lifecycle progress"),
    }
}

fn archive(client: &AiWorkerClient, bot: &Bot, request: BotRequestId) -> BotLifecycleProgress {
    archive_at(client, bot, request, bot.revision).unwrap()
}

fn read_operation(
    client: &AiWorkerClient,
    bot: BotId,
    request: BotRequestId,
) -> BotLifecycleProgress {
    // One read, never retried: a progress read is not fenced to the Bot
    // revision it was prepared at, so an operation moving the revision while
    // the read is prepared cannot refuse it.
    match client.bots(
        context(true),
        BotCommand::ReadLifecycleOperation {
            request_id: request,
            bot_id: bot,
        },
    ) {
        Ok(BotResponse::Lifecycle { operation }) => operation,
        other => panic!(
            "lifecycle read: {:?}",
            other.map(|_| ()).map_err(|error| error.code)
        ),
    }
}

/// Read the operation until it stops moving.
fn settled(client: &AiWorkerClient, bot: BotId, request: BotRequestId) -> BotLifecycleProgress {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = read_operation(client, bot, request);
        if !operation.stage.moving() {
            return operation;
        }
        assert!(Instant::now() < deadline, "still {:?}", operation.stage);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_bot(client: &AiWorkerClient, bot: BotId) -> Bot {
    let BotResponse::Bot { bot } = client
        .bots(context(true), BotCommand::Read { bot_id: bot })
        .unwrap()
    else {
        panic!("Bot")
    };
    bot
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn owner_of(bot: &Bot) -> d::BotOwner {
    d::BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

fn api_task(bot: &Bot, state: d::BotAdmissionState, pending: usize) -> ApiTask {
    let run = d::RunId::mint();
    ApiTask {
        admitted: true,
        view: d::BotAdmissionView {
            key: d::BotSubmissionKey {
                owner: owner_of(bot),
                bot: bot.id,
                request: BotRequestId::mint(),
            },
            run,
            state,
            queue_sequence: 1,
            fence: (state == d::BotAdmissionState::Executing).then(|| d::BotExecutionFence {
                owner: owner_of(bot),
                bot: bot.id,
                run,
                attempt: d::RunAttemptId::mint(),
                generation: d::AttemptGeneration::from_raw(1).unwrap(),
                session: s::SessionId::mint(),
                turn: Some(s::AgentTurnId::mint()),
            }),
        },
        pending,
        take_on_stop: false,
        held: false,
        dispatch: false,
        started: false,
        context_unavailable: false,
    }
}

/// The task's AI attempt ended as the person's Stop: Cancelled, with the
/// owner-stop reason only `cancel_bot_chat` records.
fn stopped_by_the_person(repository: &Repository, attempt: InferenceAttemptId) -> bool {
    repository.attempt(attempt).unwrap().unwrap().status == AttemptStatus::Cancelled
        && repository.bot_replay_state(attempt).unwrap().stop_reason
            == Some(BotReplayStop::OwnerUnavailable)
}

/// An Archive captured by an earlier worker straight into the journal.
fn captured_archive(repository: &mut Repository, bot: &Bot) -> BotRequestId {
    let request = BotRequestId::mint();
    repository
        .capture_bot_lifecycle(&koru_ai_sqlite::LifecycleCapture {
            operation: request,
            owner: BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            kind: BotLifecycleKind::Archive,
            chat: None,
            fingerprint: "c".repeat(64),
            expected_revision: bot.revision,
            source: None,
            members: vec![],
            now: 1,
        })
        .unwrap();
    request
}

#[test]
fn archive_stops_the_running_task_as_the_persons_stop_and_unarchive_starts_nothing() {
    let Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt,
    } = running(false);
    let archive_id = BotRequestId::mint();
    let captured = archive(&client, &bot, archive_id);
    assert_eq!(captured.kind, BotLifecycleKind::Archive);
    // An exact repeat answers the same operation.
    assert_eq!(archive(&client, &bot, archive_id).request_id, archive_id);
    let done = settled(&client, bot.id, archive_id);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(
        done.members
            .iter()
            .all(|m| m.stage == BotLifecycleMemberStage::Acknowledged)
    );
    // A changed request under a finished operation's id conflicts.
    assert_eq!(
        archive_at(&client, &bot, archive_id, bot.revision + 2)
            .unwrap_err()
            .code,
        BotError::Conflict.code()
    );
    // The existing pause barrier, then effects off at the paused revision.
    let barriers = port.barriers.lock().unwrap().clone();
    assert!(!barriers.is_empty());
    assert!(barriers.iter().all(|b| b.paused && b.bot == bot.id));
    let policies = port.policies.lock().unwrap().clone();
    assert!(!policies.is_empty());
    assert!(
        policies
            .iter()
            .all(|p| !p.effects_allowed && p.config_revision == bot.revision + 1)
    );
    // The work came from the delegate and was stopped as the person's Stop,
    // through the conversation's own Stop lane; nothing was interrupted.
    let run = port.conversation_run();
    assert!(port.reads.load(Ordering::SeqCst) >= 1);
    assert!(port.stops_of(run) >= 1);
    assert!(port.native.state.lock().unwrap().stop_calls >= 1);
    assert_eq!(port.native.state.lock().unwrap().interrupt_calls, 0);
    assert!(port.interrupts.lock().unwrap().is_empty());
    // Out of the roster, in the Archived filter, its history still readable.
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::ListLifecycle {
            workspace: bot.home_workspace,
            lifecycle: BotLifecycle::Archived,
        },
    ) else {
        panic!("archived listing")
    };
    assert_eq!(bots.iter().map(|b| b.id).collect::<Vec<_>>(), vec![bot.id]);
    assert_eq!(bots[0].lifecycle, BotLifecycle::Archived);
    let archived = read_bot(&client, bot.id);
    assert_eq!(archived.lifecycle, BotLifecycle::Archived);
    assert_eq!(archived.open_operation, None);
    let transcript = client.read_chat(context(true), bot.main_chat).unwrap();
    assert!(!transcript.messages.is_empty());
    // A change to an archived Bot's conversation answers the archived code.
    assert_eq!(
        client
            .send_turn(context(true), request(&bot))
            .map(|_| ())
            .unwrap_err()
            .code,
        ARCHIVED
    );
    // Unarchive: active again, still paused, nothing started or resumed.
    let begins = port.native.state.lock().unwrap().begin;
    let stops = port.stops.lock().unwrap().len();
    let policies = port.policies.lock().unwrap().len();
    let BotResponse::Lifecycle { operation } = client
        .bots(
            context(true),
            BotCommand::Restore {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision + 2,
                source_request_id: archive_id,
            },
        )
        .unwrap()
    else {
        panic!("restore progress")
    };
    assert_eq!(operation.stage, BotLifecycleStage::Completed);
    assert_eq!(operation.source_request_id, Some(archive_id));
    let restored = read_bot(&client, bot.id);
    assert!(restored.paused);
    assert_eq!(restored.lifecycle, BotLifecycle::Active);
    assert_eq!(restored.revision, bot.revision + 3);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(port.native.state.lock().unwrap().begin, begins);
    assert_eq!(port.stops.lock().unwrap().len(), stops);
    assert!(port.interrupts.lock().unwrap().is_empty());
    assert_eq!(port.policies.lock().unwrap().len(), policies);
    assert!(
        port.policies
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.effects_allowed),
        "no Run's native writes were re-allowed"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    // A task Archive stopped reads as stopped by the person, not interrupted.
    assert!(stopped_by_the_person(&repository, attempt));
}

#[test]
fn archive_ends_a_running_session_a_person_holds_and_completes() {
    let Running {
        worker,
        client,
        returned,
        port,
        bot,
        attempt,
    } = running(true);
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    let done = settled(&client, bot.id, request);
    // O27 reversed: not parked, not waited for. The person's Stop ended it.
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(done.refusal, None);
    assert!(port.stops_of(port.conversation_run()) >= 1);
    assert!(port.native.state.lock().unwrap().stopped);
    assert!(port.interrupts.lock().unwrap().is_empty());
    assert_eq!(read_bot(&client, bot.id).lifecycle, BotLifecycle::Archived);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(stopped_by_the_person(&repository, attempt));
}

#[test]
fn archive_ends_a_held_session_whose_task_already_ended_and_completes() {
    // The task already ended in AI as Interrupted (a replay gap), Failed (the
    // native turn failed) or natively Cancelled (a takeover "now"), while a
    // person still holds its session, so the automatic lane's Interrupt
    // stays pending. Archive is the person's Stop: it reaches the owner for
    // each of those states and ends the session (O27, O37 reversed).
    for (variant, status) in [
        ("interrupted", AttemptStatus::Interrupted),
        ("failed", AttemptStatus::Failed),
        ("cancelled", AttemptStatus::Cancelled),
    ] {
        let (interrupt_tx, interrupt_reports) = sync_channel(256);
        let native = Native {
            hold_observation: variant == "interrupted",
            replay_failure: variant == "interrupted",
            fail_first: variant == "failed",
            native_cancel: (variant == "cancelled").then_some(s::CancelReason::TakeoverBoundary),
            hold_interrupt: AtomicBool::new(true),
            interrupt_reports: Mutex::new(Some(interrupt_tx)),
            ..Native::default()
        };
        let Running {
            worker,
            client,
            returned,
            port,
            bot,
            attempt,
        } = started_with(native, true);
        // The automatic lane is already retrying its own Interrupt, pending.
        assert!(
            !interrupt_reports
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
        );
        until("the attempt ended", || {
            client
                .read_attempt(context(true), attempt)
                .is_ok_and(|a| a.attempt.status == status)
        });
        assert_eq!(port.native.state.lock().unwrap().stop_calls, 0, "{variant}");
        let request = BotRequestId::mint();
        archive(&client, &bot, request);
        let done = settled(&client, bot.id, request);
        // Not parked as `person_holding`: the person's Stop ended the session.
        assert_eq!(
            done.stage,
            BotLifecycleStage::Completed,
            "{variant} {done:?}"
        );
        assert_eq!(done.refusal, None, "{variant}");
        assert!(port.stops_of(port.conversation_run()) >= 1, "{variant}");
        assert!(
            port.native.state.lock().unwrap().stop_calls >= 1,
            "{variant}"
        );
        // The held automatic interruption never settled it; the Stop did.
        assert!(
            !port.native.state.lock().unwrap().interrupt_settled,
            "{variant}"
        );
        assert_eq!(read_bot(&client, bot.id).lifecycle, BotLifecycle::Archived);
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
        // A terminal attempt status is absorbing (O25): it keeps how it ended,
        // and the owner records the person's Stop as the Run's Cancelled.
        assert_eq!(
            repository.attempt(attempt).unwrap().unwrap().status,
            status,
            "{variant}"
        );
    }
}

#[test]
fn an_api_task_holds_archive_until_the_delegate_settles_it_and_queued_work_never_starts() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    let mut executing = api_task(&bot, d::BotAdmissionState::Executing, usize::MAX);
    // A person holds this API-started session. That is not a reason to wait.
    executing.held = true;
    let reserved = api_task(&bot, d::BotAdmissionState::Reserved, 0);
    let queued = api_task(&bot, d::BotAdmissionState::Queued, 0);
    let runs = [executing.view.run, reserved.view.run, queued.view.run];
    port.api
        .lock()
        .unwrap()
        .extend([executing, reserved, queued]);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    until("every API task stopped", || {
        runs.iter().all(|run| port.stops_of(*run) >= 1)
    });
    // The reserved and queued tasks settled at their first Stop; the
    // executing one's Stop is still unconfirmed, and Archive waits for it:
    // an acknowledgement is not proof.
    std::thread::sleep(Duration::from_millis(800));
    let waiting = read_operation(&client, bot.id, request);
    assert_eq!(waiting.stage, BotLifecycleStage::Settling, "{waiting:?}");
    assert_eq!(port.stops_of(runs[2]), 1, "queued work is not re-sent");
    assert!(
        port.stops_of(runs[0]) >= 2,
        "the unconfirmed one is re-read"
    );
    port.api.lock().unwrap()[0].pending = 0;
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert!(port.unsettled(bot.id).is_empty());
    // Nothing started (no conversation turn) and nothing was interrupted.
    assert_eq!(port.native.state.lock().unwrap().begin, 0);
    assert!(port.interrupts.lock().unwrap().is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_takeover_that_begins_mid_archive_is_ended_too() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let port = Arc::new(Archiving::default());
    // The first read finds the session free. The person takes it as the
    // first Stop arrives, which answers unconfirmed; the next round's Stop
    // ends it, held or not, because the owner's Stop needs no lease.
    let mut task = api_task(&bot, d::BotAdmissionState::Executing, 1);
    task.take_on_stop = true;
    let run = task.view.run;
    port.api.lock().unwrap().push(task);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(done.refusal, None);
    assert_eq!(port.stops_of(run), 2);
    assert_eq!(port.interrupts_of(run), 0);
    let task = &port.api.lock().unwrap()[0];
    assert_eq!(task.view.state, d::BotAdmissionState::Settled);
    assert!(!task.held, "the held session was ended");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_parked_archive_rereads_the_delegate_after_a_repeat_and_after_an_ai_restart() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let request = captured_archive(&mut repository, &bot);
    // Parked by an earlier worker while work was still running.
    assert!(
        repository
            .park_bot_lifecycle(bot.user_scope, request, BotLifecycleRefusal::WorkRunning, 2)
            .unwrap()
    );
    let port = Arc::new(Archiving::default());
    let task = api_task(&bot, d::BotAdmissionState::Executing, usize::MAX);
    let run = task.view.run;
    port.api.lock().unwrap().push(task);
    let (worker, returned) = spawn(repository, authority.clone(), port.clone());
    let client = worker.client();
    // Parked operations are not resumed at a restart, nor retried on their own.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(port.reads.load(Ordering::SeqCst), 0);
    let current = read_bot(&client, bot.id);
    let open = current.open_operation.clone().unwrap();
    assert_eq!(
        (open.request_id, open.refusal),
        (request, Some(BotLifecycleRefusal::WorkRunning))
    );
    // A repeat naming only the id resumes it; the work is read from the
    // delegate, which still runs it, so the Archive waits.
    archive_at(&client, &bot, request, current.revision).unwrap();
    until("a delegate read after the repeat", || {
        port.reads.load(Ordering::SeqCst) >= 1 && port.stops_of(run) >= 1
    });
    let moving = read_operation(&client, bot.id, request);
    assert_eq!(moving.stage, BotLifecycleStage::Settling, "{moving:?}");
    // Only the AI worker restarts, mid-settling. Nothing is remembered: the
    // journal resumes it and the delegate is asked again.
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    let reads = port.reads.load(Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    until("a delegate read after the restart", || {
        port.reads.load(Ordering::SeqCst) > reads
    });
    assert_eq!(
        read_operation(&client, bot.id, request).stage,
        BotLifecycleStage::Settling
    );
    port.api.lock().unwrap()[0].pending = 0;
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    assert_eq!(port.interrupts_of(run), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_parked_archive_found_through_the_bot_read_is_reversed_by_its_id() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let request = captured_archive(&mut repository, &bot);
    repository
        .park_bot_lifecycle(
            bot.user_scope,
            request,
            BotLifecycleRefusal::OwnerUnavailable,
            2,
        )
        .unwrap();
    let port = Arc::new(Archiving::default());
    // The owner is still away: one that answers would take the Archive up
    // again (`answered.rs`) before the person could reverse it.
    port.unavailable.store(true, Ordering::SeqCst);
    let (worker, _returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    // The roster and the read both name it.
    let Ok(BotResponse::List { bots }) = client.bots(
        context(true),
        BotCommand::List {
            workspace: bot.home_workspace,
        },
    ) else {
        panic!("roster")
    };
    assert_eq!(
        bots[0].open_operation.as_ref().map(|o| o.request_id),
        Some(request)
    );
    let current = read_bot(&client, bot.id);
    let open = current.open_operation.unwrap();
    let BotResponse::Lifecycle { operation } = client
        .bots(
            context(true),
            BotCommand::Restore {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: current.revision,
                source_request_id: open.request_id,
            },
        )
        .unwrap()
    else {
        panic!("restore progress")
    };
    assert_eq!(operation.stage, BotLifecycleStage::Completed);
    let after = read_bot(&client, bot.id);
    assert_eq!(after.open_operation, None);
    assert!(after.paused);
    assert_eq!(after.lifecycle, BotLifecycle::Active);
    // A repeat of it by its id no longer moves it: it answers its progress,
    // which says it was reversed and keeps why it had parked (before ai18
    // was amended it read `incomplete`, as it stopped).
    let repeated = archive_at(&client, &bot, request, after.revision).unwrap();
    assert_eq!(repeated.stage, BotLifecycleStage::Reversed);
    assert!(repeated.refusal.is_some());
    assert_eq!(port.reads.load(Ordering::SeqCst), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn while_archive_is_unfinished_native_evidence_is_its_effects_off_barrier() {
    let Running {
        worker,
        client,
        returned: _returned,
        port,
        bot,
        attempt: _,
    } = running(false);
    // The conversation task's Stop stays unconfirmed, so the Archive keeps
    // settling: unfinished and not reversed.
    port.stop_pending.store(true, Ordering::SeqCst);
    let request = BotRequestId::mint();
    archive(&client, &bot, request);
    let run = port.conversation_run();
    until("the task's Stop reached the owner", || {
        port.stops_of(run) >= 1
    });
    // The task is stopped at capture now; the Archive then settles.
    until("the Archive settles", || {
        read_operation(&client, bot.id, request).stage == BotLifecycleStage::Settling
    });
    // The Bot is held while it settles.
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::CreateChat {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                },
            )
            .unwrap_err()
            .code,
        PENDING
    );
    let installed = port.policies.lock().unwrap()[0].clone();
    let submission = port
        .native
        .state
        .lock()
        .unwrap()
        .submission
        .clone()
        .unwrap();
    let evidence = client
        .native_policy(crate::worker::bots::policy::NativePolicyRequest {
            run: submission.request.run,
            provenance: submission.provenance.clone(),
            captured: submission.policy.clone(),
            target: None,
        })
        .map_err(|e| e.code)
        .unwrap();
    // The very barrier the operation installed: effects off, same request,
    // same revision. The gateway installing it again is a no-op.
    assert!(!evidence.barrier.effects_allowed);
    assert_eq!(evidence.barrier, installed);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_restart_resumes_a_captured_archive_from_the_journal_and_waits_for_the_barrier() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    // Captured by an earlier worker that stopped before any owner call.
    let request = captured_archive(&mut repository, &bot);
    let port = Arc::new(Archiving {
        // Two installs see native effects still in flight.
        policy_pending: Mutex::new(2),
        ..Archiving::default()
    });
    let (worker, returned) = spawn(repository, authority, port.clone());
    let client = worker.client();
    let done = settled(&client, bot.id, request);
    assert_eq!(done.stage, BotLifecycleStage::Completed, "{done:?}");
    // The pause went out through its receipt; the barrier was re-asked until
    // effective; the delegate was asked and reported nothing to settle.
    assert!(!port.barriers.lock().unwrap().is_empty());
    assert!(port.policies.lock().unwrap().len() >= 3);
    assert!(port.reads.load(Ordering::SeqCst) >= 1);
    assert!(port.stops.lock().unwrap().is_empty());
    assert!(port.interrupts.lock().unwrap().is_empty());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repository = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        repository.bot_any_lifecycle(bot.id).unwrap().unwrap().1,
        BotLifecycle::Archived
    );
}

#[test]
fn a_progress_read_prepared_before_the_revision_moves_still_answers() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let request = captured_archive(&mut repository, &bot);
    // Parked for work that keeps running, so the owner's answer to the pause
    // below leaves it parked for the person (one parked for the owner's
    // silence would be taken up again, `answered.rs`).
    repository
        .park_bot_lifecycle(bot.user_scope, request, BotLifecycleRefusal::WorkRunning, 2)
        .unwrap();
    let current = repository.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let pause = repository
        .bot_lifecycle_operation(bot.user_scope, request)
        .unwrap()
        .unwrap()
        .pause_request
        .unwrap();
    let chat = repository
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let prepared = proof(authority.clone(), &current, chat);
    let (worker, _returned) = spawn(
        repository,
        authority.clone(),
        Arc::new(Archiving::default()),
    );
    let client = worker.client();
    // Startup recovery sends the captured pause receipt first; once it is
    // accepted nothing else inspects authority, so the gate below holds the
    // read's own preparation.
    until("the pause receipt was accepted", || {
        matches!(
            client.bots(
                context(true),
                BotCommand::ReadOperation {
                    request_id: pause,
                    bot_id: bot.id,
                },
            ),
            Ok(BotResponse::Operation { operation })
                if operation.status == BotOperationStatus::Accepted
        )
    });
    // The read's preparation takes its snapshot (this revision), then waits
    // in the authority inspection.
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *authority.inspect_gate.lock().unwrap() = Some((entered_tx, release));
    let reader = {
        let client = client.clone();
        std::thread::spawn(move || {
            client.bots(
                context(true),
                BotCommand::ReadLifecycleOperation {
                    request_id: request,
                    bot_id: bot.id,
                },
            )
        })
    };
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // Meanwhile the revision moves: reversing the parked Archive raises it.
    // Its context is already prepared, so it does not queue behind the read.
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: prepared,
            request: BotCommand::Restore {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: current.revision,
                source_request_id: request,
            },
            reply,
        })
        .unwrap();
    assert!(matches!(
        response.recv_timeout(Duration::from_secs(5)).unwrap(),
        Ok(BotResponse::Lifecycle { .. })
    ));
    release_tx.send(()).unwrap();
    // A progress read is not fenced to the revision it was prepared at.
    let read = reader.join().unwrap();
    assert!(
        matches!(&read, Ok(BotResponse::Lifecycle { operation }) if operation.request_id == request),
        "{:?}",
        read.map(|_| ()).map_err(|error| error.code)
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn start_on_a_trashed_bot_reads_its_stable_trash_code() {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    repository
        .fixture_set_bot_lifecycle(bot.id, BotLifecycle::Trashed)
        .unwrap();
    let (worker, _returned) = spawn(repository, authority, Arc::new(Archiving::default()));
    let code = worker
        .client()
        .bots(
            context(true),
            BotCommand::Start {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                chat_id: bot.main_chat,
                expected_revision: bot.revision,
                destination: bot.home_workspace,
                task: BotTask {
                    text: "CURRENT-TASK".into(),
                    documents: vec![],
                },
            },
        )
        .unwrap_err()
        .code;
    assert_eq!(code, BOT_TRASHED);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
