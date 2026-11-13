// SPDX-License-Identifier: MPL-2.0

//! From a run that exists to a provider that is working on it.
//!
//! In order, because each step is what the next one relies on: the
//! attempt row, `Starting`; admission (P1.5 — a manual start onto a busy
//! folder warns, records the warning as a step, and proceeds, D34); the
//! materialized context under `<run-workspace>/.koru/context/<RunId>/`
//! (§7.2 step 3); the preamble with the run's line appended (P1.12); the
//! session key **bound in the ledger before the host is asked** (§7.6:
//! a restart finds the key here and asks the non-creating lookup about
//! it); the delegated creation carrying the MCP endpoint (§7.3) and the
//! frozen grant set (D28); the subscription **before** the first
//! submission, so no event is missed; and only then the objective.
//!
//! A failure at any step is `Failed { stage }` on the run, `Ended` on the
//! attempt, and the claim released — start failures are failures with a
//! stage (§7.5), never `Interrupted`.

use std::path::{Path, PathBuf};

use koru_delegate_core::{
    FailureStage, IdempotencyKey, ProviderSessionKey, RunEvent, RunId, RunStatus, StepKind,
    TimelineRef, TimelineRefs, WorkspaceScope,
};
use koru_delegate_scope::{
    Admission, ResolvedScope, RootLookup, ScopeRefusal, ScopeRoots, StartOrigin,
};
use koru_delegate_sqlite::AttemptPhase;

use crate::{CapturedContext, McpLaunch, RunFact, SessionLaunch, orchestrator::state::Attempt};

use super::{
    Orchestrator, RunError, Started,
    resume::{ResumePlan, compose_resume_submission},
};

/// The folder a run writes its results under, inside its working scope.
pub const ARTIFACTS_FOLDER: &str = ".artifacts";
/// The folder the materialized context lives under.
pub const CONTEXT_FOLDER: &str = ".koru/context";

impl Orchestrator {
    /// Start a run that exists and has not started (`Created`).
    pub fn start(&mut self, run: RunId, origin: StartOrigin) -> Result<Started, RunError> {
        match self.status(run)? {
            RunStatus::Created => {}
            other => {
                return Err(RunError::WrongStatus {
                    status: other.name(),
                });
            }
        }
        self.validate_bot_start_origin(run, &origin)?;
        let authored = self.header(run)?.scope;
        match self.plan_scope(run, &authored) {
            Ok(scope) => self.start_resolved(run, scope, origin),
            Err(_) if matches!(authored, WorkspaceScope::BotFolder { .. }) => {
                Err(super::bots::refused("bot_folder_unavailable"))
            }
            Err(_) => self.fail_scope_start(run),
        }
    }

    /// Start a durably claimed schedule using the exact scope resolved for
    /// its admission. This in-memory token prevents a moved project root or
    /// temporary-base edit from changing the folder between claim and launch.
    pub fn start_claimed(
        &mut self,
        run: RunId,
        mut scope: ResolvedScope,
        origin: StartOrigin,
    ) -> Result<Started, RunError> {
        match self.status(run)? {
            RunStatus::Created => {}
            other => {
                return Err(RunError::WrongStatus {
                    status: other.name(),
                });
            }
        }
        self.validate_bot_start_origin(run, &origin)?;
        let header = self.header(run)?;
        if !self.claims.holders_of(&scope.identity).is_empty() {
            return Err(crate::PortError::new(
                crate::Port::Scope,
                "claimed_scope_busy",
                "the claimed scope is temporarily held",
            )
            .into());
        }
        self.verify_resolved_scope(run, &header.scope, &mut scope)?;
        self.begin_attempt(run, origin, RunEvent::Start, None, Some(scope))
    }

    /// Start a manual run in the exact scope identity already checked against
    /// outstanding scheduled reservations. This closes the root-change window
    /// between that check and ordinary scope admission.
    pub fn start_resolved(
        &mut self,
        run: RunId,
        mut scope: ResolvedScope,
        origin: StartOrigin,
    ) -> Result<Started, RunError> {
        match self.status(run)? {
            RunStatus::Created => {}
            other => {
                return Err(RunError::WrongStatus {
                    status: other.name(),
                });
            }
        }
        self.validate_bot_start_origin(run, &origin)?;
        let authored = self.header(run)?.scope;
        if self
            .verify_resolved_scope(run, &authored, &mut scope)
            .is_err()
        {
            if matches!(authored, WorkspaceScope::BotFolder { .. }) {
                return Err(super::bots::refused("bot_folder_unavailable"));
            }
            return self.fail_scope_start(run);
        }
        self.begin_attempt(run, origin, RunEvent::Start, None, Some(scope))
    }

    fn fail_scope_start(&mut self, run: RunId) -> Result<Started, RunError> {
        let row = self.ports.store.open_attempt(run)?;
        let attempt = Attempt::new(row.attempt, row.generation);
        let generation = attempt.generation;
        self.open(run)?.attempt = Some(attempt);
        self.apply(run, &RunEvent::Start)?;
        self.ports
            .store
            .advance(run, generation, AttemptPhase::Starting)?;
        self.start_failed(run, FailureStage::ScopeAdmission)
    }

    pub(super) fn verify_resolved_scope(
        &self,
        _run: RunId,
        authored: &WorkspaceScope,
        scope: &mut ResolvedScope,
    ) -> Result<(), RunError> {
        if authored.is_koru_owned() && std::fs::create_dir_all(&scope.directory).is_err() {
            return Err(crate::PortError::new(
                crate::Port::Scope,
                "claimed_scope_create",
                "the claimed temporary scope could not be created",
            )
            .into());
        }
        let actual = match authored {
            WorkspaceScope::BotFolder { folder } => self.ports.roots.bot_folder(folder),
            _ => koru_delegate_scope::ResolvedScope::of_directory(&scope.directory),
        }
        .map_err(|_| {
            crate::PortError::new(
                crate::Port::Scope,
                "claimed_scope_resolve",
                "the claimed scope could not be resolved",
            )
        })?;
        if actual.identity != scope.identity {
            return Err(crate::PortError::new(
                crate::Port::Scope,
                "claimed_scope_changed",
                "the claimed scope identity changed before launch",
            )
            .into());
        }
        scope.directory = actual.directory;
        Ok(())
    }

    /// The attempt path shared by a start and a resume: `entry` is the
    /// event that moves the run to `Starting`, and `resume` is what a
    /// resume decided — the seam, its token, the artifacts so far.
    pub(crate) fn begin_attempt(
        &mut self,
        run: RunId,
        origin: StartOrigin,
        entry: RunEvent,
        resume: Option<ResumePlan>,
        resolved_scope: Option<ResolvedScope>,
    ) -> Result<Started, RunError> {
        let header = self.header(run)?;
        match (&header.bot, origin) {
            (Some(bot), StartOrigin::Bot { bot: expected }) if bot.bot == expected => {
                let record = self
                    .bot_ledger()?
                    .bot_admission(run)?
                    .ok_or_else(|| super::bots::refused("bot_admission_required"))?;
                if record.state != koru_delegate_core::BotAdmissionState::Reserved
                    || record.submission.provenance != *bot
                    || self.ports.store.current_attempt(run)?.is_some()
                {
                    return Err(super::bots::refused("bot_fenced"));
                }
                // Public start variants share this last attempt boundary. A
                // retained reservation alone is never a current-boot grant.
                if !self.bot_boot_ack_current(
                    bot.owner,
                    bot.bot,
                    bot.config_revision,
                    bot.policy_revision,
                )? || self
                    .bot_ledger()?
                    .bot_barrier(bot.owner, bot.bot)?
                    .is_none_or(|barrier| barrier.paused)
                {
                    return Err(super::bots::refused("bot_boot_pending"));
                }
            }
            (None, StartOrigin::Bot { .. }) | (Some(_), _) => {
                return Err(super::bots::refused("bot_admission_required"));
            }
            _ => {}
        }
        if !self.ports.sessions.supports_delegation() {
            return Err(RunError::NotDelegatable);
        }
        let row = self.ports.store.open_attempt(run)?;
        if header.bot.is_some() {
            self.bot_ledger()?
                .activate_bot(run, row.attempt, row.generation)?;
        }
        let key = IdempotencyKey::mint();
        let mut attempt = Attempt::new(row.attempt, row.generation);
        attempt.is_bot = header.bot.is_some();
        let (attempt_id, generation) = (attempt.id, attempt.generation);
        self.open(run)?.attempt = Some(attempt);
        self.apply(run, &entry)?;
        self.ports
            .store
            .advance(run, generation, AttemptPhase::Starting)?;
        if let Some(plan) = &resume {
            // The timeline says which seam (P1.9).
            self.step(
                run,
                StepKind::Resumed { via: plan.via },
                TimelineRefs::new([TimelineRef::Attempt {
                    attempt: attempt_id,
                }])
                .expect("one ref"),
                Some(RunFact::Resumed {
                    run,
                    attempt: attempt_id,
                    generation,
                    via: plan.via,
                }),
            )?;
        }

        let header = self.open(run)?.doc.document().header()?;

        // Admission (P1.5, D34).
        let (admission, resolved_directory) = match resolved_scope {
            Some(resolved) => (
                self.claims
                    .admit_resolved(run, attempt_id, resolved.identity, origin),
                Some(resolved.directory),
            ),
            None => (
                self.claims.admit(
                    run,
                    attempt_id,
                    &header.scope,
                    self.ports.roots.as_ref(),
                    origin,
                ),
                None,
            ),
        };
        match admission {
            Admission::Admitted { warning: None } => {}
            Admission::Admitted {
                warning: Some(busy),
            } => {
                let holders = u32::try_from(busy.holders().count()).unwrap_or(u32::MAX);
                let refs = TimelineRefs::new(
                    busy.holders()
                        .take(koru_delegate_core::MAX_TIMELINE_REFS)
                        .map(|holder| TimelineRef::Attempt {
                            attempt: holder.attempt,
                        }),
                )
                .expect("bounded above");
                self.step(
                    run,
                    StepKind::ScopeBusy { holders },
                    refs,
                    Some(RunFact::ScopeBusy {
                        run,
                        attempt: attempt_id,
                        holder: busy.holder.run,
                        holder_attempt: busy.holder.attempt,
                        // `holders_of` keeps only claims that overlap, so
                        // the disjoint answer cannot arrive here; `Same` is
                        // the conservative reading if it ever did.
                        overlap: busy
                            .holder
                            .overlap
                            .for_candidate()
                            .unwrap_or(koru_delegate_core::ScopeOverlap::Same),
                        others: u32::try_from(busy.others.len()).unwrap_or(u32::MAX),
                    }),
                )?;
            }
            // A scheduled firing onto a busy folder is skipped and recorded
            // (D34). Nothing fires on a schedule before M2, and P2.3 gives
            // this arm its own answer; until then the attempt ends the way
            // a refused admission does, so nothing starts.
            Admission::Skipped { .. } | Admission::Refused(_) => {
                return self.start_failed(run, FailureStage::ScopeAdmission);
            }
        }
        let directory = match resolved_directory {
            Some(directory) => directory,
            None => match scope_directory(&header.scope, run, self.ports.roots.as_ref()) {
                Ok(directory) => directory,
                Err(_) => return self.start_failed(run, FailureStage::ScopeAdmission),
            },
        };
        // Materialization (§7.2 step 3).
        let captured = CapturedContext {
            pins: header.context.clone(),
        };
        if let Err(failure) = self.ports.capture.materialize(run, &captured, &directory) {
            self.trace(
                run,
                "materialize_failed",
                format_args!("item {} at {:?}", failure.item, failure.stage),
            );
            return self.start_failed(run, FailureStage::ContextMaterialization);
        }
        {
            let state = self.open(run)?;
            let attempt = state.attempt.as_mut().expect("opened above");
            attempt.run_workspace = directory.clone();
            attempt.captured = captured;
        }

        // The preamble, with the run's line appended (P1.12), then the
        // objective: the first submission.
        let first_submission = if header.bot.is_some() {
            String::new()
        } else {
            let preamble = self.ports.preamble.resolve();
            if let Some(notice) = preamble.notice {
                tracing::warn!(
                    target: "koru::delegate",
                    source = ?preamble.source,
                    notice = ?notice,
                    "the edited preamble could not be used; the bundled copy was"
                );
            }
            match &resume {
                Some(plan) => compose_resume_submission(
                    &preamble.text,
                    run,
                    &directory,
                    header.objective.as_str(),
                    plan,
                ),
                None => compose_first_submission(
                    &preamble.text,
                    run,
                    &directory,
                    header.objective.as_str(),
                ),
            }
        };

        // Bind before you ask (§7.6).
        let host = self.ports.sessions.host_instance()?;
        self.ports.store.bind_session(
            run,
            generation,
            ProviderSessionKey {
                idempotency: key,
                session: None,
            },
            host,
        )?;
        let workspace = self.open(run)?.workspace;
        // The run's authority (§7.3): the endpoint, the bearer for this
        // attempt, and the tools. Issued before the provider exists, so
        // nothing ever starts holding a credential the gateway does not
        // know about — and a refusal fails the start rather than launching
        // a provider whose every tool call would answer 401.
        let tool_authority = match self.resolve_run_tool_authority(
            run,
            generation,
            header.executing_device().ok_or(RunError::UnknownRun)?,
        ) {
            Ok(authority) => authority,
            Err(_) => return self.start_failed(run, FailureStage::Authority),
        };
        let mut delegation = match self.ports.mcp.launch(McpLaunch {
            authority: tool_authority,
            run,
            generation,
            workspace,
            grants: &header.grants,
            run_workspace: &directory,
        }) {
            Ok(delegation) => delegation,
            Err(error) => {
                self.trace(
                    run,
                    "authority_failed",
                    format_args!(
                        "{:?}",
                        koru_delegate_core::RunFailureReason::from_code(&error.code)
                    ),
                );
                return self.start_failed_with_reason(
                    run,
                    FailureStage::Authority,
                    Some(koru_delegate_core::RunFailureReason::from_code(&error.code)),
                );
            }
        };
        // The adapter's resume seam: the token continues the provider
        // session; its absence is the recorded-context fallback.
        delegation.resume = resume.and_then(|plan| plan.token);
        delegation.model = match header.requested_model.as_ref() {
            Some(model) => match model.as_str().parse() {
                Ok(model) => Some(model),
                Err(_) => return self.start_failed(run, FailureStage::SessionCreation),
            },
            None => None,
        };
        let created = match self.ports.sessions.create_delegated(SessionLaunch {
            is_bot: header.bot.is_some(),
            key,
            profile: header.agent.clone(),
            project: header.scope.project(),
            delegation,
            working_directory: directory,
        }) {
            Ok(created) => created,
            Err(error) => {
                self.trace(
                    run,
                    "create_failed",
                    format_args!(
                        "{:?}",
                        koru_delegate_core::RunFailureReason::from_code(&error.code)
                    ),
                );
                return self.start_failed_with_reason(
                    run,
                    FailureStage::SessionCreation,
                    Some(koru_delegate_core::RunFailureReason::from_code(&error.code)),
                );
            }
        };
        self.ports.store.bind_session(
            run,
            generation,
            ProviderSessionKey {
                idempotency: key,
                session: Some(created.session),
            },
            host,
        )?;
        {
            let state = self.open(run)?;
            state.provider = Some(created.provider.clone());
            state.requested_model = header
                .requested_model
                .as_ref()
                .map(|model| model.as_str().to_owned())
                .or_else(|| {
                    created
                        .requested_model
                        .as_ref()
                        .map(|model| model.as_str().to_owned())
                });
            let attempt = state.attempt.as_mut().expect("opened above");
            attempt.session = Some(created.session);
        }
        // The controller, the stream, and only then the first turn.
        if let Err(error) = self.ports.sessions.attach(created.session) {
            self.trace(
                run,
                "attach_failed",
                format_args!(
                    "{:?}",
                    koru_delegate_core::RunFailureReason::from_code(&error.code)
                ),
            );
            return self.start_failed_with_reason(
                run,
                FailureStage::ProviderStart,
                Some(koru_delegate_core::RunFailureReason::from_code(&error.code)),
            );
        }
        let events = match self.ports.sessions.subscribe(created.session, None) {
            Ok(events) => events,
            Err(error) => {
                self.trace(
                    run,
                    "subscribe_failed",
                    format_args!(
                        "{:?}",
                        koru_delegate_core::RunFailureReason::from_code(&error.code)
                    ),
                );
                return self.start_failed_with_reason(
                    run,
                    FailureStage::ProviderStart,
                    Some(koru_delegate_core::RunFailureReason::from_code(&error.code)),
                );
            }
        };
        if header.bot.is_some() {
            // AI supplies assembled conversation context transiently only after
            // observation. Never auto-submit the persisted current-task intent.
            self.open(run)?
                .attempt
                .as_mut()
                .expect("opened above")
                .holds_lease = true;
            return Ok(Started {
                run,
                attempt: attempt_id,
                generation,
                status: self.status(run)?,
                events: Some(events),
            });
        }
        let turn = match self
            .ports
            .sessions
            .submit(created.session, first_submission)
        {
            Ok(turn) => turn,
            Err(error) => {
                self.trace(
                    run,
                    "submit_failed",
                    format_args!(
                        "{:?}",
                        koru_delegate_core::RunFailureReason::from_code(&error.code)
                    ),
                );
                return self.start_failed_with_reason(
                    run,
                    FailureStage::ProviderStart,
                    Some(koru_delegate_core::RunFailureReason::from_code(&error.code)),
                );
            }
        };
        {
            let state = self.open(run)?;
            let attempt = state.attempt.as_mut().expect("opened above");
            attempt.holds_lease = true;
            attempt.orchestrator_turn = Some(turn);
        }
        // Keep Starting until the first submission succeeds, so attach,
        // subscription and submit refusals use the existing StartFailed path.
        self.ports
            .store
            .advance(run, generation, AttemptPhase::Running)?;
        self.apply(run, &RunEvent::ProviderStarted)?;
        self.step(
            run,
            StepKind::Started,
            TimelineRefs::new([
                TimelineRef::Attempt {
                    attempt: attempt_id,
                },
                TimelineRef::Session {
                    session: created.session,
                },
            ])
            .expect("two refs"),
            Some(RunFact::Started {
                run,
                attempt: attempt_id,
                generation,
                session: created.session,
            }),
        )?;

        self.trace(
            run,
            "started",
            format_args!("generation {}", generation.get()),
        );
        Ok(Started {
            run,
            attempt: attempt_id,
            generation,
            status: self.status(run)?,
            events: Some(events),
        })
    }

    /// A start failure is `Failed { stage }` (§7.5) and the attempt ends.
    pub(crate) fn start_failed(
        &mut self,
        run: RunId,
        stage: FailureStage,
    ) -> Result<Started, RunError> {
        self.start_failed_with_reason(run, stage, None)
    }

    pub(crate) fn start_failed_with_reason(
        &mut self,
        run: RunId,
        stage: FailureStage,
        reason: Option<koru_delegate_core::RunFailureReason>,
    ) -> Result<Started, RunError> {
        let (attempt_id, generation) = self
            .open(run)?
            .attempt_ids()
            .expect("an attempt is open while starting");
        tracing::info!(target: "koru::delegate", event = "delegate.start.failed", run = %run, stage = ?stage, reason = ?reason, "Delegated run could not start");
        self.apply_with_failure_reason(run, &RunEvent::StartFailed { stage }, reason)?;
        // The Failed step was retained atomically with status and reason.
        self.publish(
            run,
            RunFact::Failed {
                run,
                attempt: Some(attempt_id),
                generation,
                stage,
            },
        )?;
        self.end_attempt(run, AttemptPhase::Ended, true)?;
        Ok(Started {
            run,
            attempt: attempt_id,
            generation,
            status: RunStatus::Failed { stage },
            events: None,
        })
    }

    /// The attempt is over: the ledger says how, the claim is released,
    /// the session is ended when `terminate` asks, the materialized
    /// context is cleaned up, and the state forgets the attempt while
    /// remembering which one it was.
    pub(crate) fn end_attempt(
        &mut self,
        run: RunId,
        phase: AttemptPhase,
        terminate: bool,
    ) -> Result<(), RunError> {
        if self.header(run)?.bot.is_some() {
            return self.bot_end_attempt(run, phase, terminate);
        }
        let state = self.open(run)?;
        let Some(attempt) = state.attempt.take() else {
            return Ok(());
        };
        state.last_attempt = Some((attempt.id, attempt.generation));
        let generation = attempt.generation;
        self.claims.release(attempt.id);
        self.ports.store.advance(run, generation, phase)?;
        if phase == AttemptPhase::Ended {
            // A cleanly ended attempt authorises the next one on its own;
            // an interrupted one is retired by a person's resume (§7.6).
            self.ports.store.retire(run, generation)?;
        }
        if terminate && let Some(session) = attempt.session {
            // Sessions are ephemeral; the run's result is what lasts. A
            // session that will not end is the Job Object's to end.
            if let Err(error) = self.ports.sessions.terminate(session) {
                self.trace(
                    run,
                    "terminate_failed",
                    format_args!(
                        "{:?}",
                        koru_delegate_core::RunFailureReason::from_code(&error.code)
                    ),
                );
            }
        }
        if !attempt.run_workspace.as_os_str().is_empty() {
            self.ports.capture.cleanup(run, &attempt.run_workspace);
        }
        self.trace(run, "attempt_ended", format_args!("{}", phase.token()));
        Ok(())
    }
}

/// The directory a scope names, resolved through the roots. The same
/// arithmetic `ScopeIdentity::resolve` performs for admission; admission
/// has already refused a folder outside its project.
pub(crate) fn scope_directory(
    scope: &WorkspaceScope,
    run: RunId,
    roots: &dyn ScopeRoots,
) -> Result<PathBuf, ScopeRefusal> {
    match scope {
        WorkspaceScope::None => Err(ScopeRefusal::NotAdmissible),
        WorkspaceScope::BotFolder { folder } => Ok(PathBuf::from(folder.as_str())),
        WorkspaceScope::ProjectRoot { project } => match roots.project_root(*project) {
            RootLookup::Directory(root) => Ok(root),
            RootLookup::NoRoot => Err(ScopeRefusal::ProjectHasNoRoot),
            RootLookup::UnknownProject => Err(ScopeRefusal::UnknownProject),
        },
        WorkspaceScope::ProjectFolder { project, folder } => match roots.project_root(*project) {
            RootLookup::Directory(root) => Ok(root.join(folder.as_str())),
            RootLookup::NoRoot => Err(ScopeRefusal::ProjectHasNoRoot),
            RootLookup::UnknownProject => Err(ScopeRefusal::UnknownProject),
        },
        WorkspaceScope::Temporary => roots
            .temporary_directory(run)
            .ok_or(ScopeRefusal::NoTemporaryDirectory),
    }
}

/// `<run-workspace>/.artifacts/<RunId>/`.
pub fn artifacts_directory(run_workspace: &Path, run: RunId) -> PathBuf {
    run_workspace.join(ARTIFACTS_FOLDER).join(run.to_storage())
}

/// `<run-workspace>/.koru/context/<RunId>/`.
pub fn context_directory(run_workspace: &Path, run: RunId) -> PathBuf {
    run_workspace.join(CONTEXT_FOLDER).join(run.to_storage())
}

/// The preamble as resolved, the one line the worker appends naming the
/// run and its two absolute folders (P1.12), then the objective.
///
/// Provider payload: it reaches the session and nothing else.
pub(crate) fn compose_first_submission(
    preamble: &str,
    run: RunId,
    run_workspace: &Path,
    objective: &str,
) -> String {
    format!(
        "{}\n\nThis run is `{}`. Save results under `{}` and register them; the context you were given is under `{}`.\n\n{}",
        preamble.trim_end(),
        run,
        artifacts_directory(run_workspace, run).display(),
        context_directory(run_workspace, run).display(),
        objective
    )
}
