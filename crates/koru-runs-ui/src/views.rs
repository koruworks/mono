// SPDX-License-Identifier: MPL-2.0

//! **The three views**, each a plain function of one layout pass's inputs.
//!
//! Overview is what the run is; Activity is what it did; Result is what it
//! handed back. They are gathered here rather than on `RunsSurface` so that
//! each is a function of [`Sections`] and nothing else — a view that could
//! reach `&mut self` could reach the link, and reading a run on the window
//! thread is the one thing this crate exists not to do.
//!
//! **What each view refuses to draw is as much the design as what it
//! draws.** On a device that is not executing the run there are no controls
//! at all (D15); a run still working has no rating (D44: there is nothing
//! yet to rate); a run that registered nothing says so rather than
//! pretending to a delivery (D29); and a pin whose state this device could
//! not resolve is drawn with no state rather than as a deletion (D46).

use std::collections::BTreeMap;

use koru_delegate_core::{
    AgentTurnId, ControlState, DeviceId, Rating, RunAttemptId, RunOrigin, StepKind,
};
use koru_draw::icons;
use koru_i18n::time::{DateTimeParts, LocaleFormat};
use koru_i18n::{Key, Lang, fill, number, t, time};

use crate::MAX_HELD;
use crate::action::Action;
use crate::column::{self, Column, Face, Line};
use crate::state::{Availability, PreambleView, Saving, Snapshot, Standing, Takeover};
use crate::words;

/// One layout pass's inputs, gathered so each view is a plain function of
/// them rather than of `&mut self`.
pub(crate) struct Sections<'a> {
    pub(crate) lang: Lang,
    pub(crate) locale: &'a LocaleFormat,
    pub(crate) now: i64,
    pub(crate) snapshot: &'a Snapshot,
    pub(crate) occurrence_local: Option<(i64, DateTimeParts)>,
    pub(crate) takeover: Takeover,
    pub(crate) device_names: &'a BTreeMap<DeviceId, String>,
    pub(crate) availability: Availability,
    pub(crate) note_differs: bool,
    /// Whether this platform can show a folder in a file manager at all.
    /// A control that cannot work is not laid out (`set_can_reveal`).
    pub(crate) can_reveal: bool,
    /// Whether this build lets a person start a delegation; *Re-run* is not
    /// laid out without it (`set_can_re_run`).
    pub(crate) can_re_run: bool,
}

impl Sections<'_> {
    pub(crate) fn device(&self, device: Option<DeviceId>) -> String {
        device
            .and_then(|device| self.device_names.get(&device).cloned())
            .unwrap_or_else(|| t(self.lang, Key::RunDeviceUnknown).to_owned())
    }

    pub(crate) fn when(&self, at_ms: i64) -> String {
        time::relative_long(self.lang, at_ms, self.now)
    }

    pub(crate) fn overview(&self, column: &mut Column<'_>) {
        let lang = self.lang;
        let snapshot = self.snapshot;
        // **The word, and beside it the one thing to do about it** (ADR-366).
        // A run that is over can be asked for again; the offer belongs on the
        // line that says it is over, because that is the moment somebody
        // decides. It is not *Resume*, which continues this run and stands at
        // the bottom of a run that is interrupted -- the two statuses are
        // disjoint, so the two controls never appear together.
        //
        // **Not gated on which device this is**, unlike every other control
        // here: those act on *this* run and need the session this device may
        // not hold. This one makes a new run, which any window that can
        // delegate at all can do — and only such a window (`can_re_run`):
        // a build that hides Delegate shows the run and no way to start one.
        let re_run = snapshot.re_runnable() && self.can_re_run;
        match re_run {
            true => column.line_beside(
                Line::Body,
                &words::status(lang, &snapshot.header.status),
                Action::ReRun,
                t(lang, Key::RunReRun).to_owned(),
                true,
            ),
            false => column.line(Line::Body, &words::status(lang, &snapshot.header.status)),
        }
        if matches!(
            snapshot.header.status,
            koru_delegate_core::RunStatus::Failed { .. }
        ) && let Some(reason) = snapshot.header.failure_reason
        {
            column.wrapped(Line::Meta, &words::failure_reason(lang, reason));
        }
        if re_run {
            column.consequence(t(lang, Key::RunReRunNote));
        }
        for marker in words::attention(lang, &snapshot.header.status) {
            column.wrapped(Line::Meta, &marker);
        }
        if self.availability == Availability::Down {
            column.wrapped(Line::Consequence, t(lang, Key::RunActivityUnavailable));
        }
        match snapshot.standing() {
            Standing::Executing => column.line(Line::Hint, t(lang, Key::RunExecutingHere)),
            Standing::Elsewhere { device } => {
                column.line(
                    Line::Hint,
                    &fill(t(lang, Key::RunExecutingElsewhere), &[&self.device(device)]),
                );
                column.wrapped(Line::Consequence, t(lang, Key::RunReadOnlyElsewhere));
            }
        }

        column.section(t(lang, Key::RunObjectiveHeading));
        column.wrapped(Line::Body, snapshot.header.objective.as_str());
        column.pair(
            t(lang, Key::RunAgentLabel),
            &agent_name(&snapshot.header.agent),
        );
        if let Some(model) = &snapshot.header.requested_model {
            column.line(Line::Meta, t(lang, Key::RunRequestedModelLabel));
            column.wrapped(Line::Body, model.as_str());
        }
        column.pair(
            t(lang, Key::RunStartedLabel),
            &self.when(snapshot.header.created_at_ms),
        );

        column.section(t(lang, Key::RunScopeHeading));
        // **And where that turned out to be.** *A folder Koru made for this
        // run* says what kind of folder it is and not which one, and a person
        // judging work they did not watch has to be able to go and look.
        //
        // The folder's **name**, not the whole path: a path is a wide line of
        // little use beside a control that opens the folder itself. Only the
        // device that drove the run knows it, so the other two answers are
        // said rather than guessed — a folder that has gone, and a device
        // that never resolved one. Neither offers *Open location*, because a
        // control that opens nothing is worse than no control (the same
        // policy `koru_workspace_host::reveal` follows for its menu row).
        let scope = words::scope(lang, &snapshot.header.scope);
        match &snapshot.folder {
            Some(folder) => {
                match folder.present() && self.can_reveal {
                    true => column.line_beside(
                        Line::Body,
                        scope,
                        Action::RevealFolder,
                        t(lang, Key::RunOpenLocation).to_owned(),
                        true,
                    ),
                    false => column.line(Line::Body, scope),
                }
                column.line(Line::Hint, folder.name());
                // A folder that has gone says so. A platform that cannot
                // show one says nothing extra: the folder is there, and
                // *this* is the sentence that would be untrue.
                if !folder.present() {
                    column.wrapped(Line::Consequence, t(lang, Key::RunFolderGone));
                }
            }
            None => {
                column.line(Line::Body, scope);
                column.wrapped(Line::Consequence, t(lang, Key::RunFolderNotHere));
            }
        }

        column.section(t(lang, Key::RunContextHeading));
        if snapshot.header.context.is_empty() {
            column.line(Line::Meta, t(lang, Key::RunContextNone));
        } else {
            let states = &snapshot.context;
            for (index, item) in snapshot.header.context.iter().enumerate() {
                column.line(
                    Line::Body,
                    &words::context(lang, item, states.get(index).copied()),
                );
            }
            column.wrapped(Line::Consequence, t(lang, Key::RunContextPinned));
        }

        self.summary(column);
        self.session(column);
        self.rating(column);
        self.ending(column);
    }

    pub(crate) fn summary(&self, column: &mut Column<'_>) {
        let lang = self.lang;
        let economics = &self.snapshot.header.economics;
        column.section(t(lang, Key::RunSummaryHeading));
        column.pair(
            t(lang, Key::RunDurationLabel),
            &words::duration(lang, economics.elapsed_ms),
        );
        column.pair(
            t(lang, Key::RunTurnsLabel),
            &fill(
                t(lang, Key::RunTurnsBreakdown),
                &[
                    &koru_i18n::number::decimal(lang, u64::from(economics.orchestrator_turns)),
                    &koru_i18n::number::decimal(lang, u64::from(economics.human_turns)),
                ],
            ),
        );
        column.pair(
            t(lang, Key::RunTokensInLabel),
            &words::tokens(lang, economics.input),
        );
        column.pair(
            t(lang, Key::RunTokensOutLabel),
            &words::tokens(lang, economics.output),
        );
        column.pair(
            t(lang, Key::RunTokensCachedLabel),
            &words::tokens(lang, economics.cached_input),
        );
        column.pair(
            t(lang, Key::RunToolCallsLabel),
            &koru_i18n::number::decimal(lang, u64::from(economics.tool_calls)),
        );
        column.pair(
            t(lang, Key::RunInterventionsLabel),
            &koru_i18n::number::decimal(lang, u64::from(economics.interventions)),
        );
        column.pair(
            t(lang, Key::RunModelsLabel),
            &koru_i18n::number::decimal(lang, economics.models.len() as u64),
        );
        if !economics.complete {
            column.wrapped(Line::Consequence, t(lang, Key::RunEconomicsIncomplete));
        }
    }

    /// **§7.4 and the recoverable half of D36.**
    ///
    /// Absent entirely on a device that is not executing the run (D15), and
    /// absent when there is no session to hand over.
    pub(crate) fn session(&self, column: &mut Column<'_>) {
        if !self.snapshot.standing().may_act() {
            return;
        }
        let lang = self.lang;
        let Some(_session) = self.snapshot.session else {
            return;
        };
        column.section(t(lang, Key::RunControlHeading));
        column.line(Line::Meta, words::control(lang, self.snapshot.control()));
        match (self.takeover, self.snapshot.control()) {
            (Takeover::Held, _) | (_, ControlState::Human { .. }) => {
                column.chips([
                    (
                        Action::OpenSession,
                        Face::Chip(t(lang, Key::RunOpenSession).to_owned()),
                        true,
                    ),
                    (
                        Action::GiveBackControl,
                        Face::Chip(t(lang, Key::RunGiveBackControl).to_owned()),
                        true,
                    ),
                ]);
                column.consequence(t(lang, Key::RunGiveBackControlNote));
            }
            (Takeover::Asked | Takeover::Released, _)
            | (_, ControlState::HandoverPending { .. }) => {
                column.wrapped(Line::Meta, t(lang, Key::RunTakeoverPending));
            }
            (Takeover::Idle, ControlState::Orchestrator) => {
                // The recoverable pair. They read differently from each
                // other — one waits, one does not — and both say, in the
                // line under them, that the run keeps going. Neither is
                // the red one, because neither ends anything but a step.
                column.chips([(
                    Action::TakeControl,
                    Face::Chip(t(lang, Key::RunTakeControl).to_owned()),
                    true,
                )]);
                column.consequence(t(lang, Key::RunTakeControlNote));
                column.chips([(
                    Action::TakeControlNow,
                    Face::Chip(t(lang, Key::RunTakeOverNow).to_owned()),
                    true,
                )]);
                column.consequence(t(lang, Key::RunTakeOverNowNote));
            }
        }
    }

    /// D44: one gesture on a finished run, an optional note, never
    /// blocking. Absent elsewhere (D15) and absent while the run is still
    /// working, because there is nothing yet to rate.
    pub(crate) fn rating(&self, column: &mut Column<'_>) {
        if !self.snapshot.standing().may_act() || !self.snapshot.rateable() {
            return;
        }
        let lang = self.lang;
        let current = self.snapshot.header.rating.as_ref().map(|r| r.rating);
        column.section(t(lang, Key::RunRateHeading));
        column.chips([
            (
                Action::Rate(Rating::Good),
                Face::Rating(
                    t(lang, Key::RunRateGood).to_owned(),
                    icons::THUMBS_UP,
                    current == Some(Rating::Good),
                ),
                true,
            ),
            (
                Action::Rate(Rating::NotGood),
                Face::Rating(
                    t(lang, Key::RunRateNotGood).to_owned(),
                    icons::THUMBS_DOWN,
                    current == Some(Rating::NotGood),
                ),
                true,
            ),
        ]);
        if let Some(rating) = current {
            column.line(
                Line::Meta,
                t(
                    lang,
                    match rating {
                        Rating::Good => Key::RunRatedGood,
                        Rating::NotGood => Key::RunRatedNotGood,
                    },
                ),
            );
            column.field(Action::SaveNote);
            column.wrapped(Line::Consequence, t(lang, Key::RunRateNoteStaysHere));
            if self.note_differs {
                column.chips([(
                    Action::SaveNote,
                    Face::Chip(t(lang, Key::RunRateNote).to_owned()),
                    true,
                )]);
            }
        }
    }

    /// **The irreversible half of D36**, on its own, at the bottom, in red,
    /// behind a confirmation — and with the sentence that says how it
    /// differs from the two above it.
    pub(crate) fn ending(&self, column: &mut Column<'_>) {
        if !self.snapshot.standing().may_act() {
            return;
        }
        let lang = self.lang;
        if self.snapshot.resumable() {
            column.gap(column::SECTION_GAP);
            column.chips([(
                Action::Resume,
                Face::Chip(t(lang, Key::RunResume).to_owned()),
                true,
            )]);
            column.consequence(t(lang, Key::RunResumeNote));
        }
        if !self.snapshot.endable() {
            return;
        }
        column.gap(column::SECTION_GAP);
        column.chips([(
            Action::AskToEndRun,
            Face::Danger(t(lang, Key::RunEndRun).to_owned()),
            true,
        )]);
        column.consequence(t(lang, Key::RunEndRunNote));
    }

    /// Newest first, a window at a time, each step from its closed
    /// vocabulary (D45) and each time through `koru_i18n::time`.
    pub(crate) fn activity(&self, column: &mut Column<'_>, held: u32) {
        let lang = self.lang;
        let snapshot = self.snapshot;
        // **UC7's provenance is a property of this run, not an event in its
        // timeline.** It therefore sits ahead of the paginated window and is
        // redrawn from the retained creation origin on every Activity page.
        // The current schedule rule is never consulted: it may have changed
        // since this occurrence was claimed.
        if let RunOrigin::Scheduled {
            schedule,
            occurrence_ms,
            revision,
        } = snapshot.header.origin
            && let Some((converted_ms, parts)) = self.occurrence_local
            && converted_ms == occurrence_ms
        {
            let schedule = schedule.to_storage();
            let occurrence = self.locale.date_time(parts);
            let revision = number::decimal(lang, u64::from(revision));
            column.wrapped(
                Line::Meta,
                &fill(
                    t(lang, Key::RunScheduledProvenance),
                    &[&schedule, &occurrence, &revision],
                ),
            );
            column.gap(column::NOTE_GAP);
        }
        if snapshot.items.is_empty() {
            column.wrapped(Line::Body, t(lang, Key::RunActivityEmpty));
            return;
        }
        // **One usage line per turn, not one per report.** A provider that
        // reports a running total sends one after every request it makes, so
        // a turn with seventeen tool calls appends seventeen amendments of
        // the same fact — the history keeps them all, which is what §17.2
        // asks for, and a person reading the activity wants the number the
        // turn ended on. The newest amendment of each `(attempt, turn)` is
        // what §17.2 says the value *is*, so it is the one drawn; the rest
        // are still there, in the run, for anything that replays them.
        let mut newest_seen: std::collections::HashSet<(RunAttemptId, AgentTurnId)> =
            std::collections::HashSet::new();
        for item in snapshot.items.iter().rev() {
            let when = match item {
                koru_delegate_api::wire::TimelineItem::Step(entry) => Some(entry.at_ms),
                _ => None,
            };
            if let koru_delegate_api::wire::TimelineItem::Usage(fact) = item
                && !newest_seen.insert((fact.attempt, fact.turn))
            {
                continue;
            }
            column.line(Line::Body, &words::item(lang, item));
            // **The row still reads as one sentence; what failed reads
            // under it.** This surface has no disclosure control and does
            // not want one — the consequence line already settled that a
            // detail worth having is drawn rather than hovered for
            // (`column`) — and the detail is bounded by the entry's own
            // type, so a turn of sixty failures is still a handful of
            // lines. A step from before this build carries none, and the
            // row is exactly what it always was.
            if let koru_delegate_api::wire::TimelineItem::Step(entry) = item
                && let StepKind::ToolsCompleted { failures, .. } = entry.kind
            {
                for line in words::tool_failures(lang, &failures) {
                    column.wrapped(Line::Meta, &line);
                }
            }
            if let koru_delegate_api::wire::TimelineItem::Step(entry) = item
                && let StepKind::Failed {
                    reason: Some(reason),
                    ..
                } = entry.kind
            {
                column.wrapped(Line::Meta, &words::failure_reason(lang, reason));
            }
            if let Some(at_ms) = when {
                column.line(Line::Hint, &self.when(at_ms));
            }
            column.gap(column::NOTE_GAP);
        }
        if snapshot.from > 0 && held < MAX_HELD {
            column.chips([(
                Action::ShowEarlier,
                Face::Chip(t(lang, Key::RunActivityMore).to_owned()),
                true,
            )]);
        }
    }

    /// **D29 and D27.** A run that registered nothing reads as finished
    /// needing attention, never as a delivered result; a result this device
    /// does not hold is named with the device that does.
    pub(crate) fn result(&self, column: &mut Column<'_>) {
        let lang = self.lang;
        let snapshot = self.snapshot;
        if snapshot.header.artifacts.is_empty() {
            if snapshot.header.status.is_terminal() || snapshot.finished_needing_attention() {
                column.line(Line::Body, t(lang, Key::RunResultNone));
                column.wrapped(Line::Consequence, t(lang, Key::RunResultNoneHint));
            } else {
                column.wrapped(Line::Body, t(lang, Key::RunResultNotYet));
            }
            return;
        }
        for (index, result) in snapshot.header.artifacts.iter().enumerate() {
            column.line(Line::Body, result.title.as_str());
            column.line(
                Line::Meta,
                &fill(
                    t(lang, Key::RunResultBytes),
                    &[&koru_i18n::number::decimal(lang, result.size_bytes)],
                ),
            );
            let here = Some(result.held_by) == snapshot.header.executing_device()
                && snapshot.standing().may_act();
            column.line(
                Line::Hint,
                &if here {
                    t(lang, Key::RunResultHeldHere).to_owned()
                } else {
                    fill(
                        t(lang, Key::RunResultHeldElsewhere),
                        &[&self.device(Some(result.held_by))],
                    )
                },
            );
            column.line(Line::Hint, &self.when(result.registered_at_ms));
            // **The one the maintainers asked for**: the entry does something. It
            // opens the result as a tab of its own (ADR-368), the way every
            // other document in Koru opens — in Koru, rather than handed to
            // whatever the operating system thinks a `.md` is, and beside
            // the run rather than on top of it.
            column.chips([(
                Action::OpenResult(index),
                Face::Chip(t(lang, Key::RunResultOpen).to_owned()),
                true,
            )]);
            column.gap(column::NOTE_GAP);
        }
    }
}

/// **The fourth view: the instructions a delegated agent starts with.**
///
/// A plain function rather than a method on [`Sections`], because it is the
/// one view that is not about the run at all: it draws whether or not the
/// run has been read, on any device, with no controls of its own.
///
/// **What it must not be mistaken for.** The run's document holds no prose
/// (D45), so there is no transcript of the instructions *this* run was
/// handed and this surface does not pretend there is one. It shows the file
/// as it stands now — the same `resolve` the worker calls at the start of
/// every run — and the first line on the tab says exactly that (ADR-358).
/// Under it: whether the text is the file on this machine or the built-in
/// copy, where the file lives, and, when an edit could not be used, the one
/// sentence saying so.
///
/// **The instructions are a Markdown file, so they are drawn as a document**
/// (ADR-363) — headings as headings, lists as lists — through the same
/// surface a result is drawn through, and saved back through the same
/// exporter. The editor comment at the top of the file is part of what the
/// model receives, so it is shown; it is a note to whoever edits the file
/// rather than an instruction, so it is shown quietly.
///
/// **Saving is offered only when a file is in force.** When Koru's built-in
/// copy is, the tab says so and offers nothing: there is nowhere to write,
/// and a Save that quietly created a file would put a second one into a
/// search whose order is the point.
pub(crate) fn system_prompt(
    column: &mut Column<'_>,
    lang: Lang,
    preamble: Option<&PreambleView>,
    view: &mut koru_markdown_view::MarkdownView,
    saving: &Saving,
) {
    column.wrapped(Line::Consequence, t(lang, Key::RunSystemPromptAsItStands));
    let Some(preamble) = preamble else {
        column.line(Line::Meta, t(lang, Key::RunSystemPromptReading));
        return;
    };
    column.wrapped(
        Line::Meta,
        t(
            lang,
            match preamble.source {
                koru_delegate_preamble::Source::Edited => Key::RunSystemPromptEdited,
                koru_delegate_preamble::Source::Bundled => Key::RunSystemPromptBundled,
            },
        ),
    );
    column.pair(
        t(lang, Key::RunSystemPromptFileLabel),
        koru_delegate_preamble::RELATIVE_PATH,
    );
    if let Some(notice) = preamble.notice {
        column.wrapped(Line::Consequence, &notice.message(lang));
    }
    preamble_controls(column, lang, preamble, view, saving);
    column.gap(column::SECTION_GAP);
    column.markdown(view);
    if view.remaining() > 0 {
        column.chips([(
            Action::ShowMoreMarkdown,
            Face::Chip(koru_markdown_view::text::show_rest(lang, view.remaining())),
            true,
        )]);
    }
}

/// Edit, Save and Stop editing — or the one sentence that says why none of
/// them is here.
fn preamble_controls(
    column: &mut Column<'_>,
    lang: Lang,
    preamble: &PreambleView,
    view: &koru_markdown_view::MarkdownView,
    saving: &Saving,
) {
    if !preamble.savable {
        column.wrapped(Line::Consequence, t(lang, Key::RunSystemPromptNoFileToSave));
        return;
    }
    if !view.is_document() {
        return;
    }
    let sending = matches!(saving, Saving::Sending);
    if view.is_editing() {
        column.chips([
            (
                Action::SavePreamble,
                Face::Chip(t(lang, Key::RunSystemPromptSave).to_owned()),
                view.changed() && !sending,
            ),
            (
                Action::StopEditingPreamble,
                Face::Chip(t(lang, Key::RunSystemPromptStopEditing).to_owned()),
                !sending,
            ),
        ]);
        column.consequence(t(lang, Key::RunSystemPromptSaveNote));
    } else {
        column.chips([(
            Action::EditPreamble,
            Face::Chip(t(lang, Key::RunSystemPromptEdit).to_owned()),
            !sending,
        )]);
    }
    match saving {
        Saving::Saved => column.wrapped(Line::Meta, t(lang, Key::RunSystemPromptSaved)),
        Saving::Failed(failure) => {
            column.wrapped(Line::Consequence, t(lang, Key::RunSystemPromptSaveFailed));
            column.wrapped(Line::Consequence, &failure.message(lang));
        }
        Saving::Idle | Saving::Sending => {}
    }
}

/// The agent as text: a built-in profile's key, or a custom profile's id.
/// A key is a token, not a name — the same thing the phone shows.
fn agent_name(agent: &koru_delegate_core::SessionProfileKey) -> String {
    match agent {
        koru_delegate_core::SessionProfileKey::BuiltIn(key) => key.as_str().to_owned(),
        other => format!("{other:?}"),
    }
}
