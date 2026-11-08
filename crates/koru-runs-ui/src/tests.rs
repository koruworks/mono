// SPDX-License-Identifier: MPL-2.0

//! The run views, driven end to end over a scripted link: no gateway, no
//! socket, no disk.
//!
//! Every test here presses controls **by the word on them, in the language
//! the surface is drawing in** — which is what makes the eight-locale test
//! the same test rather than a second one.

use koru_delegate_api::wire::{RunExecution, RunView, TimelineItem, TimelineWindow};
use koru_delegate_core::{
    ActorRef, ApprovalOutcome, ArtifactTitle, ArtifactVersionId, Attention, AttentionSet, BlobHash,
    BlobKind, CancelReason, ContextState, ControlState, DeviceId, DocumentId, DocumentVersionId,
    EconomicsSummary, FailureStage, Objective, PriorAttempt, ResultReference, ResumeSeam,
    RunContextItem, RunHeader, RunOrigin, RunRating, RunStatus, ScheduleId, SessionProfileKey,
    StepKind, TimelineEntry, TimelineRefs, TokenTotal, ToolFailure, ToolFailures, ToolKind,
    TurnOriginator, WorkspaceScope,
};
use koru_i18n::number::decimal;
use koru_i18n::time::{DateTimeParts, HourCycle, LocaleFormat};
use koru_i18n::{fill, t};
use uuid::Uuid;

use super::*;

// ------------------------------------------------------------------- ids

fn v4(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

fn run_id() -> RunId {
    RunId::from_uuid(v4(1))
}

fn workspace() -> WorkspaceId {
    WorkspaceId::from_uuid(v4(0xa0))
}

fn here() -> DeviceId {
    DeviceId::from_uuid(v4(0xe1))
}

fn there() -> DeviceId {
    DeviceId::from_uuid(v4(0xe2))
}

fn session() -> SessionId {
    v4(0xc1).hyphenated().to_string().parse().unwrap()
}

fn profile() -> ProfileId {
    ProfileId::from_uuid(v4(0xf0))
}

fn schedule_id() -> ScheduleId {
    ScheduleId::from_uuid(v4(0x5c))
}

const OBJECTIVE: &str = "Write the quarterly summary";
const NOW: i64 = 1_700_000_600_000;
const STARTED: i64 = 1_700_000_000_000;

// -------------------------------------------------------------- fixtures

fn header(status: RunStatus) -> RunHeader {
    RunHeader {
        run: run_id(),
        created_at_ms: STARTED,
        requested_by: ActorRef::Person { profile: profile() },
        objective: Objective::new(OBJECTIVE).unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        origin: Default::default(),
        scope: WorkspaceScope::ProjectRoot {
            project: v4(2).hyphenated().to_string().parse().unwrap(),
        },
        context: vec![
            RunContextItem::Document {
                workspace: workspace(),
                document: DocumentId::from_uuid(v4(0xd1)),
                version: DocumentVersionId::from_uuid(v4(0xd2)),
            },
            RunContextItem::Blob {
                workspace: workspace(),
                blob: BlobHash::parse(&format!("b3:{}", "a".repeat(64))).unwrap(),
                blob_kind: BlobKind::Pdf,
            },
        ],
        grants: Default::default(),
        status,
        failure_reason: None,
        environment: koru_delegate_core::ExecutionEnvironment::local(here()),
        artifacts: Vec::new(),
        economics: EconomicsSummary::default(),
        rating: None,
    }
}

const OCCURRENCE: i64 = 1_778_019_660_000;

fn occurrence_parts() -> DateTimeParts {
    DateTimeParts {
        year: 2026,
        month: 5,
        day: 4,
        hour: 21,
        minute: 7,
        second: 0,
    }
}

fn scheduled_header(status: RunStatus) -> RunHeader {
    let mut header = header(status);
    header.origin = RunOrigin::Scheduled {
        schedule: schedule_id(),
        occurrence_ms: OCCURRENCE,
        revision: 12,
    };
    header
}

fn scheduled_fixture(view: RunView) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.read(view);
    fixture
        .surface
        .set_occurrence_local(Some((OCCURRENCE, occurrence_parts())));
    fixture.surface.set_tab(Tab::Activity);
    fixture
}

fn step(kind: StepKind, at_ms: i64) -> TimelineItem {
    TimelineItem::Step(TimelineEntry::new(kind, TimelineRefs::default(), at_ms))
}

/// One turn's usage, at revision zero, for a test that amends it.
fn a_usage_fact(
    attempt: koru_delegate_core::RunAttemptId,
    turn: koru_delegate_core::AgentTurnId,
) -> koru_delegate_core::UsageFact {
    koru_delegate_core::UsageFact {
        identity: koru_delegate_core::UsageFactId::from_uuid(v4(0x11)),
        attempt,
        turn,
        revision: koru_delegate_core::AmendmentRevision::FIRST,
        input: None,
        output: Some(5_062),
        cached_input: Some(675_328),
        model: koru_delegate_core::ModelRef::from_uuid(v4(0x12)),
        completeness: koru_delegate_core::Completeness::Complete,
    }
}

fn view(header: RunHeader, device: DeviceId, session: Option<SessionId>) -> RunView {
    RunView {
        document: DocumentId::from_uuid(v4(0xdd)),
        header,
        execution: RunExecution {
            device,
            session,
            run_workspace: Some("D:/koru/delegate/runs/9f2c/workspace".into()),
        },
        context: vec![ContextState::Live, ContextState::InTrash],
        timeline: TimelineWindow {
            total: 0,
            from: 0,
            items: Vec::new(),
        },
    }
}

/// A surface following a run, with whatever view the caller wrote.
struct Fixture {
    surface: RunsSurface,
    link: Scripted,
}

impl Fixture {
    fn new() -> Self {
        let (links, link) = RunLinks::scripted();
        let mut surface = RunsSurface::with_links(links);
        surface.set_now_ms(NOW);
        surface.set_profile(Some(profile()));
        surface.set_device_name(here(), "This laptop".into());
        surface.set_device_name(there(), "The desk machine".into());
        // The platforms Koru ships on can all show a folder; the one test
        // about a platform that cannot says so itself.
        surface.set_can_reveal(true);
        // A build that can delegate; the one test about a build that hides
        // Delegate says so itself.
        surface.set_can_re_run(true);
        surface.open(run_id(), Some(workspace()));
        Self { surface, link }
    }

    fn read(&mut self, view: RunView) {
        // The folder as a worker that had looked at the disk would report
        // it: present. The tests that care about the other two answers hand
        // in their own.
        let folder = view
            .execution
            .run_workspace
            .as_deref()
            .map(|path| RunFolder::unchecked(path, true));
        self.read_with_folder(view, folder);
    }

    /// The same, with the folder answer written by the caller — a folder
    /// that has gone, or a device that never resolved one. No test looks at
    /// a disk to find out (standing requirement 9).
    fn read_with_folder(&mut self, view: RunView, folder: Option<RunFolder>) {
        self.link.answer(Answer::Read {
            run: run_id(),
            workspace: Some(workspace()),
            view: Box::new(view),
            folder,
        });
        self.surface.poll();
    }

    fn lines(&mut self) -> Vec<String> {
        self.surface.lines()
    }

    fn shows(&mut self, text: &str) -> bool {
        self.lines().iter().any(|line| line == text)
    }

    /// Type into whatever composer holds the keyboard, one character at a
    /// time, exactly as a keyboard does.
    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let typed = ch.to_string();
            self.surface.key(
                &winit::keyboard::Key::Character(typed.clone().into()),
                Some(&typed),
                winit::keyboard::ModifiersState::empty(),
            );
        }
    }
}

/// A run mid-turn on this device, with the orchestrator holding the
/// session — the state the takeover controls exist for.
fn running_here() -> RunView {
    view(header(RunStatus::running()), here(), Some(session()))
}

#[test]
fn responding_refreshes_then_hands_over_at_the_question_without_answering_it() {
    for marker in [Attention::ApprovalRequired, Attention::WaitingForInput] {
        let mut f = Fixture::new();
        let mut waiting = running_here();
        waiting.header.status = RunStatus::Running {
            attention: AttentionSet::new([marker]),
            control: ControlState::Orchestrator,
        };
        f.surface.respond();
        f.surface.respond();
        assert!(
            f.surface.take_requests().is_empty(),
            "wait for the fresh read"
        );
        f.read(waiting.clone());
        assert_eq!(
            f.surface.take_requests(),
            vec![Request::TakeControl {
                session: session(),
                urgency: TakeoverUrgency::AtTurnBoundary,
            }]
        );
        f.read(waiting);
        assert!(
            f.surface.take_requests().is_empty(),
            "one request per press"
        );
        f.surface.set_handover(HandoverState::Released);
        assert_eq!(
            f.surface.take_requests(),
            vec![Request::OpenSession { session: session() }]
        );
        f.surface.set_handover(HandoverState::Released);
        assert!(f.surface.take_requests().is_empty());
    }
}

#[test]
fn responding_to_stale_or_remote_attention_never_takes_control() {
    for case in 0..5 {
        let mut f = Fixture::new();
        let mut current = running_here();
        current.header.status = RunStatus::Running {
            attention: AttentionSet::new([Attention::ApprovalRequired]),
            control: ControlState::Orchestrator,
        };
        match case {
            0 => current.header.status = RunStatus::Completed,
            1 => current.header.status = RunStatus::running(),
            2 => {
                current.header.environment =
                    koru_delegate_core::ExecutionEnvironment::local(there())
            }
            3 => current.execution.session = None,
            _ => {
                f.surface.close();
            }
        }
        f.surface.respond();
        f.read(current);
        assert!(f.surface.take_requests().is_empty(), "case {case}");
    }
}

#[test]
fn responding_when_control_is_held_reopens_the_same_session() {
    let mut f = Fixture::new();
    let mut waiting = running_here();
    waiting.header.status = RunStatus::Running {
        attention: AttentionSet::new([Attention::WaitingForInput]),
        control: ControlState::Human {
            objective_satisfied: false,
        },
    };
    f.surface.respond();
    f.read(waiting);
    assert_eq!(
        f.surface.take_requests(),
        vec![Request::OpenSession { session: session() }]
    );
}

// ------------------------------------------------------------------ UC3

#[test]
fn uc3_activity_alone_says_what_happened() {
    let mut f = Fixture::new();
    let mut running = running_here();
    let steps = [
        step(StepKind::RunCreated, STARTED),
        step(StepKind::Started, STARTED + 1_000),
        step(
            StepKind::TurnStarted {
                originator: TurnOriginator::Orchestrator,
            },
            STARTED + 2_000,
        ),
        step(
            // An entry from before the breakdown existed: a failure count
            // and nothing under it.
            StepKind::ToolsCompleted {
                calls: 12,
                failed: 1,
                failures: ToolFailures::default(),
            },
            STARTED + 3_000,
        ),
        step(StepKind::ApprovalRequired, STARTED + 4_000),
        step(
            StepKind::ApprovalResolved {
                outcome: ApprovalOutcome::Approved,
            },
            STARTED + 5_000,
        ),
        step(
            StepKind::ControlTaken { discovered: false },
            STARTED + 6_000,
        ),
        step(
            StepKind::TurnStarted {
                originator: TurnOriginator::Human,
            },
            STARTED + 7_000,
        ),
        step(StepKind::ControlReturned, STARTED + 8_000),
        step(
            StepKind::TurnCompleted {
                originator: TurnOriginator::Orchestrator,
                artifacts_registered: 1,
            },
            STARTED + 9_000,
        ),
        step(StepKind::Completed, STARTED + 10_000),
    ];
    running.timeline.total = steps.len() as u64;
    running.timeline.items = steps.to_vec();
    f.read(running);
    f.surface.set_tab(Tab::Activity);

    let lines = f.lines();
    let lang = Lang::En;
    // The run's name sits above all three views (ADR-360) — the same line
    // on Overview, Activity and Result, because it is what the run is
    // *called* rather than a piece of any one view's content.
    assert_eq!(
        lines.first().map(String::as_str),
        Some(
            koru_delegate_core::RunName::of(run_id())
                .to_string()
                .as_str()
        )
    );
    // Then the history, newest first, every step a sentence rather than a
    // token.
    assert_eq!(
        lines.get(1).map(String::as_str),
        Some(t(lang, Key::RunStepCompleted))
    );
    for key in [
        Key::RunStepCreated,
        Key::RunStepStarted,
        Key::RunStepTurnStartedAgent,
        Key::RunStepApprovalRequired,
        Key::RunStepApprovalApproved,
        Key::RunStepControlTaken,
        Key::RunStepTurnStartedHuman,
        Key::RunStepControlReturned,
    ] {
        assert!(
            lines.iter().any(|line| line == t(lang, key)),
            "Activity does not say {key:?}"
        );
    }
    // The counts a step carries are in the sentence, not lost.
    assert!(
        lines
            .iter()
            .any(|line| line.contains("12") && line.contains('1')),
        "the tool-call counts are missing: {lines:?}"
    );
    let created = lines
        .iter()
        .position(|line| line == t(lang, Key::RunStepCreated))
        .unwrap();
    let completed = lines
        .iter()
        .position(|line| line == t(lang, Key::RunStepCompleted))
        .unwrap();
    assert!(completed < created, "newest first");
}

#[test]
fn a_gap_is_rendered_as_a_gap_and_an_unknown_step_keeps_its_place() {
    let mut f = Fixture::new();
    let mut running = running_here();
    running.timeline.items = vec![
        step(StepKind::Gap { events_lost: None }, STARTED),
        step(
            StepKind::Gap {
                events_lost: Some(7),
            },
            STARTED + 1,
        ),
        TimelineItem::Unrecognized,
    ];
    running.timeline.total = 3;
    f.read(running);
    f.surface.set_tab(Tab::Activity);
    let lines = f.lines();
    assert!(
        lines
            .iter()
            .any(|line| line == t(Lang::En, Key::RunStepGap))
    );
    assert!(lines.iter().any(|line| line.contains('7')));
    assert!(
        lines
            .iter()
            .any(|line| line == t(Lang::En, Key::RunStepUnknown)),
        "a step this build cannot decode is a row, not a silence"
    );
}

// ------------------------------------------------------ UC7 provenance

#[test]
fn scheduled_activity_names_its_retained_origin_even_when_activity_is_empty() {
    let mut f = scheduled_fixture(view(scheduled_header(RunStatus::Created), here(), None));
    let expected = fill(
        t(Lang::En, Key::RunScheduledProvenance),
        &[
            &schedule_id().to_storage(),
            "05/04/2026 9:07 PM",
            &decimal(Lang::En, 12),
        ],
    );
    let lines = f.lines();
    let provenance = lines.iter().position(|line| line == &expected).unwrap();
    let empty = lines
        .iter()
        .position(|line| line == t(Lang::En, Key::RunActivityEmpty))
        .unwrap();
    assert!(provenance < empty, "the retained origin heads Activity");
    assert_eq!(
        lines.iter().filter(|line| *line == &expected).count(),
        1,
        "provenance is one row, not a synthetic timeline entry"
    );
}

#[test]
fn an_unchanged_borrowed_locale_keeps_the_owned_value_and_layout_fresh() {
    let mut f = scheduled_fixture(view(scheduled_header(RunStatus::Created), here(), None));
    let locale = LocaleFormat::for_system(Some("en-GB"), Some(HourCycle::H24));
    assert!(f.surface.set_locale(&locale));
    f.surface.relayout();
    let stored_tag = f.surface.locale.tag().as_ptr();
    assert!(!f.surface.stale);

    assert!(
        !f.surface.set_locale(&locale),
        "an equal borrowed locale is not cloned into the surface"
    );
    assert_eq!(
        f.surface.locale.tag().as_ptr(),
        stored_tag,
        "the surface kept the same owned locale allocation"
    );
    assert!(
        !f.surface.stale,
        "the equal locale cannot schedule another layout pass"
    );
}

#[test]
fn manual_activity_never_invents_schedule_provenance() {
    let mut f = Fixture::new();
    f.read(view(header(RunStatus::Created), here(), None));
    // A stale host conversion cannot make a manual origin look scheduled.
    f.surface
        .set_occurrence_local(Some((OCCURRENCE, occurrence_parts())));
    f.surface.set_tab(Tab::Activity);
    assert!(f.shows(t(Lang::En, Key::RunActivityEmpty)));
    assert!(
        !f.lines()
            .iter()
            .any(|line| line.contains("Scheduled firing")),
        "manual is a stored origin, not absence to guess through"
    );
}

#[test]
fn provenance_is_outside_the_paginated_window_and_survives_widening_it() {
    let mut run = view(
        scheduled_header(RunStatus::running()),
        here(),
        Some(session()),
    );
    run.timeline.total = 500;
    run.timeline.from = 450;
    run.timeline.items = (0..50)
        .map(|index| step(StepKind::Started, STARTED + index))
        .collect();
    let mut f = scheduled_fixture(run);
    let phrase = t(Lang::En, Key::RunScheduledProvenance)
        .split("{}")
        .next()
        .unwrap();
    assert_eq!(
        f.lines()
            .iter()
            .filter(|line| line.starts_with(phrase))
            .count(),
        1
    );
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(Lang::En, Key::RunActivityMore)));
    assert_eq!(
        f.link.commands(),
        vec![Command::Window {
            entries: WINDOW * 2
        }]
    );
    assert_eq!(
        f.lines()
            .iter()
            .filter(|line| line.starts_with(phrase))
            .count(),
        1,
        "window size changes timeline entries, not run provenance"
    );
}

#[test]
fn provenance_wraps_and_participates_in_the_same_scroll_geometry() {
    let mut f = scheduled_fixture(view(scheduled_header(RunStatus::Created), here(), None));
    f.surface.set_viewport(180.0, 70.0);
    f.surface.relayout();
    let provenance = f
        .surface
        .laid
        .paints
        .iter()
        .find(|paint| paint.text.contains(&schedule_id().to_storage()))
        .expect("the provenance row is laid out");
    let text = provenance.text.clone();
    let style = provenance.style();
    let max_width = provenance
        .max_width
        .expect("a narrow Activity wraps the row instead of clipping it");
    let layout = f
        .surface
        .engine
        .simple_layout_wrapped(&text, style, max_width);
    assert!(
        layout.width() <= max_width,
        "the full stable Schedule id must wrap inside {max_width}, not overflow at {}",
        layout.width()
    );
    assert!(f.surface.content_bottom() > 70.0);
    f.surface
        .scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, -100.0));
    assert!(
        f.surface.scroll > 0.0,
        "the added row uses Activity scrolling"
    );
    assert!(
        f.surface
            .laid
            .paints
            .iter()
            .any(|paint| paint.text.contains(&schedule_id().to_storage())),
        "scrolling changes the transform, not the retained layout row"
    );
}

#[test]
fn scheduled_provenance_uses_the_full_locale_and_all_eight_translations() {
    for lang in Lang::ALL {
        let mut f = scheduled_fixture(view(scheduled_header(RunStatus::Completed), here(), None));
        let locale = LocaleFormat::for_system(Some(lang.tag()), Some(HourCycle::H24));
        f.surface.set_locale(&locale);
        let occurrence = locale.date_time(occurrence_parts());
        let revision = decimal(lang, 12);
        let expected = fill(
            t(lang, Key::RunScheduledProvenance),
            &[&schedule_id().to_storage(), &occurrence, &revision],
        );
        let lines = f.lines();
        assert!(
            lines.contains(&expected),
            "{} lacks {expected:?}",
            lang.tag()
        );
        assert!(
            !expected.contains("{}"),
            "{} left a placeholder",
            lang.tag()
        );
        if lang != Lang::En {
            for english in ["Scheduled firing", "schedule ", "revision "] {
                assert!(
                    !expected.contains(english),
                    "{} leaked English {english:?}: {expected:?}",
                    lang.tag()
                );
            }
        }
    }

    // Region and clock preference are not collapsed into the translated
    // language. The same English copy follows a British 24-hour convention.
    let mut gb = scheduled_fixture(view(scheduled_header(RunStatus::Completed), here(), None));
    gb.surface.set_locale(&LocaleFormat::for_system(
        Some("en-GB"),
        Some(HourCycle::H24),
    ));
    assert!(
        gb.lines()
            .iter()
            .any(|line| line.contains("04/05/2026 21:07")),
        "the occurrence is an absolute reader-local time"
    );
}

#[test]
fn arabic_provenance_is_logical_and_isolates_every_dynamic_value() {
    let mut f = scheduled_fixture(view(scheduled_header(RunStatus::Completed), here(), None));
    let locale = LocaleFormat::for_system(Some("ar"), Some(HourCycle::H24));
    f.surface.set_locale(&locale);
    let line = f
        .lines()
        .into_iter()
        .find(|line| line.contains(&schedule_id().to_storage()))
        .expect("Arabic provenance");
    let values = [
        schedule_id().to_storage(),
        locale.date_time(occurrence_parts()),
        decimal(Lang::Ar, 12),
    ];
    for value in values {
        assert!(
            line.contains(&format!("\u{2068}{value}\u{2069}")),
            "dynamic run must be isolated: {line:?}"
        );
    }
    assert_eq!(line.matches('\u{2068}').count(), 3);
    assert_eq!(line.matches('\u{2069}').count(), 3);
    assert!(line.contains("٠٤/٠٥/٢٠٢٦ ٢١:٠٧"));
    assert!(line.contains("١٢"), "revision digits are localized");
}

// ------------------------------------------------------------------ UC5

#[test]
fn uc5_a_second_device_shows_everything_and_offers_nothing() {
    let mut f = Fixture::new();
    // The run executes on `there`; this device answered, so it is `here`.
    let mut elsewhere = view(header(RunStatus::running()), here(), None);
    elsewhere.header.environment = koru_delegate_core::ExecutionEnvironment::local(there());
    // Which folder it turned out to be is the driving device's to know
    // (`RunExecution::run_workspace`), so this one is handed none — and
    // therefore offers no *Open location* either.
    elsewhere.execution.run_workspace = None;
    elsewhere.timeline.items = vec![step(StepKind::Started, STARTED)];
    elsewhere.timeline.total = 1;
    elsewhere.header.artifacts = vec![ResultReference {
        artifact: ArtifactVersionId::from_uuid(v4(0x90)),
        held_by: there(),
        title: ArtifactTitle::new("The quarterly summary").unwrap(),
        size_bytes: 4_096,
        registered_at_ms: STARTED + 5_000,
    }];
    f.read(elsewhere);

    assert_eq!(
        f.surface.standing(),
        Some(Standing::Elsewhere {
            device: Some(there())
        })
    );
    // The status, the objective and the executing device are all there.
    assert!(f.shows(t(Lang::En, Key::RunStatusRunning)));
    assert!(f.shows(OBJECTIVE));
    assert!(
        f.lines()
            .iter()
            .any(|line| line.contains("The desk machine")),
        "the surface names the device running the work (D16)"
    );
    assert!(f.shows(t(Lang::En, Key::RunReadOnlyElsewhere)));

    // **Absent, not disabled** (D15): there is nothing to press.
    for key in [
        Key::RunTakeControl,
        Key::RunTakeOverNow,
        Key::RunGiveBackControl,
        Key::RunEndRun,
        Key::RunRateGood,
        Key::RunRateNotGood,
    ] {
        let label = t(Lang::En, key);
        assert!(
            !f.surface.has_control(label),
            "{label} is laid out on a device that is not executing the run"
        );
    }
    // Only the four view selectors remain.
    assert_eq!(f.surface.control_labels().len(), Tab::ALL.len());

    // The timeline is there, and D27's result names the device holding it.
    f.surface.set_tab(Tab::Activity);
    assert!(f.shows(t(Lang::En, Key::RunStepStarted)));
    f.surface.set_tab(Tab::Result);
    assert!(f.shows("The quarterly summary"));
    assert!(
        f.lines()
            .iter()
            .any(|line| line.contains("The desk machine")),
        "the content is on the device that holds it (D27)"
    );
}

// ------------------------------------------------------------------ D36

#[test]
fn d36_the_two_takeovers_and_the_ending_never_read_the_same() {
    let mut f = Fixture::new();
    f.read(running_here());
    let lang = Lang::En;

    let take = t(lang, Key::RunTakeControl);
    let now = t(lang, Key::RunTakeOverNow);
    let end = t(lang, Key::RunEndRun);
    assert!(f.surface.has_control(take));
    assert!(f.surface.has_control(now));
    assert!(f.surface.has_control(end));

    // Every one of them says what it would do, in a line of text that is
    // laid out whether or not a pointer is anywhere near it.
    for key in [
        Key::RunTakeControlNote,
        Key::RunTakeOverNowNote,
        Key::RunEndRunNote,
    ] {
        assert!(
            f.shows(t(lang, key)),
            "{key:?} is not on screen before the click"
        );
    }

    // The recoverable pair wear the ordinary face; the irreversible one
    // wears the destructive face, and it is the only one that does.
    f.surface.relayout();
    let danger: Vec<&str> = f
        .surface
        .laid
        .controls
        .iter()
        .filter(|control| matches!(control.face, Face::Danger(_)))
        .map(|control| control.label())
        .collect();
    assert_eq!(danger, vec![end], "exactly one destructive control");

    // The two recoverable ones are separated from the run's ending: the
    // session controls come first, the ending last.
    let labels = f.surface.control_labels();
    let take_at = labels.iter().position(|label| label == take).unwrap();
    let now_at = labels.iter().position(|label| label == now).unwrap();
    let end_at = labels.iter().position(|label| label == end).unwrap();
    assert!(take_at < now_at && now_at < end_at);
}

#[test]
fn only_ending_the_run_asks_a_question_and_it_declares_no_default() {
    let mut f = Fixture::new();
    f.read(running_here());
    let lang = Lang::En;

    // Ending a step goes straight through: it is recoverable, and a
    // confirmation here is what teaches people to click through the one
    // that is not.
    assert!(f.surface.press_label(t(lang, Key::RunTakeOverNow)));
    assert!(!f.surface.is_confirming());
    let _ = f.surface.take_requests();

    // Ending the run stops and asks.
    assert!(f.surface.press_label(t(lang, Key::RunEndRun)));
    assert!(f.surface.is_confirming());
    assert_eq!(
        f.surface.confirmation_default(),
        None,
        "ADR-265 row four: Enter answers nothing until a button is chosen"
    );
    assert!(
        f.link
            .commands()
            .iter()
            .all(|command| *command != Command::EndRun),
        "nothing was ended by opening the question"
    );

    // Cancelling leaves the run alone.
    f.surface
        .key(&WinitKey::Named(NamedKey::Escape), None, Default::default());
    assert!(!f.surface.is_confirming());
    assert!(
        f.link
            .commands()
            .iter()
            .all(|command| *command != Command::EndRun)
    );

    // Confirming ends it, once — and only after a button was deliberately
    // chosen, which is what row four means.
    assert!(f.surface.press_label(t(lang, Key::RunEndRun)));
    f.surface
        .key(&WinitKey::Named(NamedKey::Enter), None, Default::default());
    assert!(
        f.link
            .commands()
            .iter()
            .all(|command| *command != Command::EndRun),
        "Enter answered nothing while no button was focused"
    );
    f.surface
        .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
    f.surface
        .key(&WinitKey::Named(NamedKey::Enter), None, Default::default());
    assert_eq!(
        f.link
            .commands()
            .iter()
            .filter(|command| **command == Command::EndRun)
            .count(),
        1
    );
}

// -------------------------------------------------------- the handover

#[test]
fn taking_the_session_asks_for_it_and_claims_nothing() {
    let mut f = Fixture::new();
    f.read(running_here());
    let lang = Lang::En;

    assert!(f.surface.press_label(t(lang, Key::RunTakeControl)));
    assert_eq!(
        f.surface.take_requests(),
        vec![Request::TakeControl {
            session: session(),
            urgency: TakeoverUrgency::AtTurnBoundary
        }]
    );
    // The surface says it is waiting; it does **not** say the session is
    // hers. §7.4 records a takeover only when the lease is granted, and
    // the orchestrator is what records it.
    assert_eq!(f.surface.takeover(), Takeover::Asked);
    assert!(f.shows(t(lang, Key::RunTakeoverPending)));
    assert!(!f.shows(t(lang, Key::RunControlYours)));
    assert!(
        f.link
            .commands()
            .iter()
            .all(|command| *command != Command::EndRun),
        "asking for the session never ends the run"
    );
}

#[test]
fn ending_the_step_asks_for_the_session_now_and_still_never_ends_the_run() {
    let mut f = Fixture::new();
    f.read(running_here());
    assert!(f.surface.press_label(t(Lang::En, Key::RunTakeOverNow)));
    assert_eq!(
        f.surface.take_requests(),
        vec![Request::TakeControl {
            session: session(),
            urgency: TakeoverUrgency::Now
        }]
    );
    assert!(
        f.link
            .commands()
            .iter()
            .all(|command| *command != Command::EndRun)
    );
}

#[test]
fn the_session_opens_when_the_holder_lets_go_and_goes_back_on_the_word() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.surface.press_label(t(Lang::En, Key::RunTakeControl));
    let _ = f.surface.take_requests();

    // The holder released: the session is this person's to open, and the
    // host is asked to open it exactly once.
    f.surface.set_handover(HandoverState::Released);
    assert_eq!(
        f.surface.take_requests(),
        vec![Request::OpenSession { session: session() }]
    );
    f.surface.set_handover(HandoverState::Released);
    assert!(f.surface.take_requests().is_empty(), "asked once");

    // Granted: the surface now says the session is hers, and offers the
    // way back.
    f.surface.set_handover(HandoverState::Taken);
    assert_eq!(f.surface.takeover(), Takeover::Held);
    assert!(f.shows(t(Lang::En, Key::RunGiveBackControlNote)));
    assert!(f.surface.press_label(t(Lang::En, Key::RunGiveBackControl)));
    assert_eq!(
        f.surface.take_requests(),
        vec![Request::GiveBackControl { session: session() }]
    );
}

#[test]
fn a_run_with_no_session_here_offers_no_handover() {
    let mut f = Fixture::new();
    f.read(view(
        header(RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Ended,
        }),
        here(),
        None,
    ));
    assert!(!f.surface.has_control(t(Lang::En, Key::RunTakeControl)));
    // …but an interrupted run this device executes still offers Resume.
    assert!(f.surface.has_control(t(Lang::En, Key::RunResume)));
    assert!(f.surface.press_label(t(Lang::En, Key::RunResume)));
    assert!(f.link.commands().contains(&Command::Resume));
}

// ------------------------------------------------------------------ D29

#[test]
fn d29_a_run_that_registered_nothing_is_finished_needing_attention_not_a_result() {
    let mut f = Fixture::new();
    let mut view = view(
        header(RunStatus::Running {
            attention: AttentionSet::new([Attention::FinishedNeedingAttention]),
            control: ControlState::Orchestrator,
        }),
        here(),
        Some(session()),
    );
    view.header.artifacts = Vec::new();
    f.read(view);

    // The Overview says what is wrong with it.
    assert!(f.shows(t(Lang::En, Key::RunNeedsAttentionNoResult)));
    // The Result view says it registered nothing — never "delivered".
    f.surface.set_tab(Tab::Result);
    assert!(f.shows(t(Lang::En, Key::RunResultNone)));
    assert!(f.shows(t(Lang::En, Key::RunResultNoneHint)));
    assert!(!f.shows(t(Lang::En, Key::RunResultNotYet)));
    assert!(!f.surface.snapshot().unwrap().delivered());
}

#[test]
fn a_run_still_working_with_no_result_says_not_yet_rather_than_nothing() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.surface.set_tab(Tab::Result);
    assert!(f.shows(t(Lang::En, Key::RunResultNotYet)));
    assert!(!f.shows(t(Lang::En, Key::RunResultNone)));
}

// --------------------------------------------------------------- ratings

fn finished() -> RunView {
    let mut view = view(header(RunStatus::Completed), here(), None);
    view.header.artifacts = vec![ResultReference {
        artifact: ArtifactVersionId::from_uuid(v4(0x90)),
        held_by: here(),
        title: ArtifactTitle::new("The quarterly summary").unwrap(),
        size_bytes: 4_096,
        registered_at_ms: STARTED + 5_000,
    }];
    view
}

#[test]
fn the_rating_is_one_gesture_and_never_blocks_anything() {
    let mut f = Fixture::new();
    f.read(finished());
    let lang = Lang::En;
    assert!(f.surface.has_control(t(lang, Key::RunRateGood)));
    assert!(f.surface.has_control(t(lang, Key::RunRateNotGood)));
    // Unrated is a normal state: the surface says nothing about it.
    assert!(!f.shows(t(lang, Key::RunRatedGood)));

    assert!(f.surface.press_label(t(lang, Key::RunRateGood)));
    let commands = f.link.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(
                command,
                Command::Rate {
                    rating: Rating::Good,
                    note: None,
                    ..
                }
            ))
            .count(),
        1,
        "one gesture, no note, nothing else asked for: {commands:?}"
    );
    // Nothing about the surface is blocked by it.
    assert!(f.surface.press_label(t(lang, Key::RunTabActivity)));
    assert_eq!(f.surface.tab(), Tab::Activity);
}

#[test]
fn the_note_is_optional_and_travels_only_with_the_rating() {
    let mut f = Fixture::new();
    let mut view = finished();
    view.header.rating = Some(RunRating {
        rating: Rating::NotGood,
        note: None,
        rated_by: ActorRef::Person { profile: profile() },
        rated_at_ms: NOW,
    });
    f.read(view);
    assert!(f.shows(t(Lang::En, Key::RunRatedNotGood)));
    assert!(f.shows(t(Lang::En, Key::RunRateNoteStaysHere)));

    // Type a note and save it: the rating goes again, carrying it.
    f.surface.relayout();
    let field = f
        .surface
        .laid
        .controls
        .iter()
        .find(|control| matches!(control.face, Face::Field))
        .expect("the note field is laid out under a rating that exists")
        .rect;
    f.surface.mouse_down(field.x + 4.0, field.y + 4.0);
    for character in "wrong file".chars() {
        let text = character.to_string();
        f.surface.key(
            &WinitKey::Character(text.clone().into()),
            Some(&text),
            Default::default(),
        );
    }
    f.surface
        .key(&WinitKey::Named(NamedKey::Enter), None, Default::default());
    let commands = f.link.commands();
    let sent = commands
        .iter()
        .find_map(|command| match command {
            Command::Rate { rating, note, .. } => Some((*rating, note.clone())),
            _ => None,
        })
        .expect("the note was saved with the rating");
    assert_eq!(sent.0, Rating::NotGood);
    assert_eq!(
        sent.1.map(|note| note.as_str().to_owned()),
        Some("wrong file".into())
    );
}

#[test]
fn a_run_still_working_is_not_rated() {
    let mut f = Fixture::new();
    f.read(running_here());
    assert!(!f.surface.has_control(t(Lang::En, Key::RunRateGood)));
}

// ------------------------------------------------------- the economics

#[test]
fn unavailable_renders_as_unavailable_and_never_as_zero() {
    let mut f = Fixture::new();
    let mut view = finished();
    view.header.economics = EconomicsSummary {
        orchestrator_turns: 3,
        human_turns: 1,
        turns_with_usage: 1,
        // Nothing reported at all.
        input: TokenTotal {
            known: 0,
            unavailable_turns: 3,
        },
        // Some reported, some not: a floor, and it says how many.
        output: TokenTotal {
            known: 1_200,
            unavailable_turns: 2,
        },
        // Every turn reported, and the answer happens to be zero.
        cached_input: TokenTotal {
            known: 0,
            unavailable_turns: 0,
        },
        tool_calls: 9,
        interventions: 2,
        elapsed_ms: Some(3_723_000),
        models: Default::default(),
        complete: false,
    };
    f.read(view);
    let lines = f.lines();
    let unavailable = t(Lang::En, Key::RunUnavailable);
    assert!(
        lines.iter().any(|line| line == unavailable),
        "a total nobody reported reads as unavailable: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("1,200") && line.contains('2')),
        "a partial total says how much is missing: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "0"),
        "a genuine zero is still a zero: {lines:?}"
    );
    // One hour and two minutes of actual work.
    assert!(lines.iter().any(|line| line == "1h 2m"), "{lines:?}");
    assert!(f.shows(t(Lang::En, Key::RunEconomicsIncomplete)));
}

#[test]
fn a_run_that_never_started_has_no_duration_rather_than_a_zero_one() {
    let mut f = Fixture::new();
    f.read(view(header(RunStatus::Created), here(), None));
    assert!(f.shows(t(Lang::En, Key::RunUnavailable)));
}

// ----------------------------------------------------- the pinned context

#[test]
fn a_pin_names_what_it_received_and_what_that_points_at_now() {
    let mut f = Fixture::new();
    f.read(running_here());
    let lines = f.lines();
    let in_trash = t(Lang::En, Key::RunContextStateInTrash);
    assert!(
        lines.iter().any(|line| line.contains(in_trash)),
        "a trashed source is reported as trashed (D46): {lines:?}"
    );
    assert!(f.shows(t(Lang::En, Key::RunContextPinned)));
}

#[test]
fn a_device_that_cannot_tell_says_nothing_rather_than_saying_gone() {
    let mut f = Fixture::new();
    let mut running = running_here();
    running.context = Vec::new();
    f.read(running);
    let unavailable = t(Lang::En, Key::RunContextStateUnavailable);
    assert!(
        !f.lines().iter().any(|line| line.contains(unavailable)),
        "an unresolved pin must not read as a deleted one"
    );
    assert!(f.shows(t(Lang::En, Key::RunContextDocument)));
}

// ------------------------------------------------- deterministic states

#[test]
fn every_state_a_run_can_be_in_draws_and_names_itself() {
    let states = [
        (RunStatus::Created, Key::RunStatusCreated),
        (RunStatus::Starting, Key::RunStatusStarting),
        (RunStatus::running(), Key::RunStatusRunning),
        (
            RunStatus::Running {
                attention: AttentionSet::new([Attention::WaitingForInput]),
                control: ControlState::Orchestrator,
            },
            Key::RunStatusRunning,
        ),
        (
            RunStatus::Interrupted {
                prior_attempt: PriorAttempt::Unknown,
            },
            Key::RunStatusInterrupted,
        ),
        (RunStatus::Completed, Key::RunStatusCompleted),
        (
            RunStatus::Failed {
                stage: FailureStage::ProviderStart,
            },
            Key::RunStatusFailed,
        ),
        (RunStatus::Cancelled, Key::RunStatusCancelled),
    ];
    for (status, key) in states {
        for tab in Tab::ALL {
            let mut f = Fixture::new();
            f.read(view(header(status.clone()), here(), None));
            f.surface.set_tab(tab);
            let lines = f.lines();
            assert!(!lines.is_empty(), "{status:?} on {tab:?} drew nothing");
            if tab == Tab::Overview {
                assert!(
                    lines.iter().any(|line| line.contains(t(Lang::En, key))),
                    "{status:?} does not name itself"
                );
            }
        }
    }
    // The empty surface — nothing read yet — says it is reading rather
    // than saying the run is empty.
    let mut surface = RunsSurface::new();
    assert!(
        surface
            .lines()
            .iter()
            .any(|line| line == t(Lang::En, Key::RunActivityLoading))
    );
}

#[test]
fn a_waiting_run_says_what_it_is_waiting_for() {
    for (marker, key) in [
        (Attention::ApprovalRequired, Key::RunNeedsApproval),
        (Attention::WaitingForInput, Key::RunNeedsInput),
        (Attention::ChildSessionDetected, Key::RunNeedsChildStopped),
    ] {
        let mut f = Fixture::new();
        f.read(view(
            header(RunStatus::Running {
                attention: AttentionSet::new([marker]),
                control: ControlState::Orchestrator,
            }),
            here(),
            Some(session()),
        ));
        assert!(f.shows(t(Lang::En, key)), "{marker:?}");
    }
}

// ---------------------------------------------------------- eight locales

#[test]
fn every_view_of_every_state_draws_in_all_eight_languages() {
    for lang in Lang::ALL {
        for status in [
            RunStatus::running(),
            RunStatus::Completed,
            RunStatus::Cancelled,
            RunStatus::Interrupted {
                prior_attempt: PriorAttempt::Ended,
            },
        ] {
            let mut f = Fixture::new();
            f.surface.set_lang(lang);
            let mut view = view(header(status.clone()), here(), Some(session()));
            view.timeline.items = vec![
                step(StepKind::Started, STARTED),
                step(
                    StepKind::Resumed {
                        via: ResumeSeam::RecordedContext,
                    },
                    STARTED + 1,
                ),
                step(
                    StepKind::TurnCancelled {
                        originator: TurnOriginator::Orchestrator,
                        reason: CancelReason::TakeoverBoundary,
                    },
                    STARTED + 2,
                ),
            ];
            view.timeline.total = 3;
            f.read(view);
            for tab in Tab::ALL {
                f.surface.set_tab(tab);
                let lines = f.lines();
                assert!(!lines.is_empty(), "{} drew nothing on {tab:?}", lang.tag());
                assert!(
                    lines.iter().all(|line| !line.trim().is_empty()),
                    "{} drew an empty line on {tab:?}",
                    lang.tag()
                );
            }
        }
    }
}

#[test]
fn the_two_takeovers_and_the_ending_are_three_different_words_in_every_language() {
    for lang in Lang::ALL {
        let mut f = Fixture::new();
        f.surface.set_lang(lang);
        f.read(running_here());
        let labels = f.surface.control_labels();
        let take = t(lang, Key::RunTakeControl);
        let now = t(lang, Key::RunTakeOverNow);
        let end = t(lang, Key::RunEndRun);
        assert!(labels.iter().any(|label| label == take), "{}", lang.tag());
        assert!(labels.iter().any(|label| label == now), "{}", lang.tag());
        assert!(labels.iter().any(|label| label == end), "{}", lang.tag());
        assert_ne!(take, now, "{}", lang.tag());
        assert_ne!(now, end, "{}", lang.tag());
        assert_ne!(take, end, "{}", lang.tag());
    }
}

// ------------------------------------------------- keyboard reachability

#[test]
fn every_control_is_reachable_from_the_keyboard_and_activates_on_enter() {
    let mut f = Fixture::new();
    f.read(running_here());
    let count = f.surface.control_labels().len();
    assert!(count >= 6);

    // Tab walks the whole list and comes back to where it started.
    let mut seen = Vec::new();
    for _ in 0..count {
        f.surface
            .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
        seen.push(f.surface.focus);
    }
    assert_eq!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        count,
        "Tab reaches every control exactly once before repeating"
    );
    f.surface
        .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
    assert_eq!(f.surface.focus, seen[0]);

    // Shift+Tab walks back.
    let mut mods = winit::keyboard::ModifiersState::empty();
    mods.set(winit::keyboard::ModifiersState::SHIFT, true);
    f.surface.key(&WinitKey::Named(NamedKey::Tab), None, mods);
    assert_eq!(f.surface.focus, seen[count - 1]);

    // Enter presses what is focused: the second control is the Activity
    // tab, and Enter opens it.
    f.surface.focus = None;
    f.surface
        .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
    f.surface
        .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
    f.surface
        .key(&WinitKey::Named(NamedKey::Enter), None, Default::default());
    assert_eq!(f.surface.tab(), Tab::Activity);
}

// ---------------------------------------------------- the off-thread rule

#[test]
fn a_surface_with_no_worker_still_draws_and_asks_for_nothing() {
    // The window thread's whole contract: laying out, hit-testing, drawing
    // and pressing reach no gateway, so a surface whose link is inert
    // behaves exactly like one whose worker has not answered yet.
    let mut surface = RunsSurface::new();
    assert!(!RunLinks::inert().is_live());
    surface.open(run_id(), Some(workspace()));
    surface.set_now_ms(NOW);
    let mut scene = vello::Scene::new();
    surface.draw(&mut scene, vello::kurbo::Affine::IDENTITY);
    surface.mouse_down(5.0, 5.0);
    assert!(surface.take_requests().is_empty());
    assert_eq!(surface.availability(), Availability::Loading);
}

#[test]
fn the_window_of_history_is_widened_by_asking_the_worker_not_by_reading_more() {
    let mut f = Fixture::new();
    let mut running = running_here();
    running.timeline.total = 500;
    running.timeline.from = 450;
    running.timeline.items = (0..50)
        .map(|index| step(StepKind::Started, STARTED + index))
        .collect();
    f.read(running);
    f.surface.set_tab(Tab::Activity);
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(Lang::En, Key::RunActivityMore)));
    assert_eq!(
        f.link.commands(),
        vec![Command::Window {
            entries: WINDOW * 2
        }]
    );
}

// ------------------------------------------------------------- refusals

#[test]
fn a_worker_that_will_not_answer_leaves_the_run_on_screen_and_says_so() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.link.answer(Answer::Reachable(false));
    f.surface.poll();
    assert_eq!(f.surface.availability(), Availability::Down);
    assert!(f.shows(OBJECTIVE), "what was read stays on screen");
    assert!(f.shows(t(Lang::En, Key::RunActivityUnavailable)));
}

#[test]
fn a_refusal_is_a_code_this_surface_can_name() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.link.answer(Answer::Refused {
        operation: Operation::EndRun,
        code: "wrong_status".into(),
    });
    f.surface.poll();
    assert_eq!(
        f.surface.refusal(),
        Some((Operation::EndRun, "wrong_status"))
    );
    assert_eq!(Operation::EndRun.as_str(), "end_run");
}

/// **A turn's usage is one line, whatever the provider sent.**
///
/// The maintainers found this reading a real run: a provider that reports a running
/// total sends one after every request it makes, so a turn with seventeen
/// tool calls left seventeen rows of climbing numbers in the Activity —
/// 87,341 in, then 175,817, then 274,622, and on. Every one of them is the
/// same fact amended (§17.2), the history keeps them all, and the number a
/// person wants is the one the turn ended on.
#[test]
fn a_turns_usage_is_drawn_once_however_many_times_it_was_reported() {
    let mut f = Fixture::new();
    let mut running = running_here();
    let attempt = koru_delegate_core::RunAttemptId::from_uuid(v4(0xa1));
    let turn: koru_delegate_core::AgentTurnId = v4(0xa2).hyphenated().to_string().parse().unwrap();
    let mut items = vec![step(StepKind::RunCreated, STARTED)];
    for (revision, input) in [87_341u64, 175_817, 274_622, 816_002]
        .into_iter()
        .enumerate()
    {
        let mut fact = a_usage_fact(attempt, turn);
        fact.input = Some(input);
        fact.revision = koru_delegate_core::AmendmentRevision(revision as u32);
        items.push(TimelineItem::Usage(fact));
    }
    running.timeline.total = items.len() as u64;
    running.timeline.items = items;
    f.read(running);
    f.surface.set_tab(Tab::Activity);

    let lines = f.lines();
    let finals: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("816,002"))
        .collect();
    assert_eq!(finals.len(), 1, "the turn's final total, once: {lines:?}");
    for stale in ["87,341", "175,817", "274,622"] {
        assert!(
            !lines.iter().any(|line| line.contains(stale)),
            "an amended total is history, not a row: {stale} in {lines:?}"
        );
    }
}

/// **Saying which folder, not only what kind of folder.** The maintainers' words:
/// *"Say where, it is important to be clear of what we do."* The Overview
/// said *A folder Koru made for this run* and stopped, which names a kind and
/// leaves a person unable to go and look.
#[test]
fn the_working_folder_says_which_one_on_the_device_running_it() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.surface.set_tab(Tab::Overview);
    let lines = f.lines();
    assert!(
        lines.iter().any(|line| line.contains("workspace")),
        "the folder the agent is in: {lines:?}"
    );

    // Elsewhere the device driving the run is the only one that knows, so
    // the line is absent rather than guessed.
    let mut f = Fixture::new();
    let mut elsewhere = running_here();
    elsewhere.header.environment = koru_delegate_core::ExecutionEnvironment::local(there());
    elsewhere.execution.run_workspace = None;
    f.read(elsewhere);
    f.surface.set_tab(Tab::Overview);
    assert!(
        !f.lines().iter().any(|line| line.contains("workspace")),
        "a device that is not running it claims no folder"
    );
}

/// **The folder's name, and a way to open it** — the maintainers' *"Show the folder
/// name and a link to 'Open location'"*.
///
/// The name, not the path: a path is a wide line of little use beside a
/// control that opens the folder itself. The reveal is a [`Request`], because
/// this surface starts no processes — the shell owns the one platform seam
/// that shows a path in a file manager.
#[test]
fn the_working_folder_is_named_and_can_be_opened() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.surface.set_tab(Tab::Overview);

    let lines = f.lines();
    assert!(
        lines.iter().any(|line| line == "workspace"),
        "the folder's own name, on its own line: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("D:/koru/delegate/runs")),
        "and not the whole path: {lines:?}"
    );

    assert!(f.surface.has_control(t(Lang::En, Key::RunOpenLocation)));
    assert!(f.surface.press_label(t(Lang::En, Key::RunOpenLocation)));
    assert_eq!(
        f.surface.take_requests(),
        vec![Request::RevealFolder(
            "D:/koru/delegate/runs/9f2c/workspace".into()
        )]
    );
}

/// ...and a folder that is not there is said, not offered.
///
/// Two ways it happens: the folder was moved or deleted after the run ended,
/// and a device that never resolved one at all. Neither may put a control on
/// screen that would open nothing — the same policy the shell's *Show in
/// Explorer* menu row follows, which hides itself rather than dimming.
#[test]
fn a_folder_that_is_not_there_is_said_rather_than_offered() {
    for (folder, sentence) in [
        (
            Some(RunFolder::unchecked(
                "D:/koru/delegate/runs/9f2c/workspace",
                false,
            )),
            Key::RunFolderGone,
        ),
        (None, Key::RunFolderNotHere),
    ] {
        let named = folder.is_some();
        let mut f = Fixture::new();
        f.read_with_folder(running_here(), folder);
        f.surface.set_tab(Tab::Overview);

        assert!(
            !f.surface.has_control(t(Lang::En, Key::RunOpenLocation)),
            "there is nothing to open"
        );
        assert!(f.shows(t(Lang::En, sentence)), "{:?}", f.lines());
        assert_eq!(
            f.lines().iter().any(|line| line == "workspace"),
            named,
            "a folder that was named is still named; one that was not, is not"
        );
        // And the sentence has words in every language.
        for lang in Lang::ALL {
            assert!(!t(lang, sentence).trim().is_empty(), "{lang:?}");
        }
    }
}

/// ...and neither does a platform that has no file manager to show it in.
///
/// `koru_workspace_host::reveal` hides its menu row rather than dimming it
/// where it cannot work, and says why: a row that is there and fails when
/// pressed is the outcome the policy exists to prevent. This surface is
/// chartered not to know which platform it is on, so the host answers — and
/// until it does, nothing is promised. The folder is still named, because
/// naming it was never the part that needed a file manager.
#[test]
fn a_platform_that_cannot_show_a_folder_offers_no_link_to_one() {
    let mut f = Fixture::new();
    f.surface.set_can_reveal(false);
    f.read(running_here());
    f.surface.set_tab(Tab::Overview);

    assert!(!f.surface.has_control(t(Lang::En, Key::RunOpenLocation)));
    assert!(
        f.lines().iter().any(|line| line == "workspace"),
        "the folder is still named: {:?}",
        f.lines()
    );
    assert!(
        !f.shows(t(Lang::En, Key::RunFolderGone)),
        "the folder is there; only the way to show it is missing"
    );
}

/// **A path is the person's own** (§13.7). It is drawn and it is handed to
/// the file manager; it is in no `Debug` this crate can produce, which is
/// the one place a path leaks without anybody deciding to print it.
#[test]
fn a_working_folder_never_reaches_a_debug_line() {
    let path = "D:/Users/someone/Private Projects/tax-2026";
    let folder = RunFolder::unchecked(path, true);
    assert!(!format!("{folder:?}").contains("tax-2026"));
    assert!(format!("{folder:?}").contains("present"));

    let request = Request::RevealFolder(path.into());
    assert!(!format!("{request:?}").contains("tax-2026"));
    assert!(format!("{request:?}").contains("RevealFolder"));
}

// ------------------------------------------------- the instructions (tab 4)

/// A short stand-in for the real file: what matters here is that whatever
/// was resolved is what reaches the screen, unchanged.
const INSTRUCTIONS: &str = "# Working as a delegated agent in Koru\n\
                            \n\
                            ## Put the answer in a file\n\
                            \n\
                            Write it under `.artifacts/<RunId>/` and register it.\n";

fn instructions(source: PreambleSource, notice: Option<PreambleNotice>) -> PreambleView {
    document_instructions(INSTRUCTIONS, source, notice)
}

/// The worker builds the document beside the read, so a fixture does too.
fn document_instructions(
    text: &str,
    source: PreambleSource,
    notice: Option<PreambleNotice>,
) -> PreambleView {
    PreambleView {
        content: koru_markdown_view::Content::Document(Box::new(
            koru_markdown_view::MarkdownDocument::of(text).expect("a document"),
        )),
        source,
        notice,
        savable: source == PreambleSource::Edited,
    }
}

/// A surface showing the tab, with the instructions already answered.
fn showing_instructions(source: PreambleSource, notice: Option<PreambleNotice>) -> Fixture {
    let mut f = Fixture::new();
    f.read(running_here());
    f.surface.set_tab(Tab::SystemPrompt);
    f.link
        .answer(Answer::Preamble(instructions(source, notice)));
    f.surface.poll();
    f
}

#[test]
fn opening_the_tab_asks_the_worker_to_read_the_file_rather_than_reading_it_here() {
    let mut f = Fixture::new();
    f.read(running_here());
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(Lang::En, Key::RunTabSystemPrompt)));
    assert_eq!(f.surface.tab(), Tab::SystemPrompt);
    assert_eq!(f.link.commands(), vec![Command::ReadPreamble]);
    assert!(
        f.surface.preamble().is_none(),
        "nothing until the worker answers"
    );
    assert!(f.shows(t(Lang::En, Key::RunSystemPromptReading)));
}

/// **Nothing on the frame loop reads a file.** Laying out, drawing and
/// hit-testing the tab a hundred times over an inert link — one with no
/// worker at all — leaves the surface with no instructions, because the
/// only path to them is the worker thread.
#[test]
fn no_frame_reads_the_file() {
    let mut surface = RunsSurface::new();
    surface.open(run_id(), Some(workspace()));
    surface.set_tab(Tab::SystemPrompt);
    let mut scene = vello::Scene::new();
    for _ in 0..100 {
        surface.draw(&mut scene, vello::kurbo::Affine::IDENTITY);
        surface.mouse_move(5.0, 5.0);
        let _ = surface.lines();
    }
    assert!(surface.preamble().is_none());
    assert!(surface.take_requests().is_empty());
}

/// Every re-opening asks again, which is what lets the tab claim the text
/// is the one in force *now*.
#[test]
fn every_reopening_reads_the_file_again() {
    let mut f = Fixture::new();
    f.read(running_here());
    let _ = f.link.commands();
    for _ in 0..3 {
        f.surface.set_tab(Tab::SystemPrompt);
        f.surface.set_tab(Tab::Overview);
    }
    assert_eq!(
        f.link.commands(),
        vec![
            Command::ReadPreamble,
            Command::ReadPreamble,
            Command::ReadPreamble
        ]
    );
}

#[test]
fn an_edited_file_is_shown_as_written_and_named_as_the_file_on_this_machine() {
    let mut f = showing_instructions(PreambleSource::Edited, None);
    let lang = Lang::En;
    let lines = f.lines();
    // The one line that stops it being read as a transcript of this run.
    assert!(
        lines
            .iter()
            .any(|line| line == t(lang, Key::RunSystemPromptAsItStands))
    );
    assert!(
        lines
            .iter()
            .any(|line| line == t(lang, Key::RunSystemPromptEdited))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == t(lang, Key::RunSystemPromptBundled))
    );
    // Where the file is, so a person can go and edit it.
    assert!(
        lines
            .iter()
            .any(|line| line == koru_delegate_preamble::RELATIVE_PATH)
    );
    // And the instructions themselves \u2014 **as a document**, which is what
    // changed: a heading is a heading rather than a line beginning with a
    // hash, and a code span is the word rather than the word in backticks.
    assert!(
        lines
            .iter()
            .any(|line| line == "Working as a delegated agent in Koru"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "Put the answer in a file"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains(".artifacts/<RunId>/") && !line.contains('`')),
        "a code span is drawn as code, not as backticks: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with('#')),
        "no line still wears its Markdown: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.trim().is_empty()),
        "a blank line in the file is a gap, never an empty paint"
    );
}

#[test]
fn the_built_in_copy_says_so_rather_than_pretending_to_be_a_file() {
    let mut f = showing_instructions(PreambleSource::Bundled, None);
    let lang = Lang::En;
    assert!(f.shows(t(lang, Key::RunSystemPromptBundled)));
    assert!(!f.shows(t(lang, Key::RunSystemPromptEdited)));
}

/// An edit that could not be used is news on this tab, because this is the
/// tab where a person would otherwise be looking for their own words and
/// not finding them.
#[test]
fn an_edit_that_was_ignored_says_which_fault_it_was() {
    for notice in [
        PreambleNotice::Empty,
        PreambleNotice::Unreadable,
        PreambleNotice::BadEncoding,
        PreambleNotice::Oversized {
            bytes: 100_000,
            limit: 65_536,
        },
    ] {
        let mut f = showing_instructions(PreambleSource::Bundled, Some(notice));
        let lang = Lang::En;
        assert!(
            f.shows(&notice.message(lang)),
            "{notice:?} is not on the tab"
        );
        assert!(f.shows(t(lang, Key::RunSystemPromptBundled)));
    }
}

/// The tab carries the four view selectors, **one** control of its own \u2014
/// Edit \u2014 and nothing about the run: ending it, resuming it and rating it
/// belong to the run and this tab is not about the run.
#[test]
fn the_tab_offers_editing_and_nothing_that_belongs_to_the_run() {
    let mut f = showing_instructions(PreambleSource::Edited, None);
    let lang = Lang::En;
    let mut expected: Vec<String> = Tab::ALL
        .iter()
        .map(|tab| {
            t(
                lang,
                match tab {
                    Tab::Overview => Key::RunTabOverview,
                    Tab::Activity => Key::RunTabActivity,
                    Tab::Result => Key::RunTabResult,
                    Tab::SystemPrompt => Key::RunTabSystemPrompt,
                },
            )
            .to_owned()
        })
        .collect();
    expected.push(t(lang, Key::RunSystemPromptEdit).to_owned());
    assert_eq!(f.surface.control_labels(), expected);
    // The run's own controls are still there one tab over.
    f.surface.set_tab(Tab::Overview);
    assert!(f.surface.has_control(t(lang, Key::RunEndRun)));
}

/// **Saving is offered only when a file is in force.** With the built-in
/// copy there is nowhere to write, so the tab says so and offers nothing \u2014
/// rather than a control that would have to invent a file.
#[test]
fn the_built_in_copy_offers_no_save_and_says_why() {
    let mut f = showing_instructions(PreambleSource::Bundled, None);
    let lang = Lang::En;
    assert!(f.shows(t(lang, Key::RunSystemPromptNoFileToSave)));
    assert!(!f.surface.has_control(t(lang, Key::RunSystemPromptEdit)));
    assert!(!f.surface.has_control(t(lang, Key::RunSystemPromptSave)));
    // And pressing what is not there does nothing.
    let _ = f.link.commands();
    assert!(!f.surface.press_label(t(lang, Key::RunSystemPromptSave)));
    assert_eq!(f.link.commands(), vec![]);
}

/// Editing opens the composer on the document's own Markdown, and Save is
/// dark until something is typed: a Save offered over an untouched file is
/// a Save that rewrites a file for no reason.
#[test]
fn an_edit_opens_the_composer_and_save_waits_for_a_change() {
    let mut f = showing_instructions(PreambleSource::Edited, None);
    let lang = Lang::En;
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(lang, Key::RunSystemPromptEdit)));
    assert!(f.surface.preamble_view().is_editing());
    assert!(f.shows(t(lang, Key::RunSystemPromptSaveNote)));
    assert!(
        f.surface
            .has_control(t(lang, Key::RunSystemPromptStopEditing))
    );
    // Present and refusing, because nothing has been typed.
    assert!(!f.surface.press_label(t(lang, Key::RunSystemPromptSave)));
    assert_eq!(f.link.commands(), vec![]);
    assert!(
        f.surface
            .press_label(t(lang, Key::RunSystemPromptStopEditing))
    );
    assert!(!f.surface.preamble_view().is_editing());
}

/// A save asks the worker; the worker writes the file. **Nothing on this
/// thread touches the disk.**
#[test]
fn saving_asks_the_worker_and_sends_the_document_it_was_drawing() {
    let mut f = showing_instructions(PreambleSource::Edited, None);
    let lang = Lang::En;
    f.surface.press_label(t(lang, Key::RunSystemPromptEdit));
    f.type_text(" and one more line.");
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(lang, Key::RunSystemPromptSave)));
    let commands = f.link.commands();
    assert_eq!(commands.len(), 1);
    let Command::SavePreamble { text } = &commands[0] else {
        panic!("{commands:?}")
    };
    assert!(text.contains("and one more line."), "{text}");
    assert!(matches!(f.surface.saving(), Saving::Sending));
}

/// A save that failed says so, names why, and leaves the instructions
/// exactly as they were.
#[test]
fn a_save_that_failed_says_so_and_changes_nothing() {
    for failure in [
        SaveFailure::NoFile,
        SaveFailure::NotADocument,
        SaveFailure::Write(koru_delegate_preamble::WriteFault::Unwritable),
    ] {
        let mut f = showing_instructions(PreambleSource::Edited, None);
        let lang = Lang::En;
        f.surface.press_label(t(lang, Key::RunSystemPromptEdit));
        f.type_text("x");
        f.surface.press_label(t(lang, Key::RunSystemPromptSave));
        f.link.answer(Answer::PreambleSaved(Box::new(Err(failure))));
        f.surface.poll();
        assert!(
            f.shows(t(lang, Key::RunSystemPromptSaveFailed)),
            "{failure:?}"
        );
        assert!(f.shows(&failure.message(lang)), "{failure:?}");
        assert!(
            f.surface.preamble_view().is_editing(),
            "the edit is still there to try again"
        );
    }
}

/// A save that landed closes the composer and shows what the file now says.
#[test]
fn a_save_that_landed_shows_what_the_next_run_would_read() {
    let mut f = showing_instructions(PreambleSource::Edited, None);
    let lang = Lang::En;
    f.surface.press_label(t(lang, Key::RunSystemPromptEdit));
    f.type_text("x");
    f.surface.press_label(t(lang, Key::RunSystemPromptSave));
    f.link
        .answer(Answer::PreambleSaved(Box::new(Ok(document_instructions(
            "# Mine now\n\nThe new instructions.\n",
            PreambleSource::Edited,
            None,
        )))));
    f.surface.poll();
    assert!(matches!(f.surface.saving(), Saving::Saved));
    assert!(!f.surface.preamble_view().is_editing());
    assert!(f.shows(t(lang, Key::RunSystemPromptSaved)));
    assert!(f.shows("The new instructions."));
}

#[test]
fn the_tab_is_reachable_from_the_keyboard_beside_the_other_three() {
    let mut f = Fixture::new();
    f.read(running_here());
    f.surface.focus = None;
    // Four presses of Tab land on the fourth control, which is this tab.
    for _ in 0..4 {
        f.surface
            .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
    }
    f.surface
        .key(&WinitKey::Named(NamedKey::Enter), None, Default::default());
    assert_eq!(f.surface.tab(), Tab::SystemPrompt);
    // And it scrolls like any other view.
    f.link
        .answer(Answer::Preamble(instructions(PreambleSource::Edited, None)));
    f.surface.poll();
    f.surface.set_viewport(620.0, 60.0);
    let _ = f.surface.lines();
    assert!(
        f.surface.content_bottom() > 60.0,
        "there is more than a screenful to scroll"
    );
}

#[test]
fn the_tab_draws_in_all_eight_languages() {
    for lang in Lang::ALL {
        for (source, notice) in [
            (PreambleSource::Edited, None),
            (PreambleSource::Bundled, None),
            (
                PreambleSource::Bundled,
                Some(PreambleNotice::Oversized {
                    bytes: 100_000,
                    limit: 65_536,
                }),
            ),
        ] {
            let mut f = Fixture::new();
            f.surface.set_lang(lang);
            f.read(running_here());
            f.surface.set_tab(Tab::SystemPrompt);
            f.link
                .answer(Answer::Preamble(instructions(source, notice)));
            f.surface.poll();
            let lines = f.lines();
            assert!(!lines.is_empty(), "{} drew nothing", lang.tag());
            assert!(
                lines.iter().all(|line| !line.trim().is_empty()),
                "{} drew an empty line",
                lang.tag()
            );
            assert!(
                lines
                    .iter()
                    .any(|line| line == t(lang, Key::RunSystemPromptAsItStands)),
                "{} does not say the text is the one in force now",
                lang.tag()
            );
        }
    }
}

/// Rule 7: the instructions are provider payload. They go on the screen and
/// nowhere else — not into a `Debug`, which is what reaches a log.
#[test]
fn the_instructions_never_reach_a_debug_line() {
    let secret = "SECRET-INSTRUCTION-nobody-logs-this";
    let view = document_instructions(
        &format!("# Mine\n\n{secret}\n"),
        PreambleSource::Edited,
        None,
    );
    let printed = format!("{view:?}");
    assert!(!printed.contains(secret), "{printed}");
    assert!(
        printed.contains("Edited") && printed.contains("savable"),
        "{printed}"
    );
    let answer = Answer::Preamble(view);
    assert!(!format!("{answer:?}").contains(secret));
}

// ------------------------------------ which tools failed, on the surface

fn broken_down(pairs: &[(ToolKind, ToolFailure, u32)]) -> ToolFailures {
    let mut failures = ToolFailures::default();
    for (tool, failure, calls) in pairs {
        for _ in 0..*calls {
            failures.record(*tool, *failure);
        }
    }
    failures
}

fn activity_with(kind: StepKind) -> Fixture {
    let mut f = Fixture::new();
    let mut running = running_here();
    running.timeline.items = vec![step(kind, STARTED)];
    running.timeline.total = 1;
    f.read(running);
    f.surface.set_tab(Tab::Activity);
    f
}

/// **The maintainers' question, answered.** The row still reads as one sentence —
/// *six tool calls, two of them failed* — and under it, in the same pass,
/// are the lines that say which tool and what came back.
#[test]
fn the_tool_row_is_still_one_sentence_and_says_underneath_which_tools_failed() {
    let mut f = activity_with(StepKind::ToolsCompleted {
        calls: 6,
        failed: 2,
        failures: broken_down(&[(ToolKind::CommandExecution, ToolFailure::Errored, 2)]),
    });
    let lang = Lang::En;
    let lines = f.lines();
    let sentence = fill(
        t(lang, Key::RunStepToolsWithFailures),
        &[&decimal(lang, 6), &decimal(lang, 2)],
    );
    assert!(lines.contains(&sentence), "{lines:?}");
    let detail = fill(
        t(lang, Key::RunStepToolFailureTimes),
        &[
            t(lang, Key::RunToolCommandExecution),
            t(lang, Key::RunToolFailedErrored),
            &decimal(lang, 2),
        ],
    );
    assert!(lines.contains(&detail), "{lines:?}");
    assert_eq!(detail, "Running a command — reported an error, 2 times");
}

/// **A failure Koru cannot classify is shown as unclassified**, never
/// dropped and never guessed — and so is a tool the adapter did not name.
#[test]
fn an_unclassifiable_failure_reads_as_unclassified_rather_than_vanishing() {
    let mut f = activity_with(StepKind::ToolsCompleted {
        calls: 3,
        failed: 2,
        failures: broken_down(&[
            (ToolKind::McpTool, ToolFailure::Unclassified, 1),
            (ToolKind::Other, ToolFailure::Unclassified, 1),
        ]),
    });
    let lang = Lang::En;
    let lines = f.lines();
    for tool in [Key::RunToolMcp, Key::RunToolOther] {
        let detail = fill(
            t(lang, Key::RunStepToolFailure),
            &[t(lang, tool), t(lang, Key::RunToolFailedUnclassified)],
        );
        assert!(lines.contains(&detail), "{detail} missing from {lines:?}");
    }
}

/// **Every run the maintainers have already made still renders.** An entry written
/// before the breakdown existed carries none, and the row is the count and
/// nothing more — no empty heading, no invented reason.
#[test]
fn a_step_from_before_this_build_draws_the_count_and_claims_nothing_else() {
    let mut f = activity_with(StepKind::ToolsCompleted {
        calls: 6,
        failed: 2,
        failures: ToolFailures::default(),
    });
    let lang = Lang::En;
    let lines = f.lines();
    assert!(lines.contains(&fill(
        t(lang, Key::RunStepToolsWithFailures),
        &[&decimal(lang, 6), &decimal(lang, 2)],
    )));
    for key in [
        Key::RunToolCommandExecution,
        Key::RunToolOther,
        Key::RunToolFailedUnclassified,
        Key::RunToolFailedErrored,
    ] {
        let word = t(lang, key);
        assert!(
            !lines.iter().any(|line| line.contains(word)),
            "{word:?} appeared under a step that recorded nothing: {lines:?}"
        );
    }
}

/// The tail past the entry's bound is drawn as a count, so the lines under
/// the row still add up to the number in it.
#[test]
fn failures_past_the_bound_are_drawn_as_a_count() {
    let mut failures = ToolFailures::default();
    for tool in ToolKind::ALL {
        for failure in ToolFailure::ALL {
            failures.record(tool, failure);
        }
    }
    let mut f = activity_with(StepKind::ToolsCompleted {
        calls: 40,
        failed: failures.total(),
        failures,
    });
    let lang = Lang::En;
    let more = fill(
        t(lang, Key::RunStepToolFailuresMore),
        &[&decimal(lang, u64::from(failures.elided()))],
    );
    assert!(f.lines().contains(&more), "{:?}", f.lines());
}

#[test]
fn what_failed_reads_in_all_eight_languages_and_never_repeats_a_word() {
    for lang in Lang::ALL {
        let mut f = Fixture::new();
        f.surface.set_lang(lang);
        let mut running = running_here();
        running.timeline.items = vec![step(
            StepKind::ToolsCompleted {
                calls: 9,
                failed: 4,
                failures: broken_down(&[
                    (ToolKind::CommandExecution, ToolFailure::Denied, 1),
                    (ToolKind::FileChange, ToolFailure::Cancelled, 1),
                    (ToolKind::WebSearch, ToolFailure::TimedOut, 1),
                    (ToolKind::McpTool, ToolFailure::Unclassified, 1),
                ]),
            },
            STARTED,
        )];
        running.timeline.total = 1;
        f.read(running);
        f.surface.set_tab(Tab::Activity);
        let lines = f.lines();
        assert!(
            lines.iter().all(|line| !line.trim().is_empty()),
            "{} drew an empty line",
            lang.tag()
        );
        for (tool, failure) in [
            (Key::RunToolCommandExecution, Key::RunToolFailedDenied),
            (Key::RunToolFileChange, Key::RunToolFailedCancelled),
            (Key::RunToolWebSearch, Key::RunToolFailedTimedOut),
            (Key::RunToolMcp, Key::RunToolFailedUnclassified),
        ] {
            let detail = fill(
                t(lang, Key::RunStepToolFailure),
                &[t(lang, tool), t(lang, failure)],
            );
            assert!(
                lines.contains(&detail),
                "{} lacks {detail:?} in {lines:?}",
                lang.tag()
            );
        }
        // **Four different failures must read as four different things.**
        // A translation that let two classes collapse would undo the whole
        // point of showing the class at all.
        let reasons: std::collections::BTreeSet<&str> = [
            Key::RunToolFailedDenied,
            Key::RunToolFailedCancelled,
            Key::RunToolFailedTimedOut,
            Key::RunToolFailedErrored,
            Key::RunToolFailedUnclassified,
        ]
        .into_iter()
        .map(|key| t(lang, key))
        .collect();
        assert_eq!(reasons.len(), 5, "{} collapses two reasons", lang.tag());
        let tools: std::collections::BTreeSet<&str> = ToolKind::ALL
            .into_iter()
            .map(|tool| {
                t(
                    lang,
                    match tool {
                        ToolKind::CommandExecution => Key::RunToolCommandExecution,
                        ToolKind::FileChange => Key::RunToolFileChange,
                        ToolKind::McpTool => Key::RunToolMcp,
                        ToolKind::DynamicTool => Key::RunToolDynamic,
                        ToolKind::WebSearch => Key::RunToolWebSearch,
                        ToolKind::ImageGeneration => Key::RunToolImageGeneration,
                        ToolKind::Other => Key::RunToolOther,
                    },
                )
            })
            .collect();
        assert_eq!(tools.len(), 7, "{} collapses two tools", lang.tag());
    }
}
// -------------------------------------- the Result tab opens what it names

/// A finished run with one result on this device, showing the Result tab.
fn showing_results() -> Fixture {
    let mut f = Fixture::new();
    f.read(finished());
    f.surface.set_tab(Tab::Result);
    f
}

/// **What the maintainers asked for, and where it opens now.** The entry does
/// something: it asks the *window* for a tab on the result, and nothing on
/// this thread reads a byte of it (ADR-368).
#[test]
fn pressing_a_result_asks_the_window_for_a_tab_and_reads_nothing_here() {
    let mut f = showing_results();
    let lang = Lang::En;
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(lang, Key::RunResultOpen)));
    assert_eq!(
        f.link.commands(),
        vec![],
        "the run's own worker was not asked for bytes"
    );
    let requests = f.surface.take_requests();
    let Some(Request::OpenResult(opened)) = requests.first() else {
        panic!("{requests:?}");
    };
    assert_eq!(
        opened.version,
        ArtifactVersionId::from_uuid(v4(0x90)),
        "the version the run named"
    );
    assert_eq!(opened.run, run_id());
    assert_eq!(opened.title, "The quarterly summary");
    assert_eq!(opened.size_bytes, 4_096);
    assert_eq!(opened.held_elsewhere, None, "this device holds the bytes");
}

/// **The run pane stays where the person left it.** Opening a result used
/// to replace the whole view with the document and leave a *back* button as
/// the only way out; now the list is still a list, and the entry can be
/// pressed again.
#[test]
fn opening_a_result_leaves_the_result_view_exactly_as_it_was() {
    let mut f = showing_results();
    let lang = Lang::En;
    let before = f.surface.control_labels();
    assert!(f.surface.press_label(t(lang, Key::RunResultOpen)));
    assert_eq!(f.surface.control_labels(), before);
    assert_eq!(f.surface.tab(), Tab::Result);
    assert!(f.surface.has_control(t(lang, Key::RunResultOpen)));
}

/// **D27.** A result whose bytes are on another device travels with the
/// name of the device that holds them, so the tab says so without asking a
/// gateway a question it would have to refuse.
#[test]
fn a_result_held_elsewhere_names_the_device_without_asking() {
    let mut f = Fixture::new();
    let mut view = finished();
    view.header.artifacts[0].held_by = there();
    f.read(view);
    f.surface.set_tab(Tab::Result);
    let lang = Lang::En;
    let _ = f.link.commands();
    assert!(f.surface.press_label(t(lang, Key::RunResultOpen)));
    assert_eq!(f.link.commands(), vec![], "nothing was asked for");
    let requests = f.surface.take_requests();
    let Some(Request::OpenResult(opened)) = requests.first() else {
        panic!("{requests:?}");
    };
    assert_eq!(opened.held_elsewhere.as_deref(), Some("The desk machine"));
}

/// **Standing requirement 7.** A result's title is somebody's writing and
/// never reaches a log; what a reader of the request needs is the ids, the
/// size and *whether* the bytes are elsewhere.
#[test]
fn a_result_request_prints_no_title() {
    let mut f = showing_results();
    f.surface.press_label(t(Lang::En, Key::RunResultOpen));
    let printed = format!("{:?}", f.surface.take_requests());
    assert!(!printed.contains("quarterly"), "{printed}");
    assert!(printed.contains("size_bytes"), "{printed}");
    assert!(printed.contains("elsewhere: false"), "{printed}");
}

/// Every new word on both tabs exists in all eight languages, and every one
/// of them is drawn.
#[test]
fn the_document_vocabulary_exists_in_every_language() {
    for lang in Lang::ALL {
        let mut f = showing_results();
        f.surface.set_lang(lang);
        assert!(
            f.surface.has_control(t(lang, Key::RunResultOpen)),
            "{}",
            lang.tag()
        );
        assert!(
            f.surface.press_label(t(lang, Key::RunResultOpen)),
            "{}",
            lang.tag()
        );

        let mut g = showing_instructions(PreambleSource::Edited, None);
        g.surface.set_lang(lang);
        assert!(
            g.surface.has_control(t(lang, Key::RunSystemPromptEdit)),
            "{}",
            lang.tag()
        );
        g.surface.press_label(t(lang, Key::RunSystemPromptEdit));
        assert!(
            g.shows(t(lang, Key::RunSystemPromptSaveNote)),
            "{}",
            lang.tag()
        );
        assert!(
            g.surface.has_control(t(lang, Key::RunSystemPromptSave)),
            "{}",
            lang.tag()
        );

        let mut h = showing_instructions(PreambleSource::Bundled, None);
        h.surface.set_lang(lang);
        assert!(
            h.shows(t(lang, Key::RunSystemPromptNoFileToSave)),
            "{}",
            lang.tag()
        );
    }
}

// ------------------------------------------------------- ADR-366: asking again

/// **A run that has ended offers to be asked for again, beside the word that
/// says it ended** — and offers it whatever the ending was.
#[test]
fn every_ended_run_offers_to_be_asked_for_again() {
    for status in [
        RunStatus::Completed,
        RunStatus::Cancelled,
        RunStatus::Failed {
            stage: FailureStage::ProviderStart,
        },
    ] {
        let mut f = Fixture::new();
        f.read(view(header(status.clone()), here(), None));
        assert!(
            f.surface.has_control(t(Lang::En, Key::RunReRun)),
            "{status:?}"
        );
        assert!(
            f.shows(t(Lang::En, Key::RunReRunNote)),
            "and says what pressing it would do, before the press: {status:?}"
        );
    }
}

/// **Not while it is still going**, and never on the run Resume is for.
///
/// The two statuses are disjoint by construction — `Interrupted` is the only
/// one that is neither terminal nor running — so the two controls can never
/// stand on the same run, which is the whole of how they are kept from being
/// mistaken for each other.
#[test]
fn re_run_and_resume_never_stand_on_the_same_run() {
    let mut interrupted = Fixture::new();
    interrupted.read(view(
        header(RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Ended,
        }),
        here(),
        None,
    ));
    assert!(interrupted.surface.has_control(t(Lang::En, Key::RunResume)));
    assert!(
        !interrupted.surface.has_control(t(Lang::En, Key::RunReRun)),
        "an interrupted run is continued, not started again"
    );

    let mut finished = Fixture::new();
    finished.read(view(header(RunStatus::Completed), here(), None));
    assert!(finished.surface.has_control(t(Lang::En, Key::RunReRun)));
    assert!(
        !finished.surface.has_control(t(Lang::En, Key::RunResume)),
        "a run that is over cannot be resumed, and does not offer to be"
    );

    let mut running = Fixture::new();
    running.read(running_here());
    assert!(!running.surface.has_control(t(Lang::En, Key::RunReRun)));
    assert!(!running.surface.has_control(t(Lang::En, Key::RunResume)));
}

/// **A build that hides Delegate shows the run, not a way to start another**
/// (`koru_features::delegate`, which the host answers): no *Re-run* and no
/// note about what it would do, while the status line itself is unchanged.
#[test]
fn a_build_that_hides_delegate_offers_no_re_run() {
    let mut f = Fixture::new();
    f.surface.set_can_re_run(false);
    f.read(view(header(RunStatus::Completed), here(), None));
    assert!(!f.surface.has_control(t(Lang::En, Key::RunReRun)));
    assert!(!f.shows(t(Lang::En, Key::RunReRunNote)));
    assert!(
        f.shows(&words::status(Lang::En, &RunStatus::Completed)),
        "the run's status is shown as it was"
    );
}

/// **A run that executed somewhere else can still be asked for again.**
///
/// Every other control here acts on *this* run and needs the session this
/// device may not hold, which is why they are absent elsewhere. This one makes
/// a new run, which any window that can delegate at all can do.
#[test]
fn a_run_that_executed_elsewhere_can_still_be_asked_for_again() {
    let mut f = Fixture::new();
    f.read(view(header(RunStatus::Completed), there(), None));
    assert!(!f.surface.has_control(t(Lang::En, Key::RunEndRun)));
    assert!(f.surface.has_control(t(Lang::En, Key::RunReRun)));
}

/// **What travels is the whole request**, and nothing is sent to the old run.
#[test]
fn asking_again_hands_the_whole_request_over_and_touches_the_old_run() {
    let mut f = Fixture::new();
    let mut header = header(RunStatus::Completed);
    header.grants = [workspace()].into_iter().collect();
    header.requested_model =
        Some(koru_delegate_core::ModelName::new("deepseek/deepseek-chat").unwrap());
    f.read(view(header.clone(), here(), None));

    // Everything the surface asked its own worker for while opening, taken
    // off the list, so what is left afterwards is what the press did.
    let _ = f.link.commands();

    assert!(f.surface.press_label(t(Lang::En, Key::RunReRun)));
    let requests = f.surface.take_requests();
    let [Request::ReRun(seed)] = requests.as_slice() else {
        panic!("one request, and it is the re-run: {requests:?}");
    };
    assert_eq!(
        seed.run,
        run_id(),
        "it says which run it was filled in from"
    );
    assert_eq!(seed.workspace, workspace());
    assert_eq!(seed.objective, header.objective);
    assert_eq!(seed.agent, header.agent);
    assert_eq!(seed.requested_model, header.requested_model);
    assert_eq!(seed.scope, header.scope);
    assert_eq!(seed.grants, header.grants);

    // **Nothing was asked of the run.** Pressing it is a request to open a
    // surface; the old run is not read again, not resumed and not ended.
    assert!(
        f.link.commands().is_empty(),
        "the old run was left alone: {:?}",
        f.link.commands()
    );
}

/// **A pin whose source is in the trash or gone is dropped, and said so.**
///
/// The fixture's second pin is `InTrash`, which admission would refuse (D48).
/// Carrying it would put a person in front of a run that could not start and
/// tell them so afterwards.
#[test]
fn a_source_in_the_trash_is_left_out_of_the_new_request() {
    let mut f = Fixture::new();
    f.read(view(header(RunStatus::Completed), here(), None));
    assert!(f.surface.press_label(t(Lang::En, Key::RunReRun)));
    let requests = f.surface.take_requests();
    let [Request::ReRun(seed)] = requests.as_slice() else {
        panic!("one re-run: {requests:?}");
    };
    assert_eq!(seed.context.len(), 1, "the live one, and only it");
    assert!(
        matches!(seed.context[0], RunContextItem::Document { .. }),
        "{:?}",
        seed.context
    );
    assert!(seed.dropped, "and the surface that opens will say so");
}

/// **Not knowing is not saying gone** (D46). A device that could not resolve
/// the sources answers with an empty state list, and everything is carried.
#[test]
fn a_device_that_cannot_see_the_sources_carries_all_of_them() {
    let mut f = Fixture::new();
    let mut v = view(header(RunStatus::Completed), here(), None);
    v.context = Vec::new();
    f.read(v);
    assert!(f.surface.press_label(t(Lang::En, Key::RunReRun)));
    let requests = f.surface.take_requests();
    let [Request::ReRun(seed)] = requests.as_slice() else {
        panic!("one re-run: {requests:?}");
    };
    assert_eq!(seed.context.len(), 2, "both, because neither was ruled out");
    assert!(
        !seed.dropped,
        "and nothing is claimed to have been left out"
    );
}

/// The word exists in every language, and the keyboard reaches the control.
#[test]
fn asking_again_reads_and_is_reachable_in_every_language() {
    for lang in Lang::ALL {
        let mut f = Fixture::new();
        f.surface.set_lang(lang);
        f.read(view(header(RunStatus::Completed), here(), None));
        let label = t(lang, Key::RunReRun);
        assert!(!label.trim().is_empty(), "{lang:?}");
        assert!(f.surface.has_control(label), "{lang:?}");
        assert!(
            f.shows(t(lang, Key::RunReRunNote)),
            "the consequence is said in the same language: {lang:?}"
        );
    }
}

/// Every control the two tabs added is reachable from the keyboard, in the
/// one ordered list the pointer hits.
#[test]
fn the_new_controls_are_reachable_from_the_keyboard() {
    let mut f = showing_results();
    let lang = Lang::En;
    assert!(
        f.surface
            .control_labels()
            .contains(&t(lang, Key::RunResultOpen).to_owned())
    );
    f.surface.focus = None;
    // The four tabs, then the result's own control.
    for _ in 0..5 {
        f.surface.key(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            None,
            winit::keyboard::ModifiersState::empty(),
        );
    }
    f.surface.key(
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        winit::keyboard::ModifiersState::empty(),
    );
    assert!(
        matches!(
            f.surface.take_requests().first(),
            Some(Request::OpenResult(_))
        ),
        "Enter opened the result"
    );
}

/// **Tab reaches it and Enter presses it** — the same ring every other control
/// on this surface is in, so a person who never touches a pointer can ask for
/// a run again.
#[test]
fn the_keyboard_reaches_asking_again() {
    let mut f = Fixture::new();
    f.read(view(header(RunStatus::Completed), here(), None));
    let _ = f.link.commands();

    f.surface.focus = None;
    for _ in 0..40 {
        f.surface
            .key(&WinitKey::Named(NamedKey::Tab), None, Default::default());
        if f.surface.focused_label().as_deref() == Some(t(Lang::En, Key::RunReRun)) {
            break;
        }
    }
    assert_eq!(
        f.surface.focused_label().as_deref(),
        Some(t(Lang::En, Key::RunReRun)),
        "Tab walks onto it"
    );
    f.surface
        .key(&WinitKey::Named(NamedKey::Enter), None, Default::default());
    assert!(matches!(
        f.surface.take_requests().as_slice(),
        [Request::ReRun(_)]
    ));
}

#[test]
fn overview_names_the_requested_model_without_claiming_provider_report() {
    let mut f = Fixture::new();
    let mut chosen = header(RunStatus::Completed);
    chosen.requested_model =
        Some(koru_delegate_core::ModelName::new("deepseek/deepseek-chat").unwrap());
    f.read(view(chosen, here(), None));
    f.surface.set_tab(Tab::Overview);
    let lines = f.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains(t(Lang::En, Key::RunRequestedModelLabel)))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("deepseek/deepseek-chat"))
    );

    f.read(view(header(RunStatus::Completed), here(), None));
    assert!(
        !f.lines()
            .iter()
            .any(|line| line.contains(t(Lang::En, Key::RunRequestedModelLabel)))
    );
}

#[test]
fn failure_reason_is_visible_in_overview_and_activity_and_absent_for_old_runs() {
    use koru_delegate_core::{FailureStage, RunFailureReason};
    let mut f = Fixture::new();
    for reason in [Some(RunFailureReason::CapabilityNotNegotiated), None] {
        let stage = FailureStage::SessionCreation;
        let mut failed = header(RunStatus::Failed { stage });
        failed.failure_reason = reason;
        let mut snapshot = view(failed, here(), None);
        snapshot.timeline.items = vec![step(StepKind::Failed { stage, reason }, STARTED)];
        snapshot.timeline.total = 1;
        f.read(snapshot);
        for tab in [Tab::Overview, Tab::Activity] {
            f.surface.set_tab(tab);
            assert_eq!(
                f.shows(t(Lang::En, Key::RunFailureCapability)),
                reason.is_some()
            );
        }
    }
}
