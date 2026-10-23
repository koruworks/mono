// SPDX-License-Identifier: MPL-2.0

//! **The characterisation table**, written before the routing decision was
//! factored out and pinned to the answers the unrefactored chain gave.
//!
//! `MainView`'s key path is seventeen ordered checks in a 8,000-line file, and
//! it is the busiest code path in the app: a mistake here breaks typing
//! everywhere, not only the surface being changed. So this suite does not
//! assert what the chain *should* do — it records what it *did*, cell by cell,
//! and the refactor has to reproduce the table byte for byte. The discipline is
//! `a_click_lands_in_the_same_place_through_the_container_as_without_it`'s:
//! compare against something independent of the arithmetic, never assert the
//! arithmetic back to itself.
//!
//! **Both a handled and an unhandled key in every state.** A matrix of state
//! names alone would pass while silently swallowing the keystrokes that are
//! meant to fall through — and falling through is exactly what the two
//! key-dependent layers do. An open popover *"takes Esc, the arrows and Enter —
//! and lets everything else through"*; a focused sidebar takes *"only the
//! navigation keys"*. Each cell therefore records where the character went, not
//! merely whether something claimed the press.
//!
//! Driven through [`View::key`] rather than the inherent `MainView::key`,
//! because the two differ — the trait wrapper claims the Move to… picker and
//! the search overlay before the container's own chain sees anything — and the
//! shell calls the trait one.

use super::super::*;
use crate::mainview::harness::*;
use koru_core::schema::Parent;
use koru_workspace_host::View;
use winit::keyboard::NamedKey;

// ------------------------------------------------------------------ the keys

/// One press, named so a cell can say which it was.
struct Press {
    name: &'static str,
    key: Key,
    text: Option<&'static str>,
    mods: ModifiersState,
}

fn presses() -> Vec<Press> {
    vec![
        // The one that matters most: a printable character is what dictation
        // is, so every row has to say where this one landed.
        Press {
            name: "char-x",
            key: ch("x"),
            text: Some("x"),
            mods: NONE,
        },
        Press {
            name: "escape",
            key: Key::Named(NamedKey::Escape),
            text: None,
            mods: NONE,
        },
        Press {
            name: "arrow-down",
            key: Key::Named(NamedKey::ArrowDown),
            text: None,
            mods: NONE,
        },
        Press {
            name: "enter",
            key: Key::Named(NamedKey::Enter),
            text: None,
            mods: NONE,
        },
        // A container chord, which sits *below* every blocker and above the
        // active tab — the layer a mis-ordered refactor would move.
        Press {
            name: "ctrl-t",
            key: ch("t"),
            text: Some("t"),
            mods: PRIMARY,
        },
        // A key nothing claims anywhere. Its whole job is to prove that a
        // "modal" really does swallow everything and a panel really does not.
        // **It was F9 until SESSIONRECORD**: the internal session recorder
        // claimed F9 on internal Windows builds, which is exactly the kind of
        // change this probe exists to catch — so the probe moved to F13.
        Press {
            name: "f13",
            key: Key::Named(NamedKey::F13),
            text: None,
            mods: NONE,
        },
    ]
}

// ---------------------------------------------------------------- the states

/// Every state the chain distinguishes, armed from a fresh container.
const STATES: &[&str] = &[
    "plain-document",
    "welcome-tab",
    "manage-tab",
    "trash-tab",
    "error-dialog",
    "import-dialog",
    "updates-modal",
    "update-newer-modal",
    "confirm",
    "new-document-prompt",
    "link-device",
    "workspace-icon",
    "workspace-rename",
    "context-menu",
    "recording-modal",
    "all-reminders-overlay",
    "reminder-creation",
    "reminder-management",
    "popover-open",
    "sidebar-focused",
    "sidebar-focused-over-a-prompt",
    "document-rename-prompt",
    "ai-focused",
    "move-to",
    "search-overlay",
];

fn a_workspace_list() -> Vec<koru_core::WorkspaceEntry> {
    vec![
        koru_core::WorkspaceEntry::new(koru_core::WorkspaceId::new(), "Alpha", 1),
        koru_core::WorkspaceEntry::new(koru_core::WorkspaceId::new(), "Beta", 2),
    ]
}

fn an_import_preview() -> koru_workspace_host::ImportPreview {
    koru_workspace_host::ImportPreview {
        workspace_name: "Imported".into(),
        document_count: 1,
        node_count: 3,
        image_count: 0,
        warnings: Vec::new(),
        rows: vec![koru_workspace_host::ImportPreviewRow {
            depth: 0,
            title: "Notes".into(),
            nodes: 3,
            trashed: false,
        }],
        identity_conflict: false,
    }
}

fn a_report(stage: koru_agent_wire::UpdateStage) -> koru_agent_wire::UpdateReport {
    koru_agent_wire::UpdateReport {
        stage,
        version: "0.9.1".into(),
        build_number: 412,
        title: "Faster documents".into(),
        summary: vec!["One line.".into()],
        notes_url: None,
        received: 0,
        total: 0,
        can_install: true,
        download_url: None,
        message: None,
        detail: None,
        channel: "nightly".into(),
        automatic_checks: true,
        active_session_count: 0,
        session_generation: 0,
    }
}

/// Open the explore pane, give it the workspace's rows, and hand it the
/// keyboard — the three steps every sidebar test takes.
fn focus_the_tree(v: &mut MainView, ws: &WorkspaceDoc) {
    v.toggle_sidebar();
    View::adopt(v, ws);
    v.refresh_explore(ws, true);
    v.explore.focus_tree(v.regions.sidebar);
}

fn reminder_target(v: &MainView, node: NodeId) -> reminders::Target {
    let (_, profile, _, workspace) = v.pane_context.as_ref().expect("pane context");
    let workspace = workspace.expect("fixture has a durable workspace");
    reminders::Target {
        profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
        entity: koru_store_core::EntityRef::new(
            "document",
            koru_store_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            koru_store_core::DocumentId::new(),
            node.as_uuid(),
        )
        .expect("valid reminder target"),
    }
}

/// Build the container in `state`, ready for one press.
fn arm(state: &str) -> (MainView, koru_workspace::Workspace) {
    let (mut v, mut w) = on_document();
    // Two rows under the document's title, so a character that reaches the
    // body has somewhere visible to land.
    let doc = v.document_root().expect("a document tab");
    for i in 0..2 {
        w.ws.insert(
            Parent::Node(doc),
            koru_core::node::Node::paragraph(format!("row {i}")).unwrap(),
        )
        .expect("insert");
    }
    View::adopt(&mut v, &w.ws);
    let workspaces = a_workspace_list();
    let current = workspaces[0].id;
    View::set_pane_context(
        &mut v,
        Vec::new(),
        koru_core::ProfileId::new(),
        workspaces,
        Some(current),
    );

    match state {
        "plain-document" => {}
        "welcome-tab" => {
            // Tab 0 is the welcome tab `on_document` opened behind the
            // document.
            v.select(0);
        }
        "manage-tab" => {
            v.open_manage(crate::manage::Page::Cloud);
        }
        "trash-tab" => {
            v.open_trash_tab();
        }
        "error-dialog" => {
            v.error = Some(error_dialog::ErrorDialog::new(
                koru_workspace_host::ErrorReport::new("Couldn't export", "Nothing was changed."),
            ));
        }
        "import-dialog" => {
            v.import_dialog = Some(import_dialog::ImportDialog::new(an_import_preview()));
        }
        "updates-modal" => {
            v.updates_modal = true;
        }
        "update-newer-modal" => {
            let (feed, _sent) = crate::agent::UpdateFeed::for_test(a_report(
                koru_agent_wire::UpdateStage::InstalledNewer,
            ));
            v.updates = feed;
        }
        "confirm" => {
            v.confirm = Some((
                koru_ui::Confirm::new(
                    "Move 4 items to Trash?".to_owned(),
                    "They can be restored.".to_owned(),
                    "Move to Trash",
                ),
                crate::mainview::trash::ConfirmSubject::TrashDocuments(Vec::new()),
            ));
        }
        "new-document-prompt" => {
            v.new_document = Some(new_document::NewDocument::new(&mut v.engine));
        }
        "link-device" => {
            v.link_device = Some(link_device::LinkDevice::new(&mut v.engine));
        }
        "workspace-icon" => {
            let id = v.current_workspace().expect("a workspace");
            v.workspace_icon = Some(workspace_icon::EmojiColorPanel::new(
                &mut v.engine,
                id,
                "Alpha",
                None,
                None,
            ));
        }
        "workspace-rename" => {
            let id = v.current_workspace().expect("a workspace");
            v.workspace_rename = Some(workspace_rename::WorkspaceRename::new(
                &mut v.engine,
                id,
                "Alpha",
            ));
        }
        "context-menu" => {
            v.open_window_menu(Rect::new(600.0, 0.0, 30.0, 30.0));
        }
        "recording-modal" => {
            assert!(v.open_recording_modal(), "the setup modal opened");
        }
        "all-reminders-overlay" => {
            v.reminder_shell.apply(
                reminder_shell::Intent::OpenAllReminders,
                std::time::Instant::now(),
            );
        }
        "reminder-creation" => {
            let target = reminder_target(&v, doc);
            let context = v.reminder_context(&w.ws, doc);
            v.reminders.open(
                target,
                koru_editor::reminders::Request {
                    identity: koru_editor::reminders::RequestIdentity {
                        token: koru_editor::reminders::RequestToken::new(),
                        node: doc,
                    },
                    phrase: String::new(),
                    due_anchor: context.due_anchor.clone(),
                },
                context.clone(),
            );
            let creation = v.reminders.creation.clone();
            v.reminder_creation.update_context(
                context,
                koru_i18n::time::LocaleFormat::for_system(Some("en-US"), None),
                koru_editor::reminders::Tone::System,
            );
            v.reminder_creation.sync_creation(
                &mut v.engine,
                creation.as_ref(),
                Some(Rect::new(80.0, 80.0, 120.0, 24.0)),
            );
        }
        "reminder-management" => {
            let target = reminder_target(&v, doc);
            let context = v.reminder_context(&w.ws, doc);
            v.reminders.open_management(target, context);
            v.reminder_creation
                .open_management(doc, Rect::new(80.0, 80.0, 120.0, 24.0));
        }
        "popover-open" => {
            v.open_workspaces_popover();
        }
        "sidebar-focused" => {
            focus_the_tree(&mut v, &w.ws);
        }
        "sidebar-focused-over-a-prompt" => {
            // F2 opens the document's rename prompt; the tree is then handed
            // the keyboard *while it is up*, which is the state the
            // `is_prompting` guard exists for.
            View::key(&mut v, &mut w.ws, &Key::Named(NamedKey::F2), None, NONE);
            focus_the_tree(&mut v, &w.ws);
        }
        "document-rename-prompt" => {
            View::key(&mut v, &mut w.ws, &Key::Named(NamedKey::F2), None, NONE);
        }
        "ai-focused" => {
            v.toggle_ai();
        }
        "move-to" => {
            // The picker refuses to open without a live listing, so give it
            // one naming the document that is showing.
            let id = koru_core::DocumentId::new();
            v.entries = Some(vec![koru_core::explore::ExploreEntry {
                id,
                parent: None,
                ordinal: 0,
                title: "Notes".into(),
                kind: koru_core::explore::KIND_DOCUMENT.into(),
                mime: None,
                note: None,
                trashed_at: None,
                trashed_by: None,
            }]);
            assert!(v.open_move_to(vec![id]), "the picker opened");
        }
        "search-overlay" => {
            assert!(v.open_search(), "the search overlay opened");
        }
        other => panic!("unknown state {other}"),
    }
    (v, w)
}

// ----------------------------------------------------------- the observation

/// Which blocking surfaces are standing, as a stable name list.
fn open_surfaces(v: &MainView) -> String {
    let mut out = Vec::new();
    if v.move_to.is_some() {
        out.push("move-to");
    }
    if v.search.is_some() {
        out.push("search");
    }
    if v.error.is_some() {
        out.push("error");
    }
    if v.import_dialog.is_some() {
        out.push("import");
    }
    if v.updates_modal {
        out.push("updates");
    }
    if v.update_newer_modal_open() {
        out.push("newer");
    }
    if v.confirm.is_some() {
        out.push("confirm");
    }
    if v.new_document.is_some() {
        out.push("new-document");
    }
    if v.snapshot_prompt.is_some() {
        out.push("save-snapshot");
    }
    if v.link_device.is_some() {
        out.push("link-device");
    }
    if v.workspace_icon.is_some() {
        out.push("workspace-icon");
    }
    if v.workspace_rename.is_some() {
        out.push("workspace-rename");
    }
    if v.menu.is_some() {
        out.push("menu");
    }
    if v.recording_modal.is_some() {
        out.push("recording");
    }
    if v.reminder_shell.overlay_live() {
        out.push("all-reminders");
    }
    if v.reminder_creation.is_creation() {
        out.push("reminder-creation");
    }
    if v.reminder_creation.is_management() {
        out.push("reminder-management");
    }
    if v.tabs().active_tab().view.is_prompting() {
        out.push("tab-prompt");
    }
    if out.is_empty() {
        "-".to_owned()
    } else {
        out.join("+")
    }
}

/// The active document's text, title row first — **where a character that fell
/// all the way through would have landed**.
fn document_text(v: &MainView, ws: &WorkspaceDoc) -> String {
    let Some(root) = v.document_root() else {
        return "·".to_owned();
    };
    let mut rows = Vec::new();
    if let Ok(node) = ws.read(root) {
        rows.push(node.content.as_text().unwrap_or_default().to_owned());
    }
    if let Ok(kids) = ws.children(Parent::Node(root)) {
        for kid in kids {
            if let Ok(node) = ws.read(kid) {
                rows.push(node.content.as_text().unwrap_or_default().to_owned());
            }
        }
    }
    rows.join("/")
}

fn tab_kind(v: &MainView) -> &'static str {
    match &v.tabs().active_tab().view {
        TabView::Welcome(_) => "welcome",
        TabView::Home(_) => "home",
        TabView::Manage(_) => "manage",
        TabView::Trash(_) => "trash",
        TabView::Settings(_) => "settings",
        TabView::Tasks(_) => "tasks",
        TabView::Reminders(_) => "reminders",
        TabView::Feedback(_) => "feedback",
        TabView::Diff(_) => "diff",
        TabView::AiChat(_) => "ai_chat",
        TabView::Socials(_) => "socials",
        TabView::Session(_) => "session",
        TabView::Run(_) => "run",
        TabView::Result(_) => "result",
        TabView::Canvas(_) => "canvas",
        TabView::Leaf(_) => "leaf",
    }
}

/// One cell: what the press did, said in terms nothing in the chain owns.
fn observe(v: &MainView, ws: &WorkspaceDoc, handled: bool) -> String {
    format!(
        "handled={} open={} doc={} tabs={}/{}:{} explore=popover:{},focus:{},sel:{} ai={}",
        if handled { 'Y' } else { 'n' },
        open_surfaces(v),
        document_text(v, ws),
        v.tabs().len(),
        v.tabs().active(),
        tab_kind(v),
        if v.explore.popover_open() { 'Y' } else { 'n' },
        if v.explore.has_focus() { 'Y' } else { 'n' },
        v.explore
            .selected()
            .map_or_else(|| "-".to_owned(), |_| "some".to_owned()),
        if v.ai_focused { 'Y' } else { 'n' },
    )
}

/// The whole matrix, rendered.
fn table() -> String {
    let mut out = String::new();
    for state in STATES {
        for press in presses() {
            let (mut v, mut w) = arm(state);
            let handled = View::key(&mut v, &mut w.ws, &press.key, press.text, press.mods);
            out.push_str(&format!(
                "{state:<30} {:<11} {}\n",
                press.name,
                observe(&v, &w.ws, handled)
            ));
        }
    }
    out
}

/// **The gate.** Captured from the chain before it was factored; the refactor
/// reproduces it or it is a behaviour change, whatever it looks like.
///
/// When a *deliberate* routing change is made, the way to update this is to
/// paste the printed table below — never to relax the comparison.
///
/// **Moved once, deliberately, by SP-8b.** The workspaces tab's `char-x`,
/// `escape`, `arrow-down` and `f9` went from `handled=Y` to `handled=n`. The
/// manager that swallowed them had a search box and walked its own rows with
/// the arrows; the shared surface (`koru-workspaces-ui`) claims no chord of the
/// container's at all — only a confirmation on screen takes a key, and there is
/// none in these rows. So four keys the manager used to eat now reach the
/// container that owns them.
///
/// **Moved a second time, deliberately, by SESSIONRECORD (I3).** The unclaimed
/// probe key was F9, and F9 is now the internal session recorder's chord on
/// internal Windows builds — claimed by design, so the probe moved to F13 and
/// every `f9` row became an `f13` row with the same answer. The matrix also
/// gained the recorder's own state: `recording-modal` swallows the probe
/// exactly as every other modal does.
///
/// **Moved a third time, deliberately, by the one Manage tab
/// (ADR-449-onemanagetab).** The `workspaces-tab` state went with the
/// Workspaces tab and became `manage-tab`, the tab that replaced it. Its
/// `arrow-down` and `enter` are `handled=Y`: Manage's nav walks its rows with
/// the arrows and opens one with Enter. The other four keys route as the
/// Workspaces tab's did.
const EXPECTED: &str = include_str!("routing.table");

#[test]
fn the_key_chain_routes_exactly_as_it_did_before_it_was_factored() {
    let actual = table();
    // `include_str!` hands back whatever is on disk, and git may have checked
    // the table out with CRLF endings on this platform. Normalising here means
    // the golden file is compared for its *content* rather than for how the
    // working tree happens to store it.
    let expected = EXPECTED.replace("\r\n", "\n");
    if actual != expected {
        // Printed whole, because a one-line diff of a matrix is unreadable and
        // the fix is either a bug or a paste.
        panic!(
            "the routing table moved.\n\n--- expected ---\n{EXPECTED}\n--- actual ---\n{actual}"
        );
    }
}

/// **F9 is no longer a key nothing claims.** Where the session recorder
/// exists (internal Windows builds) it opens the setup modal, the modal is
/// the standing blocker, and F9 again — through the modal, which is what the
/// blocker row above routes — puts it away. Everywhere else the key falls
/// through exactly as if unbound, which is the gate doing its job.
#[test]
fn f9_opens_and_closes_the_recording_modal_where_it_exists() {
    // The F9 arm reads the process-global RECORDING_ACTIVE: the lock
    // serializes this against the recording suite's parallel flip
    // (TESTING.md's rule).
    let _guard = koru_recording::RECORDING_ACTIVE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut v, mut w) = arm("plain-document");
    let handled = View::key(&mut v, &mut w.ws, &Key::Named(NamedKey::F9), None, NONE);
    if cfg!(windows) && koru_channel::ReleaseChannel::this_build_is_internal() {
        assert!(handled, "the recorder's chord is claimed");
        assert_eq!(format!("{:?}", v.blockers()), "Modal(Recording)");
        let handled = View::key(&mut v, &mut w.ws, &Key::Named(NamedKey::F9), None, NONE);
        assert!(handled, "the modal swallows the key that closes it");
        assert_eq!(format!("{:?}", v.blockers()), "None");
    } else {
        assert!(!handled, "nowhere the recorder exists is F9 claimed");
    }
}

// ------------------------------------------------- the two factored answers

/// What [`MainView::blockers`] and [`MainView::editable_target`] say in each of
/// the same states.
///
/// **Read against the `char-x` column of the table above**, which is what makes
/// this a claim rather than a restatement: every `Tab(..)` here is a state
/// where the character reached the document, every `Field(..)` one where it was
/// taken by a surface and the document was left alone, and every `None` one
/// where the press changed nothing anywhere.
const ANSWERS: &[(&str, &str, &str)] = &[
    ("plain-document", "None", "Tab(TabId(1))"),
    ("welcome-tab", "None", "None"),
    // Manage has no text field outside a page's own rename or create form,
    // and a wrong `None` only declines a dictation where a wrong target would
    // put words somewhere nobody looked. See QUESTIONS Q-V14.
    ("manage-tab", "None", "None"),
    ("trash-tab", "None", "None"),
    ("error-dialog", "Modal(Error)", "None"),
    ("import-dialog", "Modal(Import)", "None"),
    ("updates-modal", "Modal(Updates)", "None"),
    ("update-newer-modal", "Modal(UpdateNewer)", "None"),
    ("confirm", "Modal(Confirm)", "None"),
    (
        "new-document-prompt",
        "Prompt(NewDocument)",
        "Field(NewDocument)",
    ),
    ("link-device", "Prompt(LinkDevice)", "Field(LinkDevice)"),
    ("workspace-icon", "Prompt(Appearance)", "Field(Appearance)"),
    ("workspace-rename", "Prompt(Rename)", "Field(Rename)"),
    ("context-menu", "Modal(ContextMenu)", "None"),
    ("recording-modal", "Modal(Recording)", "None"),
    ("all-reminders-overlay", "Modal(ReminderOverlay)", "None"),
    (
        "reminder-creation",
        "Prompt(ReminderCreation)",
        "Field(ReminderCreation)",
    ),
    ("reminder-management", "Modal(ReminderManagement)", "None"),
    // **The two that make the case for two types.** Both hold the keyboard for
    // some keys and neither accepts text, so neither is a blocker and neither
    // changes where a character goes.
    ("popover-open", "None", "Tab(TabId(1))"),
    ("sidebar-focused", "None", "Tab(TabId(1))"),
    (
        "sidebar-focused-over-a-prompt",
        "None",
        "Field(DocumentPrompt)",
    ),
    ("document-rename-prompt", "None", "Field(DocumentPrompt)"),
    ("ai-focused", "None", "Field(Ai)"),
    ("move-to", "Prompt(MoveTo)", "Field(MoveTo)"),
    ("search-overlay", "Prompt(Search)", "Field(Search)"),
];

#[test]
fn the_blocker_and_the_editable_target_agree_with_the_chain_that_routes_the_key() {
    // Every state in the matrix has a row here, or one of the two was added
    // without anybody deciding what it answers.
    let named: Vec<&str> = ANSWERS.iter().map(|(state, _, _)| *state).collect();
    assert_eq!(named, STATES.to_vec(), "every state has an answer");

    for (state, blockers, target) in ANSWERS {
        let (v, _w) = arm(state);
        assert_eq!(
            format!("{:?}", v.blockers()),
            *blockers,
            "blockers in {state}"
        );
        assert_eq!(
            format!("{:?}", v.editable_target()),
            *target,
            "editable target in {state}"
        );
    }
}

/// **The order lives in one table, and both entry points walk it.**
///
/// `View::key` starts at the top; the inherent `MainView::key` starts below the
/// two surfaces the wrapper has already claimed. Asserted rather than described
/// because "starts lower down" is exactly the kind of thing a later edit
/// re-implements as a second `if`.
#[test]
fn the_container_chain_starts_below_the_two_surfaces_the_wrapper_claims() {
    let (mut v, _w) = arm("move-to");
    v.error = Some(error_dialog::ErrorDialog::new(
        koru_workspace_host::ErrorReport::new("Couldn't export", "Nothing was changed."),
    ));
    assert_eq!(
        format!("{:?}", v.blockers()),
        "Prompt(MoveTo)",
        "the picker floats over the whole window"
    );
    assert_eq!(
        format!("{:?}", v.blockers_from(super::Layer::Error)),
        "Modal(Error)",
        "…and the container's own chain, which has never seen it, still finds \
         the dialog it always found"
    );
}

// ------------------------------------------------------------- the dictation

use koru_dictation::{DictationAction, Interruption, Transcript};

/// **No test starts a voice worker.**
///
/// `cfg(test)` inside `koru-dictation` cannot cover this crate — a dependency
/// is never compiled with it — so `apps/koru`'s `[dev-dependencies]` turns on
/// `no-engine`, and this is what says the flag is still there. The same shape,
/// and the same lesson, as `koru-search-ui`'s `no-reader`.
#[test]
fn a_dictation_built_under_test_reaches_no_worker() {
    assert!(
        koru_dictation::ENGINE_OFF,
        "`no-engine` is off in apps/koru's dev-dependencies: a test could dial \
         the voice worker on the machine running the suite, or ask an agent to \
         start one"
    );
}

/// The chord is a registry row, at the layer that can actually see a release.
#[test]
fn the_dictate_chord_is_registered_at_the_shell() {
    let row = koru_keys::registry::get(koru_keys::ShortcutId::Dictate);
    assert_eq!(row.layer, koru_keys::Layer::Shell);
    // Not `PRIMARY_SHIFT`+Space: that is the editor's emoji picker one layer
    // down, and it is what every input method uses to switch itself on and
    // off. `no_shortcut_shadows_a_lower_layer` in `koru-keys` covers the
    // first half; the second half is written down in QUESTIONS Q-V58.
    assert_ne!(
        row.chord,
        koru_keys::Chord::new(
            koru_keys::named(winit::keyboard::NamedKey::Space),
            PRIMARY_SHIFT
        )
    );
}

/// What `MainView::dictate` does with a `Begin` in each state of the matrix
/// above, beside what a character does.
///
/// **Read against the `char-x` column and the `ANSWERS` table.** The claim is
/// that a dictation reaches the same surface a keystroke would, and the two
/// exceptions are named rather than hidden.
const OPENS: &[(&str, bool)] = &[
    ("plain-document", true),
    ("welcome-tab", false),
    ("manage-tab", false),
    ("trash-tab", false),
    ("error-dialog", false),
    ("import-dialog", false),
    ("updates-modal", false),
    ("update-newer-modal", false),
    ("confirm", false),
    ("new-document-prompt", true),
    // **Q-V25.** A character reaches the device-link code box and a dictation
    // does not: it is a credential, drawn masked, read off another screen.
    // The refusal is the host's, because the widget cannot know.
    ("link-device", false),
    ("workspace-icon", true),
    ("workspace-rename", true),
    ("context-menu", false),
    ("all-reminders-overlay", false),
    ("reminder-creation", true),
    ("reminder-management", false),
    ("popover-open", true),
    ("sidebar-focused", true),
    ("sidebar-focused-over-a-prompt", true),
    ("document-rename-prompt", true),
    // **Q-V59.** `TextArea` forwards the same provisional composition as the
    // one-line fields; committing text never invokes the AI send gesture.
    ("ai-focused", true),
    ("move-to", true),
    ("workspace-icon", true),
    ("search-overlay", true),
];

/// Open a dictation and answer whether the container took it.
fn begin(v: &mut MainView, ws: &mut WorkspaceDoc, generation: u64) -> bool {
    v.dictate(ws, &DictationAction::Begin { generation })
}

fn update(v: &mut MainView, ws: &mut WorkspaceDoc, generation: u64, text: &str) -> bool {
    v.dictate(
        ws,
        &DictationAction::Update {
            generation,
            text: Transcript::new(text),
        },
    )
}

fn commit(v: &mut MainView, ws: &mut WorkspaceDoc, generation: u64, text: &str) -> bool {
    v.dictate(
        ws,
        &DictationAction::Commit {
            generation,
            text: Transcript::new(text),
        },
    )
}

/// **A dictation reaches the same surface a keystroke would.**
///
/// Driven through the same `Blockers`/`EditableTarget` pair the key path uses
/// and compared against the answers that table already records — never against
/// a second copy of the routing.
#[test]
fn a_dictation_and_a_keystroke_agree_about_what_is_blocking_and_where_text_lands() {
    for (state, opens) in OPENS {
        let (mut v, mut w) = arm(state);
        // What the key path says about this state, from the factored answers
        // rather than from a chain of its own.
        let target = v.editable_target();
        let blocking = v.blockers().is_blocking();
        let took = begin(&mut v, &mut w.ws, 1);
        assert_eq!(took, *opens, "dictating in {state}");
        if took {
            assert!(
                target.accepts_text(),
                "{state}: a dictation opened where a character has nowhere to go"
            );
            assert!(
                MainView::accepts_dictation(target),
                "{state}: a dictation opened into a surface that refuses one"
            );
        }
        // A modal swallows a character and a dictation alike. The prompts are
        // blocking *and* editable, which is exactly the distinction
        // `Blockers`/`EditableTarget` exists to carry.
        if blocking && !target.accepts_text() {
            assert!(!took, "{state}: a dictation reached past a modal");
        }
    }
}

#[test]
fn all_reminders_refuses_dictation_to_the_document_underneath() {
    let (mut v, mut w) = arm("all-reminders-overlay");
    let before = document_text(&v, &w.ws);
    assert_eq!(v.blockers(), Blockers::Modal(ModalKind::ReminderOverlay));
    assert_eq!(
        v.editable_target(),
        koru_workspace_host::routing::EditableTarget::None
    );
    assert!(!begin(&mut v, &mut w.ws, 19));
    assert!(v.dictation.is_none());
    assert_eq!(document_text(&v, &w.ws), before);
}

#[test]
fn reminder_creation_dictation_updates_its_field_and_never_the_document() {
    let (mut v, mut w) = arm("reminder-creation");
    let before = document_text(&v, &w.ws);
    assert!(begin(&mut v, &mut w.ws, 71));
    assert!(update(&mut v, &mut w.ws, 71, "tomorrow at nine"));
    assert!(commit(&mut v, &mut w.ws, 71, "tomorrow at nine"));
    assert_eq!(
        v.reminders
            .creation
            .as_ref()
            .map(|creation| creation.request.phrase.as_str()),
        Some("tomorrow at nine")
    );
    assert_eq!(document_text(&v, &w.ws), before);
}

#[test]
fn existing_reminder_dictation_updates_the_surface_owned_edit() {
    let (mut v, mut w) = arm("reminder-creation");
    let node = v.document_root().expect("document");
    let original = koru_editor::reminders::Draft::preserving(
        koru_editor::utils::when::ReminderDraft::Floating(koru_core::time::FloatingDateTime::new(
            chrono::NaiveDate::from_ymd_opt(2030, 9, 3)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )),
        Some(koru_editor::reminders::Tone::Soft),
    );
    v.reminders.creation = None;
    v.reminder_creation.open_existing_edit(
        &mut v.engine,
        node,
        Rect::new(80.0, 80.0, 120.0, 24.0),
        koru_store_core::ReminderId::new().to_storage(),
        original,
        false,
    );

    assert!(begin(&mut v, &mut w.ws, 73));
    assert!(update(&mut v, &mut w.ws, 73, "tomorrow at"));
    assert!(commit(&mut v, &mut w.ws, 73, "tomorrow at 9am"));
    assert!(v.reminders.creation.is_none());

    v.sync_reminder_creation_surface(&w.ws);
    let outcome = v
        .reminder_creation
        .key(&mut v.engine, &Key::Named(NamedKey::Enter), None, NONE);
    let Some(reminder_creation::Action::Management(
        koru_editor::reminders::ManagementIntent::Edit { draft, .. },
    )) = outcome.action
    else {
        panic!("dictated existing edit should be ready to save");
    };
    assert_eq!(
        draft.tone_override,
        Some(koru_editor::reminders::Tone::Soft)
    );
}

#[test]
fn reminder_management_refuses_keys_and_dictation_to_the_document() {
    let (mut v, mut w) = arm("reminder-management");
    let before = document_text(&v, &w.ws);
    assert_eq!(v.blockers(), Blockers::Modal(ModalKind::ReminderManagement));
    assert!(!begin(&mut v, &mut w.ws, 72));
    assert!(View::key(&mut v, &mut w.ws, &ch("x"), Some("x"), NONE));
    assert_eq!(document_text(&v, &w.ws), before);
}

/// **A secret field refuses**, and the refusal comes from the host.
///
/// Q-V25's answer: `TextInput` keeps `accepts_dictation`'s `true` default,
/// because one box is a search field in one place and an API key in another
/// and the only difference is which `draw` method the host calls. The host is
/// the thing that knows, so the host is where the `false` lives.
#[test]
fn the_device_link_code_field_refuses_a_dictation() {
    let (mut v, mut w) = arm("link-device");
    // A typed character does reach it — this is not a blocker question.
    assert_eq!(
        format!("{:?}", v.editable_target()),
        "Field(LinkDevice)",
        "the code box is still where a character goes"
    );
    assert!(!MainView::accepts_dictation(v.editable_target()));
    assert!(
        !begin(&mut v, &mut w.ws, 1),
        "a credential box takes no dictation"
    );
    // …and nothing is left half-open for the next one.
    assert!(v.dictation.is_none());
    assert!(!update(&mut v, &mut w.ws, 1, "one two three"));
}

/// **Re-running the router per partial cannot redirect a live session.**
///
/// The route is recorded at `Begin` and never re-derived. A surface that gains
/// focus mid-utterance — here, a prompt opening over the document — takes the
/// keyboard, and the rest of the sentence still goes where it started.
#[test]
fn a_surface_that_gains_focus_mid_session_does_not_take_the_sentence() {
    let (mut v, mut w) = arm("plain-document");
    let before = document_text(&v, &w.ws);
    assert!(begin(&mut v, &mut w.ws, 7));
    assert!(update(&mut v, &mut w.ws, 7, "hello"));

    // A prompt opens over the top. `editable_target` now says the prompt.
    v.new_document = Some(new_document::NewDocument::new(&mut v.engine));
    assert_eq!(
        format!("{:?}", v.editable_target()),
        "Field(NewDocument)",
        "a character would now reach the prompt"
    );

    assert!(commit(&mut v, &mut w.ws, 7, "hello there"));
    let after = document_text(&v, &w.ws);
    assert_ne!(after, before, "the sentence landed in the document");
    assert!(after.contains("hello there"), "{after}");
    assert!(
        v.new_document
            .as_mut()
            .is_some_and(|p| !p.dictation_field().value().contains("hello")),
        "and not in the prompt that opened over it"
    );
}

/// **A late `Final` from a cancelled session commits nothing.**
///
/// The generation is carried on every action for exactly this: the hard case
/// is not two dictations with different ids, it is one arriving after its own
/// was thrown away.
#[test]
fn a_late_final_from_a_cancelled_session_commits_nothing() {
    let (mut v, mut w) = arm("plain-document");
    let before = document_text(&v, &w.ws);
    assert!(begin(&mut v, &mut w.ws, 3));
    assert!(update(&mut v, &mut w.ws, 3, "half a sentence"));
    assert!(v.dictate(&mut w.ws, &DictationAction::Cancel { generation: 3 }));
    assert_eq!(
        document_text(&v, &w.ws),
        before,
        "a cancel leaves the document byte-identical"
    );

    // The next dictation opens while the old one's result is still in flight.
    assert!(begin(&mut v, &mut w.ws, 4));
    assert!(
        !commit(&mut v, &mut w.ws, 3, "the cancelled sentence"),
        "an action for a generation that is not the open one is dropped"
    );
    assert!(!document_text(&v, &w.ws).contains("cancelled"));
    assert!(v.dictate(&mut w.ws, &DictationAction::Cancel { generation: 4 }));
    assert_eq!(document_text(&v, &w.ws), before);
}

/// **A click or a keypress after release but before the `Final` discards, the
/// event lands immediately, and the document is unchanged.**
///
/// Driven through the controller and the router together, in the order the
/// shell drives them, because the contradiction this resolves is between the
/// two layers: Plan 2.3 says an ordinary local event *commits* an open
/// composition, and this plan says only a matching `Final` may. In the
/// finalising window no `Final` has arrived, so the controller pre-empts and
/// discards.
#[test]
fn an_event_in_the_finalising_window_discards_and_lands_immediately() {
    for interrupting in ["key", "click"] {
        let (mut v, mut w) = arm("plain-document");
        let before = document_text(&v, &w.ws);

        let mut controller = crate::mainview::harness::consenting_dictation();
        let now = std::time::Instant::now();
        let action = controller.press(now).expect("a session opens");
        let accepted = v.dictate(&mut w.ws, &action);
        controller.began(accepted);
        assert!(accepted);

        let live = koru_dictation::Live {
            session: controller.session().expect("a session"),
            generation: controller.generation().expect("a generation"),
        };
        controller.observe(koru_dictation::Event::Partial {
            session: live.session,
            generation: live.generation,
            text: Transcript::new("half a sentence"),
        });
        for action in controller.pump(now) {
            v.dictate(&mut w.ws, &action);
        }
        // The key comes up; the worker has not answered yet.
        assert!(controller.release(now));
        assert_eq!(controller.stage(), koru_dictation::Stage::Finalising);

        // …and something else happens. This is what the shell does on any
        // press or click while a dictation is live.
        controller.interrupt(Interruption::LocalEvent);
        for action in controller.pump(now) {
            v.dictate(&mut w.ws, &action);
        }
        assert_eq!(
            document_text(&v, &w.ws),
            before,
            "{interrupting}: the unfinished sentence left no trace"
        );

        // The event itself lands, immediately, with nothing held back.
        let landed = match interrupting {
            "key" => View::key(&mut v, &mut w.ws, &ch("x"), Some("x"), NONE),
            _ => View::mouse_down(&mut v, &mut w.ws, 400.0, 200.0, false),
        };
        assert!(
            landed || interrupting == "click",
            "{interrupting} was handled"
        );

        // …and the `Final` that was already in flight commits nothing.
        controller.observe(koru_dictation::Event::Final {
            session: live.session,
            generation: live.generation,
            text: Transcript::new("half a sentence, finished."),
        });
        controller.observe(koru_dictation::Event::Ended {
            session: live.session,
            generation: live.generation,
            reason: koru_dictation::EndReason::Completed,
        });
        for action in controller.pump(now) {
            v.dictate(&mut w.ws, &action);
        }
        assert!(
            !document_text(&v, &w.ws).contains("finished"),
            "{interrupting}: a final for a discarded session was committed"
        );
        assert_eq!(controller.stats().commits, 0);
    }
}

/// **Losing window focus mid-session discards and leaves the document
/// unchanged.** The shell's `Focused(false)` arm calls exactly this.
#[test]
fn losing_focus_mid_session_leaves_the_document_unchanged() {
    let (mut v, mut w) = arm("plain-document");
    let before = document_text(&v, &w.ws);

    let mut controller = crate::mainview::harness::consenting_dictation();
    let now = std::time::Instant::now();
    let action = controller.press(now).expect("a session opens");
    controller.began(v.dictate(&mut w.ws, &action));

    let live = koru_dictation::Live {
        session: controller.session().unwrap(),
        generation: controller.generation().unwrap(),
    };
    controller.observe(koru_dictation::Event::Partial {
        session: live.session,
        generation: live.generation,
        text: Transcript::new("mid sentence"),
    });
    for action in controller.pump(now) {
        v.dictate(&mut w.ws, &action);
    }

    controller.interrupt(Interruption::FocusLost);
    for action in controller.pump(now) {
        v.dictate(&mut w.ws, &action);
    }
    assert_eq!(document_text(&v, &w.ws), before);
    assert!(
        v.dictation.is_none(),
        "the router let go with the controller"
    );
    assert!(!controller.is_live());
}

/// A whole dictation into a one-line prompt, end to end: one edit, and the
/// provisional text was never the field's value.
#[test]
fn a_dictation_into_a_prompt_is_one_edit() {
    let (mut v, mut w) = arm("workspace-rename");
    assert!(begin(&mut v, &mut w.ws, 1));
    assert!(update(&mut v, &mut w.ws, 1, "Proj"));
    assert!(update(&mut v, &mut w.ws, 1, "Project"));
    assert!(commit(&mut v, &mut w.ws, 1, "Project plan"));
    assert!(v.dictation.is_none());
    assert_eq!(
        v.workspace_rename
            .as_mut()
            .map(|r| r.dictation_field().value())
            .unwrap_or_default(),
        "Project plan"
    );
}

#[test]
fn a_dictation_into_the_embedded_ai_composer_is_one_edit() {
    let (mut v, mut w) = arm("ai-focused");
    assert!(begin(&mut v, &mut w.ws, 21));
    assert!(update(&mut v, &mut w.ws, 21, "Draft"));
    assert!(update(&mut v, &mut w.ws, 21, "Draft an answer"));
    assert!(commit(
        &mut v,
        &mut w.ws,
        21,
        "Draft an answer\nwith sources"
    ));
    let composer = v.ai_chat.dictation_composer();
    assert_eq!(composer.text(), "Draft an answer\nwith sources");
}

#[test]
fn the_dedicated_global_search_window_accepts_a_dictation() {
    let (updates, _sent) =
        crate::agent::UpdateFeed::for_test(a_report(koru_agent_wire::UpdateStage::Idle));
    let mut v = MainView::search_window(updates);
    let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();

    assert!(begin(&mut v, &mut ws, 22));
    assert!(update(&mut v, &mut ws, 22, "global"));
    assert!(commit(&mut v, &mut ws, 22, "global search"));
    let (query, _) = v.search.as_mut().unwrap().dictation_field();
    assert_eq!(query.raw_text(), "global search");
}
