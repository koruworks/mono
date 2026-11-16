// SPDX-License-Identifier: MPL-2.0

//! The delegation modal's state — four fields, one grant, one focus ring.
//!
//! D9's four things (objective, agent, working scope, context) and D28's
//! grant, with **no geometry and no drawing**: this file answers *what is
//! true* and `super::paint` answers *where it is on screen*. That split is
//! what lets every behavioural test in `super::tests` drive the surface
//! without a window, and it is why the focus ring is a `Vec<Hit>` rebuilt on
//! demand rather than a set of rectangles.
//!
//! **Nothing here touches disk, a store or the network.** Opening it, typing
//! into it and moving around it are pure changes to this struct; the one thing
//! that reaches the world is `Outcome::Delegate`, which the container performs
//! on a worker thread (`super::job`).

use koru_delegate_context::ContextSource;
use koru_delegate_core::{RunId, ScheduleId, WorkspaceId, WorkspaceScope};
use koru_draw::text::TextEngine;
use koru_i18n::Key as I18nKey;
use koru_ui::{TextArea, TextField, Typed};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::world::{AgentRow, ContextRow, ScopeRow, World, first_choosable_scope};

pub(crate) mod models;
pub(crate) mod readiness;
#[cfg(test)]
pub(super) mod readiness_tests;
pub(crate) mod repeat;

/// Every control a person can reach, by keyboard or by pointer.
///
/// The indices are into the modal's own lists and are rebuilt from them on
/// every press, so a removed row can never leave focus pointing at nothing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Hit {
    RunOnce,
    Repeat,
    Objective,
    ScheduleName,
    ModelSearch,
    ModelDefault,
    Model(usize),
    ModelPrevious,
    ModelNext,
    ModelRefresh,
    Agent(usize),
    AgentRefresh(usize),
    Scope(usize),
    /// **Change where Koru makes a run's own folder** (ADR-365). It sits on
    /// the temporary row because that is the only row whose folder is Koru's
    /// to place; a project's root is the project's, and is changed where
    /// projects are.
    ScopeBase,
    ContextRemove(usize),
    ContextAdd,
    GrantRemove,
    ScheduleHourly,
    ScheduleDaily,
    ScheduleWeekly,
    ScheduleMonthly,
    ScheduleWeekday(koru_recurrence::Weekday),
    ScheduleTime,
    ScheduleDay,
    ScheduleAdvanced,
    ScheduleFloating,
    ScheduleZoned,
    ScheduleZone,
    Submit,
    /// Retry a refused schedule with its retained request, token, and
    /// `ScheduleId`. Run once never exposes this control because repinning its
    /// live sources would not replay the original create-run body.
    Retry,
    Cancel,
    /// The one button of the finished states.
    Close,
    /// **Go and look at the run that was just made.** Offered only beside
    /// *OK*, because it is the only phase in which there is a run to look
    /// at. The window opens the tab (`MainView::open_run_tab`); a modal
    /// opens nothing.
    ViewRun,
}

/// Where the modal is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    /// Being filled in. `problem` is the sentence a refused *Delegate* left
    /// behind — an empty objective, no working folder.
    Editing { problem: Option<I18nKey> },
    /// The request is out. The window is not waiting on it: the work is on a
    /// thread and this is only what the surface says meanwhile.
    Sending,
    /// The recurring template is being made durable off the window thread.
    /// Kept distinct from `Sending` so the modal never claims an agent run is
    /// already being delegated while it is only creating a schedule.
    Scheduling,
    /// `202 Accepted`. `busy` names the run that already holds this folder,
    /// when one does — D34's warning, which is not a refusal.
    ///
    /// `document` is the **Run document** the gateway registered, which is not
    /// the run's id: the pill points at a document, and this is the only place
    /// a window learns which one.
    Done {
        run: RunId,
        document: koru_core::DocumentId,
        busy: Option<RunId>,
    },
    /// The recurring intent is durable. Deliberately carries no `RunId`: a
    /// schedule is not a first run. A document-origin schedule pill is queued
    /// separately from the retained anchor after this outcome is accepted.
    Scheduled { schedule: ScheduleId },
    /// The gateway said no, with its stable code.
    Refused { code: String },
}

/// What the container should do about a gesture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    /// Nothing the container needs to know about; the surface changed.
    Keep,
    /// Redraw is not needed either.
    Ignored,
    /// Take the surface down.
    Close,
    /// **Open a folder picker.** A modal may not: a native dialog blocks, and
    /// the window performs it off the press the way it performs every other
    /// picker. The answer comes back through
    /// [`Delegation::chose_temporary_base`].
    ChooseFolder,
    /// Retry the selected managed provider on the backend worker.
    RefreshProvider(koru_session_protocol::ProviderKind),
    /// **Take the surface down and open the run's tab.** Which tab strip a
    /// run lands in was never a modal's to decide, the same way opening a
    /// result is the window's (ADR-368); the surface names the run and the
    /// window opens it through `MainView::open_run_tab`, the one door every
    /// other gesture that opens a run already goes through.
    ViewRun(RunId),
    /// **Save a Bot Routine's timing and name** (P4.3). The surface holds no
    /// Bot link: the window hands the three values to the Bot's tab, which
    /// sends one command and answers through
    /// [`Delegation::routine_answered`].
    SaveRoutineTiming {
        schedule: ScheduleId,
        name: koru_delegate_schedule::ScheduleName,
        recurrence: koru_recurrence::RecurrenceRule,
    },
    /// Pin the context and create the run, off the window thread.
    ///
    /// Boxed: every keystroke in the surface returns an `Outcome`, and the one
    /// variant that carries a whole request would otherwise set the size of
    /// all of them.
    Delegate(Box<Request>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Submission {
    RunOnce {
        run: RunId,
    },
    Repeat {
        schedule: ScheduleId,
        name: koru_delegate_schedule::ScheduleName,
        recurrence: koru_recurrence::RecurrenceRule,
    },
    Edit {
        schedule: ScheduleId,
        expected_revision: u32,
        expected_name: koru_delegate_schedule::ScheduleName,
        requested_by: koru_delegate_core::ActorRef,
        name: koru_delegate_schedule::ScheduleName,
        recurrence: koru_recurrence::RecurrenceRule,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct EditSeed {
    pub(crate) schedule: ScheduleId,
    pub(crate) expected_revision: u32,
    pub(crate) expected_name: koru_delegate_schedule::ScheduleName,
    pub(crate) requested_by: koru_delegate_core::ActorRef,
    pub(crate) grants: std::collections::BTreeSet<WorkspaceId>,
}

/// **A Bot Routine's timing and name, edited in this surface** (P4.3,
/// D8). Only the name and the recurrence picker are shown: the task, the
/// agent, the folder and the access are not this dialog's to change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct RoutineTiming {
    pub(crate) bot: koru_ai_core::bots::BotId,
    /// The tab that asked: its Bot link sends the command.
    pub(crate) tab: u64,
    pub(crate) schedule: ScheduleId,
    /// The Routine is on: the new time applies right away, and says so.
    pub(crate) on: bool,
    /// The time zone line, already in the reader's language.
    pub(crate) zone: String,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct RequestToken(uuid::Uuid);

impl RequestToken {
    pub(crate) fn mint() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum RequestTarget {
    Run(RunId),
    Schedule(ScheduleId),
}

/// The complete identity of one press. The token distinguishes separate
/// presses and reopened surfaces (an idempotent schedule retry keeps it); mode
/// plus target prevent a success for one submission kind from being mistaken
/// for the other even under a malformed response.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct RequestCorrelation {
    pub(crate) token: RequestToken,
    pub(crate) mode: repeat::Kind,
    pub(crate) target: RequestTarget,
    pub(crate) edit: bool,
}

impl RequestCorrelation {
    /// A success must name the exact target this press minted. Refusals do not
    /// carry a target in their public failure shape, so their correlation is
    /// established by the stamped answer itself.
    pub(crate) fn accepts_phase(self, phase: &Phase) -> bool {
        match (self.target, phase) {
            (RequestTarget::Run(expected), Phase::Done { run, .. }) => expected == *run,
            (RequestTarget::Schedule(expected), Phase::Scheduled { schedule }) => {
                expected == *schedule
            }
            (_, Phase::Refused { .. }) => true,
            _ => false,
        }
    }
}

/// Exactly what the worker is asked to do — no window, no engine, no view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Request {
    pub(crate) token: RequestToken,
    pub(crate) submission: Submission,
    pub(crate) workspace: WorkspaceId,
    pub(crate) profile: koru_store_core::ProfileId,
    pub(crate) objective: String,
    pub(crate) agent: koru_delegate_core::SessionProfileKey,
    pub(crate) requested_model: Option<koru_delegate_core::ModelName>,
    pub(crate) scope: WorkspaceScope,
    pub(crate) sources: Vec<ContextSource>,
    /// D28: the workspaces the run's tools may reach. Empty when the person
    /// removed the grant — which denies all workspace access, and is the
    /// difference the remove control exists to make.
    pub(crate) grants: std::collections::BTreeSet<WorkspaceId>,
    /// Where the pill goes when this is accepted (D8). Carried through the
    /// worker untouched — it is the window's business, not the request's — so
    /// that an answer arriving after the surface has gone still knows where
    /// the crumb belongs.
    pub(crate) pill_anchor: Option<super::world::PillAnchor>,
}

impl Request {
    pub(crate) fn correlation(&self) -> RequestCorrelation {
        let (mode, target, edit) = match &self.submission {
            Submission::RunOnce { run } => (repeat::Kind::RunOnce, RequestTarget::Run(*run), false),
            Submission::Repeat { schedule, .. } => (
                repeat::Kind::Repeat,
                RequestTarget::Schedule(*schedule),
                false,
            ),
            Submission::Edit { schedule, .. } => (
                repeat::Kind::Repeat,
                RequestTarget::Schedule(*schedule),
                true,
            ),
        };
        RequestCorrelation {
            token: self.token,
            mode,
            target,
            edit,
        }
    }
}

pub(crate) struct Delegation {
    pub(crate) world: World,
    pub(crate) objective: TextArea,
    pub(crate) schedule_name: TextField,
    schedule_name_seeded: bool,
    pub(crate) models: models::ModelPicker,
    pub(crate) agent: Option<usize>,
    pub(crate) scope: Option<usize>,
    pub(crate) context: Vec<ContextRow>,
    /// D28's grant, present until the person removes it. `None` is an empty
    /// grant set, and the run then reaches only what is attached.
    pub(crate) grant: bool,
    pub(crate) kind: repeat::Kind,
    pub(crate) edit: Option<EditSeed>,
    /// Set when the surface edits one Bot Routine's timing and name.
    pub(crate) routine: Option<RoutineTiming>,
    other_grants: std::collections::BTreeSet<WorkspaceId>,
    pub(crate) repeat: repeat::RepeatEditor,
    pub(crate) focus: usize,
    pub(crate) phase: Phase,
    /// The exact press this still-open surface is awaiting. Cleared on an
    /// accepted answer; a newly opened modal therefore accepts no answer from
    /// the surface it replaced.
    pub(crate) in_flight: Option<RequestCorrelation>,
    /// The exact schedule request behind the current attempt. A refusal can
    /// mean the response was lost after the server committed it, so Repeat's
    /// Retry replays this request and its `ScheduleId`. Run once deliberately
    /// has no retry here: retrying its pre-pin sources could capture different
    /// document versions under the same `RunId`.
    pub(crate) retry_request: Option<Request>,
    pub(crate) hover: Option<Hit>,
    pub(crate) dragging: bool,
    pub(crate) scroll: f32,
    pub(crate) scrollbar_drag: Option<f32>,
}

impl Delegation {
    /// **The one constructor.** Every gesture reaches the modal through here,
    /// which is what makes "one code path" a fact rather than a convention.
    ///
    /// It takes no origin, deliberately: everything a gesture knew is already
    /// in `world`, and a surface that also remembered *which* gesture made it
    /// would be a place for the four to start differing.
    #[cfg(test)]
    pub(crate) fn open(engine: &mut TextEngine, world: World) -> Self {
        Self::open_with_environment(
            engine,
            world,
            koru_i18n::time::LocaleFormat::default(),
            "UTC",
        )
    }

    pub(crate) fn open_with_environment(
        engine: &mut TextEngine,
        world: World,
        locale: koru_i18n::time::LocaleFormat,
        current_zone: &str,
    ) -> Self {
        let mut objective = TextArea::new(engine, koru_draw::theme::role::body());
        if !world.objective.is_empty() {
            objective.set_text(engine, &world.objective);
        }
        let context = world.context.clone();
        // **What the gesture asked for, if it is still there.** Only a re-run
        // asks (ADR-366), and only what this machine can still offer is taken:
        // unavailable agents remain visible, but neither they nor a project
        // without a folder can become the selected launch candidate.
        let agent = world
            .chosen_agent
            .as_ref()
            .and_then(|key| {
                world
                    .agents
                    .iter()
                    .position(|row| row.key == *key && row.choosable())
            })
            .or_else(|| world.agents.iter().position(AgentRow::choosable));
        let scope = world
            .chosen_scope
            .as_ref()
            .and_then(|scope| {
                world
                    .scopes
                    .iter()
                    .position(|row| row.scope == *scope && row.choosable())
            })
            .or_else(|| first_choosable_scope(&world.scopes));
        let models = models::ModelPicker::new(engine, world.chosen_model.clone());
        let repeat = repeat::RepeatEditor::new(engine, locale, current_zone);
        let schedule_name = TextField::new(engine, koru_draw::theme::role::ui());
        Self {
            models,
            agent,
            scope,
            context,
            grant: world.grant,
            kind: repeat::Kind::RunOnce,
            edit: None,
            routine: None,
            other_grants: std::collections::BTreeSet::new(),
            repeat,
            schedule_name,
            schedule_name_seeded: false,
            // Keep the caret-ready objective as the initial focus even though
            // the two When chips precede it in visual and tab order.
            focus: 2,
            phase: Phase::Editing { problem: None },
            in_flight: None,
            retry_request: None,
            hover: None,
            dragging: false,
            scroll: 0.0,
            scrollbar_drag: None,
            objective,
            world,
        }
    }

    pub(crate) fn open_edit_with_environment(
        engine: &mut TextEngine,
        world: World,
        locale: koru_i18n::time::LocaleFormat,
        current_zone: &str,
        seed: EditSeed,
        name: &koru_delegate_schedule::ScheduleName,
        recurrence: &koru_recurrence::RecurrenceRule,
    ) -> Self {
        let mut modal = Self::open_with_environment(engine, world, locale, current_zone);
        modal.kind = repeat::Kind::Repeat;
        modal.schedule_name.set_text(engine, name.as_str());
        modal.schedule_name_seeded = true;
        modal.repeat.set_rule(engine, recurrence);
        // Creation falls back to the first runnable choices. Edit instead
        // keeps the schedule's exact choices visible and mutable even when
        // one is temporarily unavailable; saving must not silently replace
        // an existing agent or working folder.
        modal.agent = modal
            .world
            .chosen_agent
            .as_ref()
            .and_then(|chosen| modal.world.agents.iter().position(|row| row.key == *chosen));
        modal.scope = modal.world.chosen_scope.as_ref().and_then(|chosen| {
            modal
                .world
                .scopes
                .iter()
                .position(|row| row.scope == *chosen)
        });
        modal.other_grants = seed
            .grants
            .iter()
            .copied()
            .filter(|workspace| *workspace != modal.world.workspace)
            .collect();
        modal.edit = Some(seed);
        modal.focus_on(Hit::Objective);
        modal
    }

    /// **The timing and name of one Bot Routine**: the same name field and
    /// recurrence picker as a Repeat, and nothing else. Opening it reads
    /// nothing; `name` and `recurrence` are what the Bot's pane last read.
    pub(crate) fn open_routine_timing(
        engine: &mut TextEngine,
        world: World,
        locale: koru_i18n::time::LocaleFormat,
        current_zone: &str,
        timing: RoutineTiming,
        name: &koru_delegate_schedule::ScheduleName,
        recurrence: &koru_recurrence::RecurrenceRule,
    ) -> Self {
        let mut modal = Self::open_with_environment(engine, world, locale, current_zone);
        modal.kind = repeat::Kind::Repeat;
        modal.schedule_name.set_text(engine, name.as_str());
        modal.schedule_name_seeded = true;
        modal.repeat.set_rule(engine, recurrence);
        modal.routine = Some(timing);
        modal.focus_on(Hit::ScheduleName);
        modal
    }

    /// **The Bot's tab answered a Save**, or could not send it: the form
    /// comes back with what the person typed and the sentence for why. A
    /// Save that was applied closes the surface instead (the window does).
    pub(crate) fn routine_answered(&mut self, problem: I18nKey) {
        if self.routine.is_none() {
            return;
        }
        self.phase = Phase::Editing {
            problem: Some(problem),
        };
        self.focus_on(Hit::ScheduleName);
    }

    /// The recurrence picker's controls, in the order they are drawn.
    fn schedule_controls(&self, controls: &mut Vec<Hit>) {
        controls.extend([
            Hit::ScheduleHourly,
            Hit::ScheduleDaily,
            Hit::ScheduleWeekly,
            Hit::ScheduleMonthly,
        ]);
        if self.repeat.frequency == koru_recurrence::RuleShape::Weekly {
            controls.extend(
                koru_recurrence::Weekday::ALL
                    .into_iter()
                    .map(Hit::ScheduleWeekday),
            );
        }
        if self.repeat.frequency != koru_recurrence::RuleShape::Hourly {
            controls.push(Hit::ScheduleTime);
        }
        if self.repeat.frequency == koru_recurrence::RuleShape::Monthly {
            controls.push(Hit::ScheduleDay);
        }
        controls.push(Hit::ScheduleAdvanced);
        controls.extend([Hit::ScheduleFloating, Hit::ScheduleZoned]);
        if self.repeat.basis == repeat::TimeBasis::Zoned {
            controls.push(Hit::ScheduleZone);
        }
    }

    /// Accepted host changes update the open picker without discarding its draft.
    pub(crate) fn replace_agents(&mut self, agents: Vec<AgentRow>) {
        if !matches!(self.phase, Phase::Editing { .. }) {
            return;
        }
        let selected = self
            .agent
            .and_then(|index| self.world.agents.get(index))
            .map(|row| row.key.clone());
        let previous_focus = self.focused();
        let focused = previous_focus.and_then(|hit| match hit {
            Hit::Agent(index) | Hit::AgentRefresh(index) => self
                .world
                .agents
                .get(index)
                .map(|row| (row.key.clone(), matches!(hit, Hit::AgentRefresh(_)))),
            _ => None,
        });
        self.world.agents = agents;
        self.agent = match selected {
            Some(key) => self.world.agents.iter().position(|row| row.key == key),
            None => self.world.agents.iter().position(AgentRow::choosable),
        };
        if let Some((key, refresh)) = focused
            && let Some(index) = self.world.agents.iter().position(|row| row.key == key)
        {
            let hit = if refresh && readiness::retryable(&self.world.agents[index].readiness) {
                Hit::AgentRefresh(index)
            } else {
                Hit::Agent(index)
            };
            self.focus_on(hit);
        } else if let Some(hit) = previous_focus {
            self.focus_on(hit);
        }
        self.clamp_focus();
    }

    pub(crate) fn inspected_agent(&self) -> Option<usize> {
        match self.focused() {
            Some(Hit::Agent(index) | Hit::AgentRefresh(index)) => Some(index),
            _ => self
                .agent
                .or_else(|| (!self.world.agents.is_empty()).then_some(0)),
        }
    }

    /// The grant set the request will carry (D28).
    pub(crate) fn grants(&self) -> std::collections::BTreeSet<WorkspaceId> {
        let mut set = self.other_grants.clone();
        if self.grant {
            set.insert(self.world.workspace);
        }
        set
    }

    /// **The pointer's shape over the surface** — the modal's own answer,
    /// rather than the document's.
    ///
    /// A modal draws a scrim over everything, so nothing underneath may be
    /// asked: without this the container falls through to the tab, and the row
    /// that happens to be behind the *Delegate* button answers `Text`. The
    /// objective is the one field a caret can go into, so it is the one place
    /// the I-beam belongs; every control is `PRESS`, which is the plain arrow
    /// (`koru_ui::cursor` — Koru does not use the hand), and the panel's
    /// margins and the scrim are `IDLE`. Same shape as
    /// `link_device::LinkDevice::cursor` and `feedback`'s, for the same reason.
    pub(crate) fn cursor(&self) -> winit::window::CursorIcon {
        match self.hover {
            Some(
                Hit::Objective
                | Hit::ScheduleName
                | Hit::ModelSearch
                | Hit::ScheduleTime
                | Hit::ScheduleDay
                | Hit::ScheduleAdvanced
                | Hit::ScheduleZone,
            ) => winit::window::CursorIcon::Text,
            Some(_) => koru_ui::cursor::PRESS,
            None => koru_ui::cursor::IDLE,
        }
    }

    /// The open document, when there is one and it is not already attached.
    /// `None` hides the control entirely rather than dimming it: a row that
    /// does nothing is worse than a row that is not there, and the reason it
    /// would be dim ("it is already in the list above") is already on screen.
    pub(crate) fn attachable(&self) -> Option<&ContextRow> {
        let candidate = self.world.attachable.as_ref()?;
        self.context
            .iter()
            .all(|row| row.source != candidate.source)
            .then_some(candidate)
    }

    pub(crate) fn chosen_scope(&self) -> Option<&ScopeRow> {
        self.scope.and_then(|index| self.world.scopes.get(index))
    }

    fn chosen_agent(&self) -> Option<&AgentRow> {
        self.agent.and_then(|index| self.world.agents.get(index))
    }

    /// Creation promises a runnable template now, so every execution choice
    /// must be live and verified. Editing has a different boundary: an
    /// existing schedule remains manageable while its provider, model catalog,
    /// or folder is temporarily unavailable. The firing loop already turns
    /// those conditions into a visible pause instead of running with less.
    fn execution_choices_ready(&self) -> bool {
        let Some(agent) = self.chosen_agent() else {
            return false;
        };
        let Some(scope) = self.chosen_scope() else {
            return false;
        };
        self.edit.is_some()
            || (self.models.selection_valid() && agent.choosable() && scope.choosable())
    }

    /// Whether *Delegate* is offered at all. An objective and a working folder
    /// are the two things a run cannot be started without; the agent list is
    /// empty only when this machine has none, and the modal says so in its own
    /// sentence rather than by dimming a button with no explanation.
    pub(crate) fn ready(&self) -> bool {
        // A Routine's timing needs a valid name and a valid time, and
        // nothing of a run's.
        if self.routine.is_some() {
            return self.repeat.rule().is_some()
                && koru_delegate_schedule::ScheduleName::new(self.schedule_name.value()).is_ok();
        }
        self.execution_choices_ready()
            && !self.objective.value().trim().is_empty()
            && (self.kind == repeat::Kind::RunOnce || self.repeat.rule().is_some())
            && (self.kind == repeat::Kind::RunOnce
                || koru_delegate_schedule::ScheduleName::new(self.schedule_name.value()).is_ok())
    }

    /// **The tab order, rebuilt from the current state.**
    ///
    /// A control that cannot be used is simply absent, so focus can never land
    /// on a dimmed project or on a *Delegate* that would refuse — the same
    /// shape onboarding uses, and for the same reason.
    pub(crate) fn controls(&self) -> Vec<Hit> {
        match self.phase {
            Phase::Editing { .. } if self.routine.is_some() => {
                let mut controls = vec![Hit::ScheduleName];
                self.schedule_controls(&mut controls);
                if self.ready() {
                    controls.push(Hit::Submit);
                }
                controls.push(Hit::Cancel);
                controls
            }
            Phase::Editing { .. } => {
                let mut controls = Vec::new();
                if self.edit.is_none() {
                    controls.extend([Hit::RunOnce, Hit::Repeat]);
                }
                controls.push(Hit::Objective);
                if self.kind == repeat::Kind::Repeat {
                    controls.push(Hit::ScheduleName);
                }
                for (index, row) in self.world.agents.iter().enumerate() {
                    // Unavailable rows remain focusable so their reason is readable.
                    controls.push(Hit::Agent(index));
                    if readiness::retryable(&row.readiness) {
                        controls.push(Hit::AgentRefresh(index));
                    }
                }
                controls.extend(self.models.controls());
                for (index, row) in self.world.scopes.iter().enumerate() {
                    if row.choosable() {
                        controls.push(Hit::Scope(index));
                    }
                    // The control that changes where Koru's own run folders
                    // are made follows the row it belongs to, so Tab walks the
                    // list the way the eye does.
                    if matches!(row.scope, WorkspaceScope::Temporary) {
                        controls.push(Hit::ScopeBase);
                    }
                }
                controls.extend((0..self.context.len()).map(Hit::ContextRemove));
                if self.attachable().is_some() {
                    controls.push(Hit::ContextAdd);
                }
                if self.grant {
                    controls.push(Hit::GrantRemove);
                }
                if self.kind == repeat::Kind::Repeat {
                    self.schedule_controls(&mut controls);
                }
                if self.ready() {
                    controls.push(Hit::Submit);
                }
                controls.push(Hit::Cancel);
                controls
            }
            // Nothing to fill in and nothing to undo: the request is out, or
            // it is finished. One control, so Tab has somewhere honest to go.
            Phase::Sending | Phase::Scheduling => vec![Hit::Cancel],
            // **`Close` first, so focus lands on it.** A finished surface
            // opens on the answer that only shuts the window: a person who
            // presses Enter without reading has dismissed a dialog, not
            // opened a tab. Tab reaches *View run*, and the ring is listed
            // right-to-left the way the editing row's two buttons are.
            Phase::Done { .. } => vec![Hit::Close, Hit::ViewRun],
            Phase::Scheduled { .. } => vec![Hit::Close],
            Phase::Refused { .. } if self.retry_request.is_some() => {
                vec![Hit::Retry, Hit::Close]
            }
            Phase::Refused { .. } => vec![Hit::Close],
        }
    }

    pub(crate) fn focused(&self) -> Option<Hit> {
        self.controls().get(self.focus).copied()
    }

    /// The one field a dictation or a keystroke can land in.
    pub(crate) fn objective_focused(&self) -> bool {
        self.focused() == Some(Hit::Objective)
    }

    pub(crate) fn dictation_field(&mut self) -> &mut TextArea {
        &mut self.objective
    }

    pub(crate) fn recurrence_field_mut(&mut self, hit: Hit) -> Option<&mut koru_ui::TextField> {
        match hit {
            Hit::ScheduleTime => Some(&mut self.repeat.time),
            Hit::ScheduleDay => Some(&mut self.repeat.day),
            Hit::ScheduleAdvanced => Some(&mut self.repeat.advanced),
            Hit::ScheduleZone => Some(&mut self.repeat.zone),
            _ => None,
        }
    }

    pub(crate) fn schedule_name_problem(&self) -> Option<I18nKey> {
        use koru_delegate_core::TextError;
        match koru_delegate_schedule::ScheduleName::new(self.schedule_name.value()) {
            Ok(_) => None,
            Err(TextError::Empty) => Some(I18nKey::DelegateScheduleNameNeeded),
            Err(TextError::TooLong { .. }) => Some(I18nKey::DelegateScheduleNameTooLong),
            Err(TextError::ControlCharacter) => Some(I18nKey::DelegateScheduleNameControl),
        }
    }

    pub(crate) fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        // **Escape always cancels**, from every phase. A request already sent
        // is durable before the gateway answers it (ROADMAP §7.1), so closing
        // the surface never unmakes a run — the container turns a late answer
        // into a toast instead of losing it.
        if matches!(key, Key::Named(NamedKey::Escape)) {
            return Outcome::Close;
        }
        // **Ctrl+Enter delegates**, from anywhere in the surface — including
        // the objective, where a plain Enter is a newline like it is in every
        // other multi-line field in the product. `koru_keys::ShortcutId::Delegate`
        // is the registry's name for it.
        if matches!(key, Key::Named(NamedKey::Enter)) && koru_keys::is_chord(mods) {
            if self.focused() == Some(Hit::ScheduleAdvanced) {
                self.repeat.settle_advanced(engine);
            }
            return self.submit();
        }
        if self.objective_focused()
            && matches!(self.phase, Phase::Editing { .. })
            && self.objective.key(engine, key, text, mods) == Typed::Handled
        {
            // Typing clears a stale complaint: it is about what was true when
            // the button was pressed, not about what is true now.
            self.phase = Phase::Editing { problem: None };
            return Outcome::Keep;
        }
        if self.focused() == Some(Hit::ModelSearch)
            && matches!(self.phase, Phase::Editing { .. })
            && self.models.search.key(engine, key, text, mods) == Typed::Handled
        {
            self.models.filter();
            return Outcome::Keep;
        }
        if self.focused() == Some(Hit::ScheduleName)
            && matches!(self.phase, Phase::Editing { .. })
            && self.schedule_name.key(engine, key, text, mods) == Typed::Handled
        {
            return Outcome::Keep;
        }
        if matches!(self.phase, Phase::Editing { .. }) {
            let focused = self.focused();
            let handled = match focused {
                Some(Hit::ScheduleTime) => self.repeat.time.key(engine, key, text, mods),
                Some(Hit::ScheduleDay) => self.repeat.day.key(engine, key, text, mods),
                Some(Hit::ScheduleAdvanced) => {
                    let handled = self.repeat.advanced.key(engine, key, text, mods);
                    if handled == Typed::Handled {
                        self.repeat.advanced_changed(engine);
                    }
                    handled
                }
                Some(Hit::ScheduleZone) => self.repeat.zone.key(engine, key, text, mods),
                _ => Typed::Passed,
            };
            if handled == Typed::Handled {
                if matches!(focused, Some(Hit::ScheduleTime | Hit::ScheduleDay)) {
                    self.repeat.accept_picker(engine);
                }
                return Outcome::Keep;
            }
        }
        if self.focused() == Some(Hit::ScheduleAdvanced)
            && matches!(
                key,
                Key::Named(NamedKey::Tab | NamedKey::ArrowDown | NamedKey::ArrowUp)
            )
        {
            self.repeat.settle_advanced(engine);
        }
        let controls = self.controls();
        match key {
            Key::Named(NamedKey::Tab) if mods == ModifiersState::SHIFT => {
                self.focus = self
                    .focus
                    .checked_sub(1)
                    .unwrap_or(controls.len().saturating_sub(1));
                Outcome::Keep
            }
            Key::Named(NamedKey::Tab) => {
                self.focus = (self.focus + 1) % controls.len().max(1);
                Outcome::Keep
            }
            Key::Named(NamedKey::ArrowDown) if mods.is_empty() => {
                self.focus = (self.focus + 1) % controls.len().max(1);
                Outcome::Keep
            }
            Key::Named(NamedKey::ArrowUp) if mods.is_empty() => {
                self.focus = self
                    .focus
                    .checked_sub(1)
                    .unwrap_or(controls.len().saturating_sub(1));
                Outcome::Keep
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if mods.is_empty() => {
                match controls.get(self.focus).copied() {
                    Some(hit) => self.activate_with_engine(engine, hit),
                    None => Outcome::Keep,
                }
            }
            // **Everything else is swallowed.** A modal that let keystrokes
            // through would type into the document underneath it.
            _ => Outcome::Ignored,
        }
    }

    /// What a control does — shared by the keyboard and the pointer, so the
    /// two can never drift.
    #[cfg(test)]
    pub(crate) fn activate(&mut self, hit: Hit) -> Outcome {
        self.activate_with_engine(&mut TextEngine::new(), hit)
    }

    pub(crate) fn activate_with_engine(&mut self, engine: &mut TextEngine, hit: Hit) -> Outcome {
        if self.focused() == Some(Hit::ScheduleAdvanced) && hit != Hit::ScheduleAdvanced {
            self.repeat.settle_advanced(engine);
        }
        match hit {
            Hit::RunOnce => {
                if self.edit.is_some() {
                    return Outcome::Ignored;
                }
                self.focus_on(Hit::RunOnce);
                self.kind = repeat::Kind::RunOnce;
                Outcome::Keep
            }
            Hit::Repeat => {
                if self.edit.is_some() {
                    return Outcome::Ignored;
                }
                self.focus_on(Hit::Repeat);
                if !self.schedule_name_seeded {
                    let objective = self.objective.value();
                    let seed =
                        koru_delegate_core::Objective::new(objective)
                            .ok()
                            .map(|objective| {
                                koru_delegate_schedule::ScheduleName::from_objective(&objective)
                            });
                    self.schedule_name
                        .set_text(engine, seed.as_ref().map_or("", |name| name.as_str()));
                    self.schedule_name_seeded = true;
                }
                self.kind = repeat::Kind::Repeat;
                Outcome::Keep
            }
            Hit::Objective => {
                self.focus_on(Hit::Objective);
                Outcome::Keep
            }
            Hit::ScheduleName => {
                self.focus_on(Hit::ScheduleName);
                Outcome::Keep
            }
            Hit::Agent(index) => {
                self.focus_on(Hit::Agent(index));
                if self
                    .world
                    .agents
                    .get(index)
                    .is_some_and(AgentRow::choosable)
                {
                    if self.agent != Some(index) {
                        self.models.selected = None;
                    }
                    self.agent = Some(index);
                }
                Outcome::Keep
            }
            Hit::ModelSearch => {
                self.focus_on(Hit::ModelSearch);
                Outcome::Keep
            }
            Hit::ModelDefault => {
                self.models.selected = None;
                Outcome::Keep
            }
            Hit::Model(index) => {
                if let Some(model) = self
                    .models
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.models().get(index))
                {
                    self.models.selected = Some(
                        koru_delegate_core::ModelName::new(model.id.as_str())
                            .expect("validated model"),
                    );
                }
                Outcome::Keep
            }
            Hit::ModelPrevious => {
                self.models.previous_page();
                Outcome::Keep
            }
            Hit::ModelNext => {
                self.models.next_page();
                Outcome::Keep
            }
            Hit::ModelRefresh => {
                self.models.target = None;
                Outcome::Keep
            }
            Hit::AgentRefresh(index) => {
                let Some(row) = self
                    .world
                    .agents
                    .get_mut(index)
                    .filter(|row| readiness::retryable(&row.readiness))
                else {
                    return Outcome::Ignored;
                };
                let kind = row.provider.clone();
                row.readiness.availability =
                    koru_session_protocol::ExecutableAvailability::Checking;
                row.readiness.failure = None;
                self.focus_on(Hit::Agent(index));
                Outcome::RefreshProvider(kind)
            }
            Hit::Scope(index) => {
                if self
                    .world
                    .scopes
                    .get(index)
                    .is_some_and(ScopeRow::choosable)
                {
                    self.scope = Some(index);
                }
                Outcome::Keep
            }
            Hit::ScopeBase => {
                // A complaint about the last folder is about the last folder;
                // asking for another one clears it.
                self.world.base_problem = None;
                Outcome::ChooseFolder
            }
            Hit::ContextRemove(index) => {
                if index < self.context.len() {
                    self.context.remove(index);
                    self.clamp_focus();
                }
                Outcome::Keep
            }
            Hit::ContextAdd => {
                let Some(candidate) = self.attachable().cloned() else {
                    return Outcome::Ignored;
                };
                self.context.push(candidate);
                self.clamp_focus();
                Outcome::Keep
            }
            Hit::GrantRemove => {
                self.grant = false;
                self.clamp_focus();
                Outcome::Keep
            }
            Hit::ScheduleHourly
            | Hit::ScheduleDaily
            | Hit::ScheduleWeekly
            | Hit::ScheduleMonthly => {
                let shape = match hit {
                    Hit::ScheduleHourly => koru_recurrence::RuleShape::Hourly,
                    Hit::ScheduleDaily => koru_recurrence::RuleShape::Daily,
                    Hit::ScheduleWeekly => koru_recurrence::RuleShape::Weekly,
                    Hit::ScheduleMonthly => koru_recurrence::RuleShape::Monthly,
                    _ => unreachable!(),
                };
                self.focus_on(hit);
                self.repeat.set_frequency(engine, shape);
                Outcome::Keep
            }
            Hit::ScheduleWeekday(day) => {
                self.focus_on(Hit::ScheduleWeekday(day));
                self.repeat.toggle_weekday(engine, day);
                Outcome::Keep
            }
            Hit::ScheduleFloating => {
                self.focus_on(Hit::ScheduleFloating);
                self.repeat.basis = repeat::TimeBasis::Floating;
                Outcome::Keep
            }
            Hit::ScheduleZoned => {
                self.focus_on(Hit::ScheduleZoned);
                self.repeat.basis = repeat::TimeBasis::Zoned;
                Outcome::Keep
            }
            Hit::ScheduleTime | Hit::ScheduleDay | Hit::ScheduleAdvanced | Hit::ScheduleZone => {
                self.focus_on(hit);
                Outcome::Keep
            }
            Hit::Submit => self.submit(),
            Hit::Retry => {
                if !matches!(self.phase, Phase::Refused { .. }) {
                    return Outcome::Ignored;
                }
                let Some(request) = self.retry_request.clone() else {
                    return Outcome::Ignored;
                };
                let correlation = request.correlation();
                if correlation.mode != repeat::Kind::Repeat {
                    return Outcome::Ignored;
                }
                self.kind = repeat::Kind::Repeat;
                self.phase = Phase::Scheduling;
                self.in_flight = Some(correlation);
                self.focus = 0;
                Outcome::Delegate(Box::new(request))
            }
            // The run the gateway answered with, which is the only run this
            // surface has ever known. A phase with no run cannot reach here
            // — the control is not in the ring — and answers nothing rather
            // than guessing one.
            Hit::ViewRun => match &self.phase {
                Phase::Done { run, .. } => Outcome::ViewRun(*run),
                _ => Outcome::Ignored,
            },
            Hit::Cancel | Hit::Close => Outcome::Close,
        }
    }

    /// Put the caret in a named control, when it is in the ring.
    fn focus_on(&mut self, hit: Hit) {
        if let Some(index) = self.controls().iter().position(|control| *control == hit) {
            self.focus = index;
        }
    }

    fn clamp_focus(&mut self) {
        let len = self.controls().len();
        if self.focus >= len {
            self.focus = len.saturating_sub(1);
        }
    }

    /// Try to hand the work over. A refusal here is about this surface's own
    /// state and never about the world — the world's answer arrives later.
    fn submit(&mut self) -> Outcome {
        if !matches!(self.phase, Phase::Editing { .. }) {
            return Outcome::Ignored;
        }
        // **A Routine's timing: one Save is one command**, with the name
        // and the time together. Nothing of a run is read or sent.
        if let Some(routine) = &self.routine {
            let Ok(name) = koru_delegate_schedule::ScheduleName::new(self.schedule_name.value())
            else {
                self.focus_on(Hit::ScheduleName);
                return Outcome::Keep;
            };
            let Some(recurrence) = self.repeat.rule() else {
                self.phase = Phase::Editing {
                    problem: Some(I18nKey::BotsRoutineInvalidTime),
                };
                return Outcome::Keep;
            };
            let schedule = routine.schedule;
            self.phase = Phase::Scheduling;
            self.focus = 0;
            return Outcome::SaveRoutineTiming {
                schedule,
                name,
                recurrence,
            };
        }
        let objective = self.objective.value();
        if objective.trim().is_empty() {
            self.phase = Phase::Editing {
                problem: Some(I18nKey::DelegateObjectiveNeeded),
            };
            self.focus_on(Hit::Objective);
            return Outcome::Keep;
        }
        let Some(scope) = self
            .chosen_scope()
            .filter(|row| self.edit.is_some() || row.choosable())
        else {
            self.phase = Phase::Editing {
                problem: Some(I18nKey::DelegateScopeNeeded),
            };
            return Outcome::Keep;
        };
        let scope = scope.scope.clone();
        let Some(agent) = self
            .chosen_agent()
            .filter(|row| self.edit.is_some() || row.choosable())
            .map(|row| row.key.clone())
        else {
            // No agent on this machine. The empty list already says so above
            // the button; refusing silently would be the worse answer.
            return Outcome::Ignored;
        };
        if self.edit.is_none() && !self.models.selection_valid() {
            return Outcome::Keep;
        }
        let submission = match self.kind {
            repeat::Kind::RunOnce => Submission::RunOnce { run: RunId::mint() },
            repeat::Kind::Repeat => {
                let Ok(name) =
                    koru_delegate_schedule::ScheduleName::new(self.schedule_name.value())
                else {
                    self.focus_on(Hit::ScheduleName);
                    return Outcome::Keep;
                };
                let Some(recurrence) = self.repeat.rule() else {
                    return Outcome::Keep;
                };
                match self.edit.clone() {
                    Some(edit) => Submission::Edit {
                        schedule: edit.schedule,
                        expected_revision: edit.expected_revision,
                        expected_name: edit.expected_name,
                        requested_by: edit.requested_by,
                        name,
                        recurrence,
                    },
                    None => Submission::Repeat {
                        schedule: ScheduleId::mint(),
                        name,
                        recurrence,
                    },
                }
            }
        };
        let request = Request {
            token: RequestToken::mint(),
            requested_model: self.models.selected.clone(),
            submission,
            workspace: self.world.workspace,
            profile: self.world.profile,
            objective,
            agent,
            scope,
            sources: self.context.iter().map(|row| row.source.clone()).collect(),
            grants: self.grants(),
            pill_anchor: if self.edit.is_none() {
                self.world.pill_anchor
            } else {
                None
            },
        };
        self.phase = match self.kind {
            repeat::Kind::RunOnce => Phase::Sending,
            repeat::Kind::Repeat => Phase::Scheduling,
        };
        self.in_flight = Some(request.correlation());
        self.retry_request = (self.kind == repeat::Kind::Repeat).then(|| request.clone());
        self.focus = 0;
        Outcome::Delegate(Box::new(request))
    }

    /// **A folder was chosen, or refused** (ADR-365).
    ///
    /// `Ok` is a base the window has already found writable; the row redraws
    /// with it at once, before the setting has been confirmed, because the
    /// person is looking at the answer to a question they just asked and a row
    /// that waited for a round trip would read as a press that did nothing.
    /// `Err` is the sentence, and the row keeps the base it had.
    pub(crate) fn chose_temporary_base(&mut self, base: Result<String, I18nKey>) {
        match base {
            Ok(base) => {
                let folder = super::world::run_folder(&base);
                self.world.temporary_base = base;
                self.world.base_problem = None;
                for row in &mut self.world.scopes {
                    if matches!(row.scope, WorkspaceScope::Temporary) {
                        row.path = Some(folder.clone());
                    }
                }
            }
            Err(problem) => self.world.base_problem = Some(problem),
        }
    }

    /// Apply a worker answer only when this is still the exact surface and
    /// press that asked for it.
    pub(crate) fn answered_for(&mut self, correlation: RequestCorrelation, phase: Phase) -> bool {
        let phase_matches_mode = matches!(
            (correlation.mode, &self.phase),
            (repeat::Kind::RunOnce, Phase::Sending) | (repeat::Kind::Repeat, Phase::Scheduling)
        );
        if self.in_flight != Some(correlation)
            || self.kind != correlation.mode
            || !phase_matches_mode
            || !correlation.accepts_phase(&phase)
        {
            return false;
        }
        self.in_flight = None;
        if self.edit.is_some()
            && matches!(
                &phase,
                Phase::Refused { code }
                    if matches!(code.as_str(), "schedule_revision_conflict" | "schedule_name_conflict")
            )
        {
            self.retry_request = None;
        }
        if !matches!(&phase, Phase::Refused { .. }) {
            self.retry_request = None;
        }
        self.phase = phase;
        self.focus = 0;
        true
    }

    /// State setup for pure surface tests. Production answers must always use
    /// [`Self::answered_for`] and carry their complete request correlation.
    #[cfg(test)]
    pub(crate) fn answered(&mut self, phase: Phase) {
        self.in_flight = None;
        self.retry_request = None;
        self.phase = phase;
        self.focus = 0;
    }
}
