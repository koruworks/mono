// SPDX-License-Identifier: MPL-2.0

//! The window's half of a run's Result view: what pressing *Open* does to
//! the tab strip.
//!
//! Nothing here dials a gateway — a result tab built under `cfg(test)` holds
//! an inert link (`result_view.rs`), so what is asserted is the strip.

use koru_artifacts_ui::OpenedArtifact;
use koru_delegate_api::wire::ListRunsResponse;
use koru_delegate_core::{
    ActorRef, ArtifactVersionId, EconomicsSummary, ExecutionEnvironment, Objective, ProfileId,
    RunHeader, RunId, RunOrigin, RunStatus, SessionProfileKey, WorkspaceScope,
};
use koru_runs_ui::OpenedResult;

use super::*;
use crate::mainview::harness::container;

fn a_result(version: u128, title: &str) -> OpenedArtifact {
    MainView::artifact_of_result(a_run_result(version, title))
}

/// The same result as a *run* names it — the shape that arrives on
/// [`Request::OpenResult`].
fn a_run_result(version: u128, title: &str) -> OpenedResult {
    OpenedResult {
        run: RunId::from_uuid(uuid::Uuid::from_u128(7)),
        version: ArtifactVersionId::from_uuid(uuid::Uuid::from_u128(version)),
        title: title.to_owned(),
        size_bytes: 4_096,
        held_elsewhere: None,
    }
}

fn a_header(run: RunId) -> RunHeader {
    RunHeader {
        run,
        created_at_ms: 10,
        requested_by: ActorRef::Person {
            profile: ProfileId::from_uuid(uuid::Uuid::from_u128(1)),
        },
        objective: Objective::new("Tidy the release notes").unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        origin: RunOrigin::Manual,
        scope: WorkspaceScope::Temporary,
        context: Vec::new(),
        grants: Default::default(),
        status: RunStatus::Completed,
        failure_reason: None,
        environment: ExecutionEnvironment::local(koru_delegate_core::DeviceId::from_uuid(
            uuid::Uuid::from_u128(3),
        )),
        artifacts: Vec::new(),
        economics: EconomicsSummary::default(),
        rating: None,
    }
}

/// **What the maintainers asked for.** A result becomes a tab of the main window,
/// named after the result, and it is the tab that is showing.
#[test]
fn a_result_opens_as_a_tab_named_after_it() {
    let (mut v, _w) = container();
    let before = v.tabs().len();
    assert!(v.open_artifact_tab(a_result(0x90, "The quarterly summary")));
    assert_eq!(v.tabs().len(), before + 1);
    let active = v.tabs().active();
    assert_eq!(active, v.tabs().len() - 1);
    assert_eq!(
        v.tabs().get(active).expect("a tab").label,
        "The quarterly summary"
    );
    assert!(matches!(
        v.tabs().get(active).expect("a tab").view,
        TabView::Result(_)
    ));
}

/// **Opened once.** A version is immutable (ADR-344), so a second tab on one
/// could only ever say the same thing; the one that exists is focused.
#[test]
fn opening_the_same_result_again_focuses_the_tab_it_is_already_in() {
    let (mut v, _w) = container();
    v.open_artifact_tab(a_result(0x90, "The quarterly summary"));
    let opened = v.tabs().len();
    let index = v.tabs().active();

    // Move away, then ask for it again.
    v.select(0);
    assert!(v.open_artifact_tab(a_result(0x90, "The quarterly summary")));
    assert_eq!(v.tabs().len(), opened, "no second tab on one version");
    assert_eq!(v.tabs().active(), index);
}

/// Two different results are two tabs: they are two documents.
#[test]
fn two_results_are_two_tabs() {
    let (mut v, _w) = container();
    v.open_artifact_tab(a_result(0x90, "The quarterly summary"));
    v.open_artifact_tab(a_result(0x91, "The regional detail"));
    let labels: Vec<&str> = v
        .tabs()
        .iter()
        .filter(|tab| matches!(tab.view, TabView::Result(_)))
        .map(|tab| tab.label.as_str())
        .collect();
    assert_eq!(labels, ["The quarterly summary", "The regional detail"]);
}

/// **The request is what opens it**, so the run views' one gesture and the
/// window's one primitive are the same path — not two.
#[test]
fn the_run_views_request_opens_the_tab() {
    let (mut v, _w) = container();
    let opened = v.perform_run_request(Request::OpenResult(Box::new(a_run_result(
        0x90,
        "The quarterly summary",
    ))));
    assert!(opened);
    assert!(matches!(
        v.tabs().get(v.tabs().active()).expect("a tab").view,
        TabView::Result(_)
    ));
}

/// A result tab's run icon is routed through the window's one run-tab door.
/// The icon remains for a run in Trash and disappears only once the run is
/// absent from both durable catalogs.
#[test]
fn a_result_run_icon_opens_the_run_and_disappears_after_retirement() {
    let (mut v, _w) = crate::mainview::harness::on_document();
    // Seat the inert index on this workspace before pushing its scripted
    // answer; `set_workspace` correctly clears an answer for another one.
    v.maintain_runs();
    let run = RunId::from_uuid(uuid::Uuid::from_u128(7));
    v.run_index.take_listing(ListRunsResponse {
        runs: vec![a_header(run)],
        sessions: Default::default(),
        documents: Default::default(),
        trashed: Vec::new(),
    });
    assert!(v.open_artifact_tab(a_result(0x96, "The quarterly summary")));
    let result_tab = v.tabs().active();

    let run_rect = match &mut v.tabs_mut().active_mut().view {
        TabView::Result(view) => view
            .surface_mut()
            .laid_rows()
            .into_iter()
            .find(|(row, _)| *row == koru_artifacts_ui::TabRow::Run)
            .map(|(_, rect)| rect)
            .expect("producing run icon"),
        _ => panic!("a result tab"),
    };
    match &mut v.tabs_mut().active_mut().view {
        TabView::Result(view) => assert!(
            view.surface_mut()
                .mouse_down(run_rect.x + run_rect.w * 0.5, run_rect.y + run_rect.h * 0.5,)
        ),
        _ => unreachable!(),
    }
    assert!(v.maintain_runs());
    assert!(matches!(
        &v.tabs().active_tab().view,
        TabView::Run(view) if view.run() == Some(run)
    ));

    // Permanent deletion is represented by absence from both the live and
    // Trash catalogs. The artifact remains open; only its run link retires.
    v.run_index.take_listing(ListRunsResponse {
        runs: Vec::new(),
        sessions: Default::default(),
        documents: Default::default(),
        trashed: Vec::new(),
    });
    v.select(result_tab);
    v.maintain_runs();
    match &mut v.tabs_mut().active_mut().view {
        TabView::Result(view) => assert!(
            !view
                .surface_mut()
                .rows()
                .contains(&koru_artifacts_ui::TabRow::Run),
            "retired run has no navigation icon"
        ),
        _ => panic!("the artifact remains open"),
    }
}

/// **Back after pressing a run pill returns to the row the pill is on**, and
/// Forward returns to the run.
///
/// The maintainers' report: *"If I click on the pill, the click back in history I'm
/// not landing where I was."* Both halves of it were broken and they were two
/// different bugs. `open_run_tab` recorded no jump of its own — the tab select
/// underneath it happened to record one, but only when a *new* tab was opened;
/// pressing a pill for a run whose tab was already in the strip and showing
/// recorded nothing at all. And a run tab was not a [`Spot`], so there was
/// never anything to come *forward* to: Back left the run and the way back
/// into it was gone.
///
/// Driven through `open_run`, which is the door the pill's click reaches, over
/// a document with a caret and a scroll — because "where I was" is a row, an
/// offset in it and how far down the page it sat, not just a tab.
#[test]
fn back_from_a_run_pill_lands_on_the_row_it_was_pressed_on() {
    use crate::mainview::delegate::pills::RunPillState;
    use crate::mainview::harness::{fill, on_document};
    use koru_delegate_core::RunStatus;
    use koru_editor::Editing;
    use koru_workspace_host::View;

    let (mut v, mut w) = on_document();
    fill(&v, &mut w.ws, 40);
    View::adopt(&mut v, &w.ws);
    v.size_visible_groups();
    if let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view {
        Editing::place_caret(leaf, 9);
        leaf.reflow();
    }
    v.tabs_mut().active_mut().view.set_scroll(48.0);
    let want = v.here().expect("the row the pill is on is a place");

    // The pill: a run crumb in this document, pressed.
    let run = RunId::from_uuid(uuid::Uuid::from_u128(0x51));
    let pill = koru_core::DocumentId::new();
    v.run_pill_states.insert(
        pill.to_storage(),
        RunPillState {
            run,
            status: RunStatus::running(),
            device: None,
            results: Vec::new(),
            latest_step: None,
            elapsed_ms: None,
            failure_reason: None,
        },
    );
    assert!(v.open_run(pill, koru_editor::OpenWhere::Reuse));
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Run(_)),
        "the pill opened the run"
    );

    // Back lands where the pill was pressed — the same tab, the same row, the
    // same caret and the same scroll.
    assert!(v.step_history(&w.ws, -1), "the row is one Back away");
    let got = v.here().expect("a document row");
    assert_eq!(got.tab, want.tab, "the tab the pill was in");
    assert_eq!(got.row, want.row, "the row it was on");
    assert_eq!(got.caret, want.caret, "with the caret where it was");
    assert_eq!(got.scroll, want.scroll, "and the page where it was");

    // ...and Forward returns to the run, which is the half a run tab that was
    // not a place could not do at all.
    assert!(v.step_history(&w.ws, 1), "forward returns to the run");
    match &v.tabs().active_tab().view {
        TabView::Run(view) => assert_eq!(view.run(), Some(run)),
        _ => panic!("forward landed somewhere that is not the run"),
    }
}

/// The other half of the same report: **pressing a pill for the run already
/// in front is not a jump**.
///
/// Back after it should still mean what it meant a moment ago. `select`
/// applies the same rule to a tab click and `record_jump` drops a stop that
/// lands where you already are; this door has to state it too, because the
/// question it asks is "is this run in front", not "is this tab index the
/// active one".
#[test]
fn pressing_a_pill_for_the_run_already_in_front_records_nothing() {
    use crate::mainview::harness::{fill, on_document};
    use koru_workspace_host::View;

    let (mut v, mut w) = on_document();
    fill(&v, &mut w.ws, 8);
    View::adopt(&mut v, &w.ws);
    v.size_visible_groups();

    let run = RunId::from_uuid(uuid::Uuid::from_u128(0x52));
    assert!(v.open_run_tab(run));
    let (tabs, depth) = (v.tabs().len(), v.history.len());

    assert!(v.open_run_tab(run), "the run in front stays in front");
    assert_eq!(v.tabs().len(), tabs, "no second tab on one run");
    assert_eq!(
        v.history.len(),
        depth,
        "standing still is not a jump, so history did not grow"
    );

    // ...and the one Back that was earned still returns to the document.
    assert!(v.step_history(&w.ws, -1));
    assert!(matches!(v.tabs().active_tab().view, TabView::Leaf(_)));
}

/// A run row for the Agents navigator, as the run index hands them over.
fn a_run_row(run: RunId) -> koru_runs_ui::RunRow {
    koru_runs_ui::RunRow {
        run,
        name: koru_delegate_core::RunName::of(run).to_string(),
        objective: "tidy the notes".to_owned(),
        status: "Finished".to_owned(),
        needs_attention: false,
        session: None,
        created_at_ms: 1_700_000_000_000,
        motion: koru_runs_ui::Motion::Still,
        results: 0,
    }
}

/// **What the maintainers asked for.** `Ctrl+Shift+E` on a run tab does for a run what
/// it has always done for a document: it brings the column forward, switches it
/// to the view the thing lives in — Agents — selects the run's own row, and
/// leaves the keyboard there.
#[test]
fn ctrl_shift_e_on_a_run_tab_reveals_the_run_in_the_agents_navigator() {
    use crate::mainview::harness::on_document;
    use crate::panes::explore::NavigatorMode;

    let (mut v, w) = on_document();
    let run = RunId::from_uuid(uuid::Uuid::from_u128(0x53));
    v.explore.set_agent_runs(vec![a_run_row(run)]);
    assert!(v.open_run_tab(run));

    assert!(v.focus_or_toggle_sidebar(&w.ws), "the shortcut is answered");
    assert_eq!(
        v.explore.navigator_mode(),
        NavigatorMode::Agents,
        "the column switched to the view a run lives in"
    );
    assert!(v.sidebar.is_open(), "and it is open");
    let node = koru_core::NodeId::from_uuid(run.as_uuid());
    assert_eq!(
        v.explore.selected(),
        Some(node),
        "with the run's own row selected"
    );
    assert_eq!(
        v.explore.focused(),
        Some(node),
        "and the keyboard on it, which is the second half of the ask"
    );
}

/// **The press that opens the column does not close it again.** The
/// fall-through in `focus_or_toggle_sidebar` toggles the pane when the column
/// already holds the keyboard, and revealing a run takes the keyboard — so
/// without the run's own early return the one gesture would open and shut in
/// the same press. The Tasks arm learned this first.
#[test]
fn revealing_a_run_leaves_the_column_open() {
    let (mut v, w) = crate::mainview::harness::on_document();
    let run = RunId::from_uuid(uuid::Uuid::from_u128(0x54));
    v.explore.set_agent_runs(vec![a_run_row(run)]);
    v.open_run_tab(run);
    while v.sidebar.is_open() {
        v.toggle_sidebar();
    }
    assert!(v.focus_or_toggle_sidebar(&w.ws));
    assert!(v.sidebar.is_open(), "one press opens it and leaves it open");
}

/// **A result tab reveals the result in Artifacts, not the run in Agents**
/// (ADR-373, the maintainers' own words: *"view in artifacts, select the view and
/// focus the node"*). A result is not its run any more — it outlives the run's
/// tab and is opened from the Artifacts column with no run in sight — so the
/// column it lives in is the one that holds it.
#[test]
fn ctrl_shift_e_on_a_result_tab_reveals_it_in_artifacts() {
    use crate::panes::explore::NavigatorMode;

    let (mut v, w) = crate::mainview::harness::on_document();
    let run = RunId::from_uuid(uuid::Uuid::from_u128(0x55));
    v.explore.set_agent_runs(vec![a_run_row(run)]);
    v.open_artifact_tab(a_result(0x95, "The quarterly summary"));

    assert!(v.focus_or_toggle_sidebar(&w.ws), "the shortcut is answered");
    assert_eq!(
        v.explore.navigator_mode(),
        NavigatorMode::Artifacts,
        "the column switched to the view a result lives in"
    );
    assert!(v.sidebar.is_open(), "and it is open");
    assert_ne!(
        v.explore.selected(),
        Some(koru_core::NodeId::from_uuid(run.as_uuid())),
        "and it selected no run — a result is not its run"
    );
}

/// **`Ctrl+Shift+E` in an AI chat tab shows the Chats view with that chat
/// selected and the keyboard on it** (AGENTUX I5) — the same gesture a result
/// tab makes toward Artifacts. The list has not answered yet under test, so
/// the reveal is remembered and lands when a listing that holds the chat
/// arrives.
#[test]
fn ctrl_shift_e_on_an_ai_chat_tab_reveals_it_in_chats() {
    use crate::panes::explore::NavigatorMode;

    let (mut v, w) = crate::mainview::harness::on_document();
    let chat = koru_ai_ui::ChatId::from_uuid(uuid::Uuid::from_u128(0xc4a7));
    assert!(v.open_ai_chat_tab_for(chat));

    assert!(v.focus_or_toggle_sidebar(&w.ws), "the shortcut is answered");
    assert_eq!(
        v.explore.navigator_mode(),
        NavigatorMode::Chats,
        "the column switched to the view a chat lives in"
    );
    assert!(v.sidebar.is_open(), "and it is open");

    let now = koru_ai_core::Timestamp::from_millis(1);
    v.explore
        .chats_mut()
        .take_listing(Ok(koru_chats_ui::Listing {
            chats: vec![koru_ai_core::Chat {
                id: chat,
                title: "Open in a tab".into(),
                private: false,
                current_model: koru_ai_core::ModelSelection {
                    provider: koru_ai_core::AiProviderId::parse("openai").unwrap(),
                    requested_model: "gpt-test".into(),
                    default_reasoning: koru_ai_core::ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                created_at: now,
                updated_at: now,
            }],
            agents: Vec::new(),
        }));
    assert_eq!(v.explore.chats().focused_chat(), Some(chat));
    assert!(v.explore.has_focus(), "the keyboard is on it");
}

/// **A run tab cards its row, the way a document tab cards its document**
/// (ADR-379, the maintainers' *active selection style background*).
///
/// The per-frame answer to *which row is the active tab showing* named a
/// session tab's session in Agents mode and nothing else, so a run tab left
/// the column with no row marked — and quietly undid the `set_selected` the
/// reveal had just made. What is asserted is the refresh, not the reveal: the
/// row stays carded after the column has been rebuilt.
#[test]
fn a_run_tab_keeps_its_row_carded_across_a_refresh() {
    use crate::mainview::harness::on_document;
    use crate::panes::explore::NavigatorMode;

    let (mut v, w) = on_document();
    let run = RunId::from_uuid(uuid::Uuid::from_u128(0x56));
    v.explore.set_agent_runs(vec![a_run_row(run)]);
    v.explore.show_agents();
    v.open_run_tab(run);

    v.refresh_explore(&w.ws, true);
    assert_eq!(v.explore.navigator_mode(), NavigatorMode::Agents);
    assert_eq!(
        v.explore.selected(),
        Some(koru_core::NodeId::from_uuid(run.as_uuid())),
        "the run's row is the one the column shows as active"
    );
}
