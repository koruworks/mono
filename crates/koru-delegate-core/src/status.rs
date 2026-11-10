// SPDX-License-Identifier: MPL-2.0

//! A run's status, and the one closed function that moves it.
//!
//! §7.5 is the specification: which provider event means what to the run,
//! who may complete it, and what fences completion. Everything a run can be
//! is a [`RunStatus`]; everything that can happen to one is a [`RunEvent`];
//! [`transition`] is the only way from one to the other, and it refuses every
//! pair it does not explicitly allow. A test enumerates the pairs.
//!
//! Two shapes are deliberate:
//!
//! - **Markers, not more statuses.** An approval, a wait for input, a
//!   detected child session or a turn that ended with nothing registered
//!   (D29) is a run that is still `Running` and needs a look — an
//!   [`Attention`] on the state, never a sibling of it. Control handover
//!   (§7.4) is a second dimension, [`ControlState`], because it fences what
//!   the orchestrator may do and what a completion may mean.
//! - **Two states nothing reaches.** `AwaitingReview` and
//!   `WaitingForParticipant` are minted now, with no transition into or out
//!   of them, so M5 unlocks a transition instead of rewriting a terminal
//!   state.
//!
//! The attempt-generation fence (an event from an older attempt may not
//! touch status) is applied *before* this function is called — see
//! [`crate::AttemptGeneration::admits`]. This function reasons about one
//! attempt at a time.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

use crate::TurnOriginator;

/// Something a `Running` run wants a person to look at. A set, because an
/// approval and a detected child session can both be outstanding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// The provider asked for an approval. M1 answers it through the
    /// existing session controls (D29).
    ApprovalRequired,
    /// The provider is waiting for input. Not terminal (§7.5).
    WaitingForInput,
    /// The provider spawned a child session, which §7.3 terminates. The
    /// marker stays until the termination is observed.
    ChildSessionDetected,
    /// An orchestrator turn ended having registered no artifact: finished
    /// and needing attention, never a delivered result (D29).
    FinishedNeedingAttention,
}

/// The attention markers on a `Running` run. Sorted and duplicate-free.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct AttentionSet(BTreeSet<Attention>);

impl AttentionSet {
    pub fn new(markers: impl IntoIterator<Item = Attention>) -> Self {
        Self(markers.into_iter().collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, marker: Attention) -> bool {
        self.0.contains(&marker)
    }

    pub fn iter(&self) -> impl Iterator<Item = Attention> + '_ {
        self.0.iter().copied()
    }

    fn with(&self, marker: Attention) -> Self {
        let mut next = self.0.clone();
        next.insert(marker);
        Self(next)
    }

    fn without(&self, marker: Attention) -> Self {
        let mut next = self.0.clone();
        next.remove(&marker);
        Self(next)
    }
}

impl FromIterator<Attention> for AttentionSet {
    fn from_iter<I: IntoIterator<Item = Attention>>(iter: I) -> Self {
        Self::new(iter)
    }
}

/// Who holds the session's controller while a run is `Running` (§7.4).
///
/// `objective_satisfied` is the fence: an orchestrator turn that completed
/// the objective while a handover was pending does not end the run — the
/// handover it was about to yield to wins — and the fact is carried here so
/// that when control returns the run completes without a second turn.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "holder", rename_all = "snake_case")]
pub enum ControlState {
    /// The orchestrator submits turns.
    #[default]
    Orchestrator,
    /// A person asked for control; the request waits for the turn boundary,
    /// and the orchestrator submits nothing more until control returns.
    HandoverPending { objective_satisfied: bool },
    /// A person holds the lease. Automatic submission is fenced; observation
    /// stays live.
    Human { objective_satisfied: bool },
}

/// Why a start or a turn failed. A closed vocabulary — a stage, never a
/// message — so a failure can be reported anywhere without carrying text.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    /// The scope could not be admitted (P1.5).
    ScopeAdmission,
    /// A context item could not be captured (§7.2 step 2).
    ContextCapture,
    /// A captured item could not be materialized (§7.2 step 3).
    ContextMaterialization,
    /// The provider session could not be created.
    SessionCreation,
    /// The provider process did not start.
    ProviderStart,
    /// The run's authority record or MCP endpoint could not be established
    /// (§7.3).
    Authority,
    /// The provider reported the turn failed.
    Turn,
}

/// Why a turn was cancelled (D36). **Only `Run` terminalizes.**
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// The person ended the job. The run is `Cancelled`.
    Run,
    /// The person ended the *step* to take control immediately (§7.4). The
    /// run stays `Running`; the handover proceeds.
    TakeoverBoundary,
}

impl CancelReason {
    /// D36's rule in one place: a takeover cancellation ends the step, not
    /// the job.
    pub fn terminalizes(self) -> bool {
        matches!(self, Self::Run)
    }
}

/// What recovery could prove about the attempt a restart interrupted (§7.6).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PriorAttempt {
    /// Nothing survived — the host restarted, or the session is known to
    /// have ended. A fresh attempt is safe.
    Ended,
    /// No evidence on a host that did not restart: nothing is running and
    /// nothing can start under the retired key, but earlier work may exist.
    /// Never auto-resumed; a person decides.
    Unknown,
}

/// Where a run is. One canonical authority — the Run document — holds it
/// (§7.1); `runs.db` never carries a second one.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunStatus {
    /// The Run document exists; nothing has been asked to start.
    Created,
    /// Admission, materialization and session creation are under way.
    Starting,
    /// The provider is up. `attention` and `control` are the markers §7.5
    /// and §7.4 put on a running run instead of more states; both default so
    /// a header written before either existed still reads.
    Running {
        #[serde(default)]
        attention: AttentionSet,
        #[serde(default)]
        control: ControlState,
    },
    /// A restart ended the attempt (D10). Never `Failed`; offers Resume.
    Interrupted {
        prior_attempt: PriorAttempt,
    },
    /// An orchestrator turn completed the objective with nothing pending.
    Completed,
    Failed {
        stage: FailureStage,
    },
    Cancelled,
    /// Minted for M5 (UC14); no transition reaches it in M1.
    AwaitingReview,
    /// Minted for M5 (UC13); no transition reaches it in M1.
    WaitingForParticipant,
}

impl RunStatus {
    /// A running run with no markers and the orchestrator in control.
    pub fn running() -> Self {
        Self::Running {
            attention: AttentionSet::default(),
            control: ControlState::Orchestrator,
        }
    }

    /// `Completed`, `Failed` or `Cancelled`: nothing moves it again.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled
        )
    }

    /// **Whether something may still be writing into the run's working
    /// folder.**
    ///
    /// The question deleting a run asks, and not the same one as
    /// [`Self::is_terminal`]. `Created` never started and `Interrupted` had
    /// its attempt ended by a restart (D10) — nothing is running in either,
    /// so putting one in the trash is as reversible as putting away a run
    /// that finished. `Starting` and `Running` have a provider that may be
    /// up on this device or another one, and trashing a run that is still
    /// writing is the one case where the reversible operation is not.
    ///
    /// `AwaitingReview` and `WaitingForParticipant` are M5's and answer
    /// *yes* deliberately: no transition reaches them yet, and the
    /// conservative answer is the one a state nobody has defined should
    /// give.
    pub fn may_be_writing(&self) -> bool {
        match self {
            Self::Starting
            | Self::Running { .. }
            | Self::AwaitingReview
            | Self::WaitingForParticipant => true,
            Self::Created | Self::Interrupted { .. } => false,
            other => !other.is_terminal(),
        }
    }

    pub fn attention(&self) -> Option<&AttentionSet> {
        match self {
            Self::Running { attention, .. } => Some(attention),
            _ => None,
        }
    }

    pub fn control(&self) -> Option<ControlState> {
        match self {
            Self::Running { control, .. } => Some(*control),
            _ => None,
        }
    }

    /// The closed vocabulary a status is reported under — an event token,
    /// a metric label, a test failure. Never carries a stage or a marker.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running { .. } => "running",
            Self::Interrupted { .. } => "interrupted",
            Self::Completed => "completed",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
            Self::AwaitingReview => "awaiting_review",
            Self::WaitingForParticipant => "waiting_for_participant",
        }
    }
}

/// What can happen to a run. Provider events arrive already normalized to
/// the run's vocabulary — the originator resolved (§7.5), the cancel reason
/// known (D36), the artifact count taken from the registrations the turn
/// made.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    /// The orchestrator begins the attempt.
    Start,
    /// The provider is up (`TurnStarted` of the first turn, or the session
    /// reporting ready).
    ProviderStarted,
    /// Admission, capture, materialization or the provider failed before a
    /// turn ran.
    StartFailed {
        stage: FailureStage,
    },
    /// Recovery classified the attempt as ended by a restart (§7.6).
    Interrupt {
        prior_attempt: PriorAttempt,
    },
    /// A person asked to continue an interrupted run. A fresh attempt.
    Resume,
    /// A person cancelled the run outside a turn — while it was starting,
    /// interrupted, or running with no turn active.
    Cancel,
    TurnStarted {
        originator: TurnOriginator,
    },
    TurnCompleted {
        originator: TurnOriginator,
        /// Artifacts the turn registered. Zero is D29's case.
        artifacts_registered: u32,
    },
    TurnFailed {
        originator: TurnOriginator,
        stage: FailureStage,
    },
    TurnCancelled {
        originator: TurnOriginator,
        reason: CancelReason,
    },
    /// The Bot owner validated durable AI delivery and the exact native terminal.
    /// Ordinary Run callers must never manufacture this authority.
    BotResponseDelivered,
    ApprovalRequired,
    /// Resolved or expired — both clear the marker.
    ApprovalResolved,
    WaitingForInput,
    ChildSessionDetected,
    ChildSessionTerminated,
    /// A person asked for control (§7.4).
    HandoverRequested,
    /// The human lease was granted, or discovered on recovery.
    ControlTaken,
    /// The orchestrator reacquired the controller.
    ControlReturned,
}

impl RunEvent {
    /// The closed vocabulary an event is reported under.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::ProviderStarted => "provider_started",
            Self::StartFailed { .. } => "start_failed",
            Self::Interrupt { .. } => "interrupt",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
            Self::TurnStarted { .. } => "turn_started",
            Self::TurnCompleted { .. } => "turn_completed",
            Self::TurnFailed { .. } => "turn_failed",
            Self::TurnCancelled { .. } => "turn_cancelled",
            Self::BotResponseDelivered => "bot_response_delivered",
            Self::ApprovalRequired => "approval_required",
            Self::ApprovalResolved => "approval_resolved",
            Self::WaitingForInput => "waiting_for_input",
            Self::ChildSessionDetected => "child_session_detected",
            Self::ChildSessionTerminated => "child_session_terminated",
            Self::HandoverRequested => "handover_requested",
            Self::ControlTaken => "control_taken",
            Self::ControlReturned => "control_returned",
        }
    }
}

/// The pair [`transition`] refused. Carries the state and the event, both of
/// which are closed vocabularies, so it can be logged as it is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IllegalTransition {
    pub from: RunStatus,
    pub event: RunEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "illegal transition: {} cannot accept {}",
            self.from.name(),
            self.event.name()
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The only way from one status to the next.
///
/// Refuses every pair not listed below. The rules, in the order the match
/// applies them:
///
/// - A terminal state accepts nothing: a cancel racing a completion resolves
///   to whichever committed first, and the loser is refused here and
///   recorded by the caller (§7.5).
/// - `AwaitingReview` and `WaitingForParticipant` accept nothing and are
///   produced by nothing.
/// - Only an orchestrator turn may complete, fail or cancel the run; a human
///   turn's outcome is recorded and changes only the markers (§7.5).
/// - An orchestrator turn may not start, complete, fail or be cancelled
///   while a person holds control, and may not start while a handover is
///   pending (§7.4, *automatic submission is fenced from release until
///   return*). A human turn may exist only under human control.
/// - A turn that completes while a handover is pending does not end the run;
///   the completion is fenced and the run completes when control returns
///   (§7.4).
/// - A turn that ends with an open approval is refused: the provider
///   resolves or expires it first (§7.5 lists both events).
/// - A completed orchestrator turn that registered nothing leaves the run
///   `Running` with `FinishedNeedingAttention` (D29).
/// - `CancelReason::TakeoverBoundary` never terminalizes (D36).
pub fn transition(from: &RunStatus, event: &RunEvent) -> Result<RunStatus, IllegalTransition> {
    use RunEvent as E;
    use RunStatus as S;

    let refuse = || {
        Err(IllegalTransition {
            from: from.clone(),
            event: *event,
        })
    };

    match (from, event) {
        // Terminal, and the two states M5 unlocks: nothing moves them.
        (
            S::Completed
            | S::Failed { .. }
            | S::Cancelled
            | S::AwaitingReview
            | S::WaitingForParticipant,
            _,
        ) => refuse(),

        (S::Created, E::Start) => Ok(S::Starting),
        (S::Created | S::Starting, E::StartFailed { stage }) => Ok(S::Failed { stage: *stage }),
        (S::Created | S::Starting | S::Interrupted { .. }, E::Cancel) => Ok(S::Cancelled),
        (S::Starting, E::ProviderStarted) => Ok(S::running()),
        (S::Starting, E::Interrupt { prior_attempt }) => Ok(S::Interrupted {
            prior_attempt: *prior_attempt,
        }),
        (S::Interrupted { .. }, E::Resume) => Ok(S::Starting),
        (S::Created | S::Starting | S::Interrupted { .. }, _) => refuse(),

        (S::Running { attention, control }, event) => running(attention, *control, event)
            .ok_or_else(|| IllegalTransition {
                from: from.clone(),
                event: *event,
            }),
    }
}

/// The `Running` rows of the table. `None` is a refusal.
fn running(attention: &AttentionSet, control: ControlState, event: &RunEvent) -> Option<RunStatus> {
    use ControlState as C;
    use RunEvent as E;
    use TurnOriginator::{Human, Orchestrator};

    let running = |attention: AttentionSet, control: ControlState| {
        Some(RunStatus::Running { attention, control })
    };
    let approval_open = attention.contains(Attention::ApprovalRequired);
    // Ownership belongs to the submitter, even while a person borrows the
    // controller to answer that turn's approval (ADR-389).
    let orchestrator_may_end_a_turn = !approval_open;

    match event {
        E::Interrupt { prior_attempt } => Some(RunStatus::Interrupted {
            prior_attempt: *prior_attempt,
        }),
        E::Cancel => Some(RunStatus::Cancelled),

        E::TurnStarted { originator } => {
            let allowed = match originator {
                // Submission happened earlier; the session handover stream
                // may overtake this known original turn's start notification.
                Orchestrator => true,
                Human => matches!(control, C::Human { .. }),
            };
            if !allowed || approval_open {
                return None;
            }
            running(
                attention
                    .without(Attention::WaitingForInput)
                    .without(Attention::FinishedNeedingAttention),
                control,
            )
        }

        E::TurnCompleted {
            originator: Human, ..
        }
        | E::TurnFailed {
            originator: Human, ..
        } => match control {
            C::Human { .. } if !approval_open => {
                running(attention.without(Attention::WaitingForInput), control)
            }
            _ => None,
        },

        E::TurnCompleted {
            originator: Orchestrator,
            artifacts_registered,
        } => {
            if !orchestrator_may_end_a_turn {
                return None;
            }
            let attention = attention.without(Attention::WaitingForInput);
            match (control, *artifacts_registered) {
                (_, 0) => running(attention.with(Attention::FinishedNeedingAttention), control),
                (C::HandoverPending { .. }, _) => running(
                    attention,
                    C::HandoverPending {
                        objective_satisfied: true,
                    },
                ),
                (C::Orchestrator, _) => Some(RunStatus::Completed),
                (C::Human { .. }, _) => running(
                    attention,
                    C::Human {
                        objective_satisfied: true,
                    },
                ),
            }
        }

        E::TurnFailed {
            originator: Orchestrator,
            stage,
        } => orchestrator_may_end_a_turn.then_some(RunStatus::Failed { stage: *stage }),

        E::TurnCancelled { originator, reason } => match (originator, reason, control) {
            (
                Orchestrator,
                CancelReason::Run,
                C::Orchestrator | C::HandoverPending { .. } | C::Human { .. },
            )
            | (Human, CancelReason::Run, C::Human { .. }) => Some(RunStatus::Cancelled),
            // The step ends; the handover proceeds; a completion already
            // fenced stays fenced.
            (
                Orchestrator,
                CancelReason::TakeoverBoundary,
                C::HandoverPending { .. } | C::Human { .. },
            ) => running(attention.without(Attention::WaitingForInput), control),
            _ => None,
        },

        E::BotResponseDelivered => {
            (control == C::Orchestrator && !approval_open).then_some(RunStatus::Completed)
        }
        E::ApprovalRequired => running(attention.with(Attention::ApprovalRequired), control),
        E::ApprovalResolved => approval_open
            .then(|| running(attention.without(Attention::ApprovalRequired), control))?,
        E::WaitingForInput => running(attention.with(Attention::WaitingForInput), control),
        E::ChildSessionDetected => {
            running(attention.with(Attention::ChildSessionDetected), control)
        }
        E::ChildSessionTerminated => attention
            .contains(Attention::ChildSessionDetected)
            .then(|| running(attention.without(Attention::ChildSessionDetected), control))?,

        E::HandoverRequested => match control {
            C::Orchestrator => running(
                attention.clone(),
                C::HandoverPending {
                    objective_satisfied: false,
                },
            ),
            _ => None,
        },
        E::ControlTaken => match control {
            C::Orchestrator => running(
                attention.clone(),
                C::Human {
                    objective_satisfied: false,
                },
            ),
            C::HandoverPending {
                objective_satisfied,
            } => running(
                attention.clone(),
                C::Human {
                    objective_satisfied,
                },
            ),
            C::Human { .. } => None,
        },
        E::ControlReturned => match control {
            C::Human {
                objective_satisfied: true,
            } if !approval_open => Some(RunStatus::Completed),
            C::Human { .. } => running(attention.clone(), C::Orchestrator),
            _ => None,
        },

        E::Start | E::ProviderStarted | E::StartFailed { .. } | E::Resume => None,
    }
}
