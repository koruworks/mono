// SPDX-License-Identifier: MPL-2.0

//! Resume — the fresh attempt after an interruption (§7.6, D10, D48).
//!
//! A person asks; nothing resumes a run on its own. In order, because each
//! step is what the next one relies on:
//!
//! 1. the run must be `Interrupted` — a run that is running is reattached
//!    to, not resumed;
//! 2. an interruption whose prior attempt is *unknown* (§7.6's last row)
//!    needs the person's explicit `confirm`, because they are told earlier
//!    work may exist. **It is not evidence** — nothing a person says proves
//!    an unobserved provider stopped, and the fence below is what does;
//! 3. D48's admission runs over the **original pins**: a source now in the
//!    trash or gone refuses the resume with its stable code, and no key is
//!    retired for a resume that will not happen;
//! 4. the previous attempt's creation key is **retired at the host before
//!    the next generation opens**, so a delayed create under it is refused,
//!    and a key found live is reattached to, never resumed past;
//! 5. the adapter's resume seam is asked. A token continues the provider
//!    session (`Native`); its absence is the recorded-context fallback
//!    (`RecordedContext`) — a fresh session whose first submission is the
//!    preamble, the objective and a bounded list of the artifacts registered
//!    so far. **The timeline says which.**
//!
//! The pins are the original ones (§7.2: *resume keeps the original pins*),
//! so `begin_attempt` re-materializes exactly what the header names. The
//! captures themselves are untouched — run cleanup removes materialized
//! copies, never captures — so nothing is captured a second time.

use std::path::Path;

use koru_delegate_core::{
    PriorAttempt, ResultReference, ResumeSeam, RunAttemptId, RunEvent, RunHeader, RunId, RunStatus,
};
use koru_delegate_scope::{ResolvedScope, StartOrigin};
use koru_delegate_sqlite::AttemptPhase;
use koru_session_protocol::{CreationLookup, ProviderResumeToken};

use super::{
    Orchestrator, RunError, Started,
    start::{artifacts_directory, context_directory},
};

/// The most artifacts the fallback submission names. A run that registered
/// more is told the most recent ones and the count, so a long run's first
/// submission stays bounded.
pub(crate) const RESUME_ARTIFACTS_BOUND: usize = 32;

/// What a resume decided before the attempt began: which seam, the token
/// for the native one, and what the fallback submission names.
pub(crate) struct ResumePlan {
    pub via: ResumeSeam,
    pub token: Option<ProviderResumeToken>,
    /// The artifacts registered so far, oldest first, bounded.
    pub artifacts: Vec<ResultReference>,
    /// How many were registered in all.
    pub registered: usize,
}

impl Orchestrator {
    /// Validate status and confirmation before a host performs filesystem
    /// scope preparation for Resume.
    pub fn resumable_scope(
        &mut self,
        run: RunId,
        confirm: bool,
    ) -> Result<koru_delegate_core::WorkspaceScope, RunError> {
        if self.header(run)?.bot.is_some() {
            return Err(super::bots::refused("bot_continue_requires_new_run"));
        }
        let prior = match self.status(run)? {
            RunStatus::Interrupted { prior_attempt } => prior_attempt,
            other => {
                return Err(RunError::NotInterrupted {
                    status: other.name(),
                });
            }
        };
        if prior == PriorAttempt::Unknown && !confirm {
            return Err(RunError::PriorAttemptUnknown);
        }
        Ok(self.open(run)?.doc.document()?.header()?.scope)
    }

    /// Continue an interrupted run as a fresh attempt. `confirm` is the
    /// person's explicit answer for a run whose prior attempt is unknown.
    pub fn resume(&mut self, run: RunId, confirm: bool) -> Result<Started, RunError> {
        self.ensure_scope_recovery()?;
        let authored = self.resumable_scope(run, confirm)?;
        let scope = self.plan_scope(run, &authored).map_err(|_| {
            crate::PortError::new(
                crate::Port::Scope,
                "scope_unavailable",
                "the working scope is not available on this device",
            )
        })?;
        self.resume_with_scope(run, confirm, Some(scope))
    }

    pub fn resume_resolved(
        &mut self,
        run: RunId,
        confirm: bool,
        scope: ResolvedScope,
    ) -> Result<Started, RunError> {
        self.resume_with_scope(run, confirm, Some(scope))
    }

    fn resume_with_scope(
        &mut self,
        run: RunId,
        confirm: bool,
        mut resolved_scope: Option<ResolvedScope>,
    ) -> Result<Started, RunError> {
        self.ensure_scope_recovery()?;
        if self.header(run)?.bot.is_some() {
            return Err(super::bots::refused("bot_continue_requires_new_run"));
        }
        let prior = match self.status(run)? {
            RunStatus::Interrupted { prior_attempt } => prior_attempt,
            other => {
                return Err(RunError::NotInterrupted {
                    status: other.name(),
                });
            }
        };
        if prior == PriorAttempt::Unknown && !confirm {
            return Err(RunError::PriorAttemptUnknown);
        }
        if !self.ports.sessions.supports_delegation() {
            return Err(RunError::NotDelegatable);
        }

        // D48, over the original pins, before anything is fenced.
        let header = self.open(run)?.doc.document()?.header()?;
        if let Some(scope) = resolved_scope.as_mut() {
            self.verify_resolved_scope(run, &header.scope, scope)?;
        }
        self.ports
            .capture
            .admit(run, &header.context)
            .map_err(RunError::SourceRefused)?;

        // The fence, then the seam.
        let mut token = None;
        let mut prior_attempt = None;
        if let Some(row) = self.ports.store.current_attempt(run)? {
            prior_attempt = Some(row.attempt);
            if let Some(key) = row.provider_session {
                let (_, lookup) = self.ports.sessions.retire(key.idempotency)?;
                if matches!(
                    lookup,
                    CreationLookup::Known { .. } | CreationLookup::Creating
                ) {
                    return Err(RunError::AttemptLive);
                }
                if let Some(session) = key.session {
                    token = self.ports.sessions.resume_token(session)?;
                }
            }
            if !row.retired {
                if !row.phase.is_terminal() {
                    self.ports
                        .store
                        .advance(run, row.generation, AttemptPhase::Interrupted)?;
                }
                self.ports.store.retire(run, row.generation)?;
            }
        }

        let (artifacts, registered) = self.artifacts_so_far(run, prior_attempt, &header);
        let via = if token.is_some() {
            ResumeSeam::Native
        } else {
            ResumeSeam::RecordedContext
        };
        self.trace(
            run,
            "resume",
            format_args!("{}, {registered} artifacts so far", seam_name(via)),
        );
        self.begin_attempt(
            run,
            StartOrigin::Manual,
            RunEvent::Resume,
            Some(ResumePlan {
                via,
                token,
                artifacts,
                registered,
            }),
            resolved_scope,
        )
    }

    /// The artifacts the run has registered so far: what the Run document
    /// holds, plus what the prior attempt registered while nobody watched
    /// (the registrations port, asked without a turn). Deduplicated by
    /// version, oldest first, the most recent [`RESUME_ARTIFACTS_BOUND`]
    /// kept, with the full count beside them.
    fn artifacts_so_far(
        &mut self,
        run: RunId,
        prior_attempt: Option<RunAttemptId>,
        header: &RunHeader,
    ) -> (Vec<ResultReference>, usize) {
        let mut all: Vec<ResultReference> = header.artifacts.clone();
        if let Some(attempt) = prior_attempt {
            for reference in self.ports.registrations.registered(run, attempt, None) {
                if !all.iter().any(|known| known.artifact == reference.artifact) {
                    all.push(reference);
                }
            }
        }
        let registered = all.len();
        let skip = registered.saturating_sub(RESUME_ARTIFACTS_BOUND);
        (all.into_iter().skip(skip).collect(), registered)
    }
}

/// The closed vocabulary a seam is traced under.
fn seam_name(via: ResumeSeam) -> &'static str {
    match via {
        ResumeSeam::Native => "native",
        ResumeSeam::RecordedContext => "recorded_context",
    }
}

/// The first submission of a resumed attempt: the preamble, the run's line,
/// what this attempt is — a continuation after an interruption — the
/// artifacts registered so far, and the objective.
///
/// Provider payload: it reaches the session and nothing else.
pub(crate) fn compose_resume_submission(
    preamble: &str,
    run: RunId,
    run_workspace: &Path,
    objective: &str,
    plan: &ResumePlan,
) -> String {
    let mut text = format!(
        "{}\n\nThis run is `{}`. Save results under `{}` and register them; the context you were given is under `{}`.\n\nAn earlier attempt of this run was interrupted. This is a fresh attempt: continue the objective from what was already delivered, and do not redo what is registered below.\n\n",
        preamble.trim_end(),
        run,
        artifacts_directory(run_workspace, run).display(),
        context_directory(run_workspace, run).display(),
    );
    if plan.artifacts.is_empty() {
        text.push_str("Artifacts registered so far: none.\n\n");
    } else {
        if plan.registered > plan.artifacts.len() {
            text.push_str(&format!(
                "Artifacts registered so far: {} (the most recent {} listed):\n",
                plan.registered,
                plan.artifacts.len()
            ));
        } else {
            text.push_str(&format!(
                "Artifacts registered so far ({}):\n",
                plan.artifacts.len()
            ));
        }
        for reference in &plan.artifacts {
            text.push_str(&format!(
                "- `{}` — {}\n",
                reference.artifact,
                reference.title.as_str()
            ));
        }
        text.push('\n');
    }
    text.push_str(objective);
    text
}
