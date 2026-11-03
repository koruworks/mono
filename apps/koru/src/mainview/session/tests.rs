// SPDX-License-Identifier: MPL-2.0

//! What survives a restart, and what is deliberately not trusted on the way
//! back in.

use crate::mainview::harness::*;

use crate::mainview::MainView;
use koru_core::node::Node;

use koru_core::schema::Parent;

use koru_draw::theme;

use koru_workspace::Workspace;
use koru_workspace_host::{Settings, TabRef, View};
use winit::event::MouseScrollDelta;

// ----------------------------------------------------------- session restore

/// A fresh container over the same workspace, as `App::new` builds one.
fn reopened(workspace: &mut Workspace, settings: &Settings) -> MainView {
    let mut v = MainView::new();
    v.restore(&workspace, settings);
    v.set_viewport(1200.0, 700.0);
    v
}

/// The whole point of the feature: what you left is what you get back.
#[test]
fn a_session_reopens_with_its_tabs_panels_and_selection() {
    let (mut v, mut workspace) = on_document();
    let a = a_document(&mut workspace.ws, "a");
    let b = a_document(&mut workspace.ws, "b");
    v.open_node(&workspace.ws, a);
    v.open_node(&workspace.ws, b);
    v.toggle_sidebar();
    v.toggle_ai();
    v.set_ai_width(640.0);
    v.commit_ai_width();
    v.select(2);

    let mut settings = Settings::default();
    v.capture(&mut settings);
    // Through the file body, not just the struct — a field written by one and
    // not read by the other would pass a struct-only round-trip.
    let before: Vec<TabRef> = settings.tabs.iter().map(|(t, _)| t.clone()).collect();
    let notes = v
        .tabs()
        .get(1)
        .and_then(|t| t.view.root())
        .expect("the document tab");
    assert_eq!(
        before,
        vec![
            TabRef::Welcome,
            TabRef::Node(notes),
            TabRef::Node(a),
            TabRef::Node(b)
        ]
    );
    assert_eq!(settings.tab, 2);
    assert!(settings.sidebar_open);
    assert!(!settings.context_open);
    assert!(settings.ai_open);
    assert_eq!(settings.ai_w, 640.0);
    // Recents are store document ids; this harness has no manifest, so nothing
    // is recorded here (see `remember`).
    assert!(settings.recents.is_empty());

    let back = reopened(&mut workspace, &settings);
    assert_eq!(back.tabs().len(), 4);
    assert_eq!(
        back.tabs()
            .iter()
            .map(|t| t.label.as_str())
            .collect::<Vec<_>>(),
        ["Welcome", "Notes", "a", "b"]
    );
    assert_eq!(back.tabs().active(), 2);
    assert_eq!(back.document_root(), Some(a));
    assert!(back.sidebar.is_open());
    assert!(!back.context.is_open());
    assert!(back.ai_panel.is_open());
    assert_eq!(back.ai_panel.full(), 640.0);
    assert!(back.recents.is_empty());
}

/// **A tab is remembered by its document, not by the node under it.**
///
/// The `NodeId` a leaf is rooted at lives in *that document's* own tree, and the
/// next launch has not opened it — nothing can resolve it, so the tab would drop
/// itself and the strip would come back one shorter every time. The manifest's
/// id survives the trip, which is what the shell reopens from.
#[test]
fn a_document_tab_is_remembered_by_its_document() {
    let (mut v, mut workspace) = on_document();
    let doc = koru_core::DocumentId::new();
    let root = a_document(&mut workspace.ws, "Elsewhere");
    v.tabs_mut().push(crate::mainview::tab::Tab::document_of(
        crate::leaf::Leaf::document(&workspace.ws, root),
        "Elsewhere",
        Some(doc),
    ));

    let mut settings = Settings::default();
    v.capture(&mut settings);
    let refs: Vec<TabRef> = settings.tabs.iter().map(|(t, _)| t.clone()).collect();
    assert!(
        refs.iter().any(|t| t.document_id() == Some(doc)),
        "the document was not remembered: {refs:?}"
    );
    assert!(
        !refs.contains(&TabRef::Node(root)),
        "and not by the node under it, which means nothing next launch"
    );
}

/// A document binned by a peer — or a store that would not answer for it — is a
/// tab the shell could not reopen. It is **skipped**, which shortens the strip;
/// the alternative is a tab over nothing at all.
#[test]
fn a_restored_document_tab_the_workspace_does_not_hold_is_skipped() {
    let (v, mut workspace) = on_document();
    let mut settings = Settings::default();
    v.capture(&mut settings);
    let had = settings.tabs.len();
    settings
        .tabs
        .push((TabRef::document(koru_core::DocumentId::new(), None), 0.0));

    let back = reopened(&mut workspace, &settings);
    assert_eq!(back.tabs().len(), had, "a tab was opened over no document");
}

/// **The startup cost rule.** `restore` runs before any window exists, so
/// laying tabs out there would be at the default viewport and thrown away by
/// the first real `set_viewport` — ten tabs, twenty wasted parley passes, on
/// the one path where slowness is most visible.
#[test]
fn restoring_leaves_every_hidden_tab_unlaid_out() {
    let (mut v, mut workspace) = on_document();
    for t in ["a", "b", "c"] {
        let d = a_document(&mut workspace.ws, t);
        v.open_node(&workspace.ws, d);
    }
    let mut settings = Settings::default();
    v.capture(&mut settings);

    let back = reopened(&mut workspace, &settings);
    let active = back.tabs().active();
    for (i, t) in back.tabs().iter().enumerate() {
        if i == active {
            assert!(t.sized.is_some(), "the visible tab is laid out");
        } else {
            assert_eq!(t.sized, None, "tab {i} was laid out before being shown");
        }
    }
}

/// A workspace edited by another peer can delete a document this one had open.
/// Reopening onto a phantom is worse than not reopening it.
#[test]
fn a_restored_tab_whose_node_is_gone_is_dropped_and_the_index_clamps() {
    let (mut v, mut workspace) = on_document();
    let a = a_document(&mut workspace.ws, "a");
    let b = a_document(&mut workspace.ws, "b");
    v.open_node(&workspace.ws, a);
    v.open_node(&workspace.ws, b);
    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert_eq!(settings.tabs.len(), 4);

    // Both documents vanish before the next run.
    workspace.ws.delete(a).expect("delete");
    workspace.ws.delete(b).expect("delete");

    let back = reopened(&mut workspace, &settings);
    assert_eq!(back.tabs().len(), 2, "only Welcome and Notes survive");
    assert!(
        back.tabs().active() < back.tabs().len(),
        "the index stayed in range"
    );
}

/// Every restored tab named a document that has since gone. The window must
/// still open on something, and that something is a welcome tab — the floor
/// that replaced the permanent one.
#[test]
fn a_session_whose_documents_have_all_gone_reopens_on_welcome() {
    let (mut v, mut workspace) = on_document();
    let a = a_document(&mut workspace.ws, "a");
    v.open_node(&workspace.ws, a);
    let mut settings = Settings::default();
    v.capture(&mut settings);
    // Drop the welcome slot too, so nothing at all survives the filter.
    settings.tabs.retain(|(t, _)| !matches!(t, TabRef::Welcome));
    for (t, _) in settings.tabs.clone() {
        if let TabRef::Node(id) = t {
            workspace.ws.delete(id).expect("delete");
        }
    }

    let back = reopened(&mut workspace, &settings);
    assert_eq!(back.tabs().len(), 1);
    assert_eq!(back.tabs().active(), 0);
    assert_eq!(
        back.tabs().get(0).map(|t| t.label.as_str()),
        Some("Welcome")
    );
}

/// A settings file from a build that had never heard of tabs must still open —
/// on the welcome tab, with nothing else assumed.
#[test]
fn an_empty_session_reopens_on_a_welcome_tab() {
    let (_, mut workspace) = container();
    let back = reopened(&mut workspace, &Settings::default());
    assert_eq!(back.tabs().len(), 1);
    assert_eq!(
        back.tabs().get(0).map(|t| t.label.as_str()),
        Some("Welcome")
    );
    assert!(back.tabs().active() < back.tabs().len());
    // A new window starts with the explore column open (the CTO's note).
    assert!(back.sidebar.is_open());
}

#[test]
fn manage_profiles_never_restores_as_the_startup_destination() {
    let (_, mut workspace) = container();
    let settings = Settings {
        tabs: vec![(TabRef::Welcome, 0.0), (TabRef::ManageProfiles, 0.0)],
        tab: 1,
        ..Settings::default()
    };

    let back = reopened(&mut workspace, &settings);
    assert_eq!(
        back.tabs().len(),
        1,
        "the utility became a duplicate Welcome"
    );
    assert_eq!(back.tabs().active(), 0);
    assert!(matches!(
        back.tabs().get(0).expect("Welcome").view,
        crate::mainview::tab::TabView::Welcome(_)
    ));
}

/// Restoring must not animate: it is motion nobody asked for, and `animating`
/// would be true before the first frame had been drawn.
#[test]
fn restoring_an_open_column_does_not_animate() {
    let (_, mut workspace) = container();
    let settings = Settings {
        sidebar_open: true,
        context_open: true,
        ..Settings::default()
    };
    let back = reopened(&mut workspace, &settings);
    // The context column is not offered — see `CONTEXT_COLUMN` — so only the
    // pane comes back open. Both are still *set* rather than slid.
    assert!(back.sidebar.is_open());
    assert!(!back.animating(), "no slide on startup");
    assert_eq!(
        back.content().x,
        theme::SIDEBAR_W + theme::RAIL_W,
        "already in place"
    );
}

/// Scroll comes back too, so a long document reopens where you were reading.
#[test]
fn a_tabs_scroll_position_survives() {
    let (mut v, mut workspace) = on_document();
    let doc = v.document_root().expect("a document tab");
    for i in 0..100 {
        workspace
            .ws
            .insert(
                Parent::Node(doc),
                Node::paragraph(format!("row {i}")).unwrap(),
            )
            .unwrap();
    }
    v.adopt(&workspace.ws);
    let c = v.content();
    v.mouse_move(c.centre().0, c.centre().1);
    v.scroll_by(MouseScrollDelta::LineDelta(0.0, -8.0));

    let mut settings = Settings::default();
    v.capture(&mut settings);
    let scrolled = settings
        .tabs
        .iter()
        .find(|(t, _)| *t == TabRef::Node(doc))
        .map(|(_, s)| *s)
        .expect("the document tab");
    assert!(scrolled > 0.0, "the fixture actually scrolled");

    let back = reopened(&mut workspace, &settings);
    let i = back
        .tabs()
        .iter()
        .position(|t| t.view.root() == Some(doc))
        .expect("the document tab came back");
    assert!(
        (back.tabs().iter().nth(i).unwrap().view.scroll() - scrolled).abs() < 1.0,
        "reopened at {} rather than {scrolled}",
        back.tabs().iter().nth(i).unwrap().view.scroll()
    );
}

/// Splitting a pane creates independent tab groups, and the full tree survives
/// a capture/restore round trip so the layout reopens as it was left.
#[test]
fn a_split_pane_tree_survives_a_restart() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, mut workspace) = on_document();
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the document tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);

    assert_eq!(v.panes.iter_groups().count(), 2, "split into two groups");

    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert!(settings.pane_tree.is_some(), "pane tree was captured");

    let back = reopened(&mut workspace, &settings);
    assert_eq!(
        back.panes.iter_groups().count(),
        2,
        "restore recreated the split"
    );
    let labels: Vec<String> = back
        .panes
        .iter_groups()
        .flat_map(|(_, g)| g.iter_tabs().map(|t| t.label.clone()))
        .collect();
    assert!(labels.contains(&"Welcome".to_string()));
    assert!(labels.contains(&"Notes".to_string()));
}

/// A focused path that no longer names a group — a hand-edited or truncated
/// settings row — must not panic the open. Focus falls back to the first
/// pane, as it does when a pane collapses, and the split itself survives.
#[test]
fn a_split_tree_whose_focus_does_not_resolve_opens_on_its_first_pane() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, mut workspace) = on_document();
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the document tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);

    let mut settings = Settings::default();
    v.capture(&mut settings);

    // A path into panes that do not exist, and the empty path a truncated
    // `pane_tree` row yields — which under a split root names the split, not
    // a group.
    for focused in [vec![7, 7], Vec::new()] {
        settings
            .pane_tree
            .as_mut()
            .expect("pane tree was captured")
            .focused = focused;
        let back = reopened(&mut workspace, &settings);
        assert_eq!(back.panes.iter_groups().count(), 2, "the split survived");
        assert_eq!(
            back.panes.focused_path(),
            &back.panes.first_group_path(),
            "focus fell back to the first pane"
        );
        let _ = back.panes.focused();
    }
}

// ----------------------------------------------------------- session tabs

/// The first built-in profile, opened as a session tab in the harness (no
/// event loop, so no backend dials out), with its session id set as the
/// backend's `Attached` would set it.
fn with_session_tab(v: &mut MainView, session: koru_session_protocol::SessionId) {
    assert!(
        v.open_session_tab_for_profile(0, None),
        "a session tab opened"
    );
    for t in v.tabs_mut().iter_mut() {
        if let crate::mainview::tab::TabView::Session(s) = &mut t.view {
            s.session = Some(session);
        }
    }
}

/// The maintainers' case, at this container's scale: agent tabs open among document
/// tabs, close Koru, reopen — the same strip comes back, session tab in its
/// slot, selection intact, and the tab still knows which session it was over.
#[test]
fn a_session_tab_survives_capture_and_restore_between_document_tabs() {
    let (mut v, mut workspace) = on_document(); // [Welcome, Notes]
    let a = a_document(&mut workspace.ws, "a");
    v.open_node(&workspace.ws, a); // [Welcome, Notes, a]
    let session = koru_session_protocol::SessionId::mint();
    with_session_tab(&mut v, session); // [Welcome, Notes, a, session]
    let b = a_document(&mut workspace.ws, "b");
    v.tabs_mut().push(crate::mainview::tab::Tab::document(
        crate::leaf::Leaf::document(&workspace.ws, b),
        "b",
    )); // [Welcome, Notes, a, session, b]
    assert!(v.select(3), "the session tab is the one showing");

    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    let profile = koru_session_profiles::built_in_profiles()
        .into_iter()
        .next()
        .expect("a built-in profile");
    assert_eq!(
        settings.tabs[3].0,
        TabRef::Session {
            session,
            profile: profile.id.clone(),
            recovery: None,
        },
        "the record is {{session_id, profile_key}}, in tab order"
    );
    assert_eq!(settings.tab, 3, "and the selection is the session tab");

    let back = reopened(&mut workspace, &settings);
    assert_eq!(
        back.tabs()
            .iter()
            .map(|t| t.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Welcome",
            "Notes",
            "a",
            profile.fallback_display_name(),
            "b"
        ],
        "every tab returned, the session tab between its document neighbours"
    );
    assert_eq!(back.tabs().active(), 3, "still the one showing");
    let restored = back
        .tabs()
        .iter()
        .find_map(|t| match &t.view {
            crate::mainview::tab::TabView::Session(s) => Some(s),
            _ => None,
        })
        .expect("the session tab came back as a session tab");
    assert_eq!(restored.session, Some(session), "holding its saved session");
    assert_eq!(
        restored.profile_key.as_ref(),
        Some(&profile.id),
        "and the stable profile key that labels it before the catalog answers"
    );
    // No backend answered anything yet, so nothing may be presented as lost:
    // the absence of a first catalog is not confirmed loss (W3b's handshake
    // decides), and only a confirmed end closes a tab.
    assert!(
        !matches!(
            restored.surface.status,
            koru_agents_ui::PaneStatus::Exited | koru_agents_ui::PaneStatus::Failed
        ),
        "startup silence must not be presented as a lost session"
    );
}

/// A session tab whose launch has not come back named holds no session id to
/// save. It skips itself; its neighbours are untouched.
#[test]
fn a_session_tab_still_launching_is_not_captured_but_its_neighbours_are() {
    let (mut v, mut workspace) = on_document();
    assert!(v.open_session_tab_for_profile(0, None));
    let a = a_document(&mut workspace.ws, "a");
    v.tabs_mut().push(crate::mainview::tab::Tab::document(
        crate::leaf::Leaf::document(&workspace.ws, a),
        "a",
    ));

    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    assert!(
        !settings
            .tabs
            .iter()
            .any(|(t, _)| matches!(t, TabRef::Session { .. })),
        "nothing to save yet: no session id has been named"
    );
    assert_eq!(
        settings.tabs.last().map(|(t, _)| t.clone()),
        Some(TabRef::Node(a)),
        "the neighbouring document tab is untouched"
    );
}

// ----------------------------------------------------------- chat tabs (D23)

/// What a tab is, for comparing a strip across a restart: a chat by the chat
/// it is over, anything else by its label.
fn tab_kind(tab: &crate::mainview::tab::Tab) -> String {
    match &tab.view {
        crate::mainview::tab::TabView::AiChat(chat) => format!("chat {:?}", chat.chat_id()),
        _ => tab.label.clone(),
    }
}

/// Every column's strip and which tab it shows, in tree order.
fn columns(v: &MainView) -> Vec<(Vec<String>, usize)> {
    v.panes
        .iter_groups()
        .map(|(_, g)| (g.iter_tabs().map(tab_kind).collect(), g.tabs().active()))
        .collect()
}

/// **A chat tab comes back on its chat** (ADR-483-chattabrestore), in its
/// column, at its place, showing if it was — here in a split whose left
/// column mixes Welcome and a chat and whose right mixes a document and a
/// chat.
#[test]
fn chat_tabs_survive_a_restart_in_a_split_beside_document_tabs() {
    use crate::mainview::panes::{PanePath, SplitDir};
    use crate::mainview::tab::Tab;

    let (mut v, mut workspace) = on_document(); // [Welcome, Notes]
    let (left, right) = (koru_ai_ui::ChatId::new(), koru_ai_ui::ChatId::new());
    v.tabs_mut().push(Tab::ai_chat_existing(left)); // [Welcome, Notes, left]
    let notes = v
        .tabs()
        .iter()
        .find(|t| t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the document tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);
    for (_, group) in v.panes.iter_groups_mut() {
        if group.iter_tabs().any(|t| t.label == "Notes") {
            group.tabs_mut().push(Tab::ai_chat_existing(right));
        }
    }
    let before = columns(&v);
    assert_eq!(before.len(), 2, "two columns");
    let chat = |id: koru_ai_ui::ChatId| format!("chat {:?}", Some(id));
    assert!(before[0].0.contains(&chat(left)) && before[1].0.contains(&chat(right)));

    let mut settings = Settings::default();
    v.capture(&mut settings);
    // The flat list is the focused column's copy; the tree has both.
    let tree = format!("{:?}", settings.pane_tree);
    for id in [left, right] {
        assert!(
            tree.contains(&format!("{:?}", TabRef::Chat(id.as_uuid()))),
            "{tree}"
        );
    }

    let back = reopened(&mut workspace, &settings);
    assert_eq!(
        columns(&back),
        before,
        "every column, every tab and its showing one"
    );
}

/// **A new chat not sent yet is not saved**: the service holds nothing to
/// reopen. Its neighbours are untouched.
#[test]
fn an_unsent_chat_tab_is_not_saved_but_its_neighbours_are() {
    let (mut v, mut workspace) = on_document(); // [Welcome, Notes]
    v.tabs_mut().push(crate::mainview::tab::Tab::ai_chat());
    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert_eq!(
        settings.tabs.len(),
        2,
        "Welcome and Notes: {:?}",
        settings.tabs
    );
    let tree = format!("{:?}", settings.pane_tree);
    assert!(
        !tree.contains("Chat("),
        "nothing in the tree either: {tree}"
    );
    let back = reopened(&mut workspace, &settings);
    assert_eq!(
        back.tabs().iter().map(tab_kind).collect::<Vec<_>>(),
        ["Welcome", "Notes"]
    );
}

/// A pinned chat tab is pinned again after a restart, on its chat.
#[test]
fn a_pinned_chat_tab_comes_back_pinned() {
    let (mut v, mut workspace) = on_document(); // [Welcome, Notes]
    let chat = koru_ai_ui::ChatId::new();
    v.tabs_mut()
        .push(crate::mainview::tab::Tab::ai_chat_existing(chat));
    let at = v.tabs().len() - 1;
    assert!(v.tabs_mut().pin(at, uuid::Uuid::new_v4(), true), "pinned");
    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert_eq!(
        settings.pinned_tabs.first().map(|pin| pin.target),
        Some(koru_workspace_host::PinnedTarget::Chat(chat.as_uuid()))
    );
    let back = reopened(&mut workspace, &settings);
    let pinned: Vec<String> = back
        .tabs()
        .iter()
        .filter(|t| t.is_pinned())
        .map(tab_kind)
        .collect();
    assert_eq!(pinned, [format!("chat {:?}", Some(chat))]);
}

/// **A chat nothing answers for does not stop the restore.** A saved chat
/// that was deleted since comes back as a chat tab opening it, beside every
/// other tab; the AI service's answer — *missing* — then turns it into a new
/// chat (`koru_ai_ui`'s boot), on a later pass and off this thread.
#[test]
fn a_saved_chat_that_is_gone_restores_without_a_panic() {
    let (v, mut workspace) = on_document();
    let mut settings = Settings::default();
    v.capture(&mut settings);
    let gone = uuid::Uuid::new_v4();
    settings.pane_tree = None;
    settings.tabs.push((TabRef::Chat(gone), 0.0));
    settings.tab = settings.tabs.len() - 1;
    let back = reopened(&mut workspace, &settings);
    assert_eq!(
        back.tabs().iter().map(tab_kind).collect::<Vec<_>>(),
        [
            "Welcome".to_owned(),
            "Notes".to_owned(),
            format!("chat {:?}", Some(koru_ai_ui::ChatId::from_uuid(gone)))
        ]
    );
    assert_eq!(back.tabs().active(), 2, "and it is the one showing");
}

// ------------------------------------------------ the Agents tree's verbs
//
// P5: every project mutation the tree offers is a typed backend verb — the
// container keeps no project truth, and the tree redraws off the catalog the
// host answers with (ADR-264, Q-E6). These drive the gestures against a test
// backend whose command channel the suite holds, and read the verbs off it.

use crate::mainview::context_menu::{self, ContextMenu, MenuAction, MenuSubject};
use crate::panes::Outcome;
use koru_session_backend::BackendCommand;
use koru_session_protocol::{ProjectId, ProjectSummary, SessionId, SessionSummary};

fn a_project_summary(name: &str) -> ProjectSummary {
    ProjectSummary {
        id: ProjectId::mint(),
        name: name.to_owned(),
        root_directory: None,
        workspace_scope: None,
        created_at_ms: 1,
    }
}

fn drain(commands: &std::sync::mpsc::Receiver<BackendCommand>) -> Vec<BackendCommand> {
    std::iter::from_fn(|| commands.try_recv().ok()).collect()
}

/// The `+`'s prompt commits a name, and what leaves is `CreateProject` with a
/// **client-minted id** (replay-safe: the same id with the same content is the
/// same project) and the window's own workspace scope.
#[test]
fn creating_a_project_sends_the_create_verb_with_a_minted_id() {
    let (mut v, mut workspace) = container();
    let commands = v.install_test_session_backend();
    // The window's identity, as the pane holds it: the scope a create carries.
    let profile = koru_core::ProfileId::new();
    let workspace_id = koru_core::WorkspaceId::new();
    let mut me = koru_core::ProfileSummary::default();
    me.id = profile;
    let entry = koru_core::WorkspaceEntry::new(workspace_id, "Example", 0);
    v.explore
        .set_context(vec![me], profile, vec![entry], workspace_id);

    let ws = &mut workspace.ws;
    assert!(v.apply(ws, Outcome::NewAgentProject));
    for c in ["A", "l", "p", "h", "a"] {
        v.new_document_key(ws, &ch(c), Some(c), NONE);
    }
    v.new_document_key(
        ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );

    let sent = drain(&commands);
    let create = sent
        .iter()
        .find_map(|c| match c {
            BackendCommand::CreateProject {
                project_id,
                name,
                workspace_scope,
            } => Some((*project_id, name.clone(), *workspace_scope)),
            _ => None,
        })
        .expect("the create verb left on the wire");
    assert_eq!(create.1, "Alpha");
    let scope = create.2.expect("scoped to the window's workspace");
    assert_eq!(scope.profile.as_uuid(), profile.as_uuid());
    assert_eq!(scope.workspace.as_uuid(), workspace_id.as_uuid());
}

/// An empty name is not a project name: Enter keeps the prompt open and no
/// verb leaves — Koru Terminal's refusal, without a second sentence.
#[test]
fn an_empty_project_name_is_refused_rather_than_created() {
    let (mut v, mut workspace) = container();
    let commands = v.install_test_session_backend();
    let ws = &mut workspace.ws;
    assert!(v.apply(ws, Outcome::NewAgentProject));
    v.new_document_key(
        ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    assert!(v.new_document.is_some(), "the prompt stays open");
    assert!(drain(&commands).is_empty(), "and nothing left on the wire");
}

/// *New session ▸* from a project row launches **into that project**: the
/// placement rides the pending launch and leaves on the settle pass's Launch.
#[test]
fn new_session_from_a_project_row_launches_with_that_project_id() {
    let (mut v, _workspace) = container();
    let commands = v.install_test_session_backend();
    let project = a_project_summary("Alpha");
    let id = project.id;
    v.explore.set_agent_projects(vec![project]);

    assert!(v.run_menu_action(
        MenuSubject::AgentTree { project: Some(id) },
        MenuAction::TerminalProfile(0),
    ));
    let launch = drain(&commands)
        .into_iter()
        .find_map(|c| match c {
            BackendCommand::Launch { project_id, .. } => Some(project_id),
            _ => None,
        })
        .expect("the settle pass sent the launch");
    assert_eq!(launch, Some(id), "placed in the project the row named");

    // The `+` menu's own row still launches into the bucket.
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::TerminalProfile(0)));
    let launch = drain(&commands)
        .into_iter()
        .find_map(|c| match c {
            BackendCommand::Launch { project_id, .. } => Some(project_id),
            _ => None,
        })
        .expect("the + menu's launch");
    assert_eq!(launch, None, "no placement: Unassigned");
}

/// Rename… seeds the prompt with the registry's name and commits the typed
/// rename verb; Delete project asks first and a yes sends the delete verb.
#[test]
fn rename_and_delete_call_their_project_verbs() {
    let (mut v, mut workspace) = container();
    let commands = v.install_test_session_backend();
    let project = a_project_summary("Alpha");
    let id = project.id;
    v.explore.set_agent_projects(vec![project]);

    let ws = &mut workspace.ws;
    assert!(v.run_menu_action(
        MenuSubject::AgentTree { project: Some(id) },
        MenuAction::Rename,
    ));
    // The field is seeded with "Alpha"; append a character and commit.
    v.new_document_key(ws, &ch("2"), Some("2"), NONE);
    v.new_document_key(
        ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    match drain(&commands).as_slice() {
        [BackendCommand::RenameProject { project_id, name }] => {
            assert_eq!(*project_id, id);
            assert_eq!(name, "Alpha2", "seeded with what was already true");
        }
        other => panic!("expected the rename verb, got {other:?}"),
    }

    assert!(v.run_menu_action(
        MenuSubject::AgentTree { project: Some(id) },
        MenuAction::DeleteProject,
    ));
    assert!(v.confirm.is_some(), "delete asks first — it is destructive");
    assert!(
        drain(&commands).is_empty(),
        "and nothing left before the yes"
    );
    assert!(v.settle_confirm(true));
    match drain(&commands).as_slice() {
        [BackendCommand::DeleteProject { project_id }] => assert_eq!(*project_id, id),
        other => panic!("expected the delete verb, got {other:?}"),
    }
}

/// Koru Terminal's refusal, mirrored: a project with running sessions dims
/// *Delete project* (the host would refuse anyway; the menu says so first),
/// and cancelling the confirm sends nothing.
#[test]
fn delete_is_refused_while_the_project_has_running_sessions() {
    let (mut v, _workspace) = container();
    let commands = v.install_test_session_backend();
    let project = a_project_summary("Alpha");
    let id = project.id;
    v.explore.set_agent_projects(vec![project]);
    v.explore.set_agent_sessions(vec![{
        use koru_session_protocol::*;
        SessionSummary {
            id: SessionId::mint(),
            parent_session_id: None,
            profile: SessionProfileKey::BuiltIn("bash".parse::<BuiltInProfileKey>().unwrap()),
            display_name: "one".to_owned(),
            working_directory: None,
            mode: SessionMode::RawPty,
            process: ProcessState::Running,
            controller: ControllerState::Detached,
            managed: None,
            active_turn_id: None,
            attention: AttentionState::default(),
            pending_attachment_count: 0,
            capabilities: CapabilitySet::default(),
            project_id: Some(id),
            handover: Default::default(),
            model: None,
            origin: Default::default(),
            created_at_ms: 1,
        }
    }]);

    let menu = crate::mainview::context_menu::ContextMenu::new(
        &mut v.engine,
        MenuSubject::AgentTree { project: Some(id) },
        (100.0, 100.0),
        crate::mainview::context_menu::agent_tree_items(true),
    );
    let delete = menu
        .items()
        .iter()
        .position(|a| *a == MenuAction::DeleteProject)
        .expect("the row is offered");
    assert_eq!(
        v.enabled_items(&menu).get(delete),
        Some(&false),
        "dimmed, with its reason, rather than hidden"
    );

    // And a cancel after the ask costs nothing.
    assert!(v.confirm_delete_agent_project(id));
    assert!(v.settle_confirm(false));
    assert!(drain(&commands).is_empty(), "no verb without a yes");
}

/// The drag's landing: a session dropped on a project row becomes the typed
/// move verb (`AssignProject`), with `None` for a drop on the bucket.
#[test]
fn a_session_dropped_on_a_project_row_sends_the_move_verb() {
    let (mut v, workspace) = container();
    let commands = v.install_test_session_backend();
    let session = koru_session_protocol::SessionId::mint();
    let project = ProjectId::mint();

    assert!(v.apply(
        &workspace.ws,
        Outcome::MoveSessionToProject {
            session,
            project: Some(project),
        },
    ));
    assert!(v.apply(
        &workspace.ws,
        Outcome::MoveSessionToProject {
            session,
            project: None,
        },
    ));
    match drain(&commands).as_slice() {
        [
            BackendCommand::AssignProject {
                session_id: a,
                project_id: pa,
            },
            BackendCommand::AssignProject {
                session_id: b,
                project_id: pb,
            },
        ] => {
            assert_eq!((*a, *pa), (session, Some(project)));
            assert_eq!((*b, *pb), (session, None));
        }
        other => panic!("expected two move verbs, got {other:?}"),
    }
}

// ------------------------------------------------ the session row's menu (P8)
//
// Q-E7 P8: a session row raises Koru Terminal's session menu, and each item
// drives the right verb or Outcome. These read the verbs off a test backend
// whose command channel the suite holds, exactly as the project tests do.

fn a_agent_session(id: SessionId, name: &str) -> SessionSummary {
    use koru_session_protocol::*;
    let mut session = a_running_session(id, None);
    session.display_name = name.to_owned();
    session.profile = SessionProfileKey::BuiltIn("claude".parse::<BuiltInProfileKey>().unwrap());
    session
}

fn a_running_session(id: SessionId, project: Option<ProjectId>) -> SessionSummary {
    use koru_session_protocol::*;
    SessionSummary {
        id,
        parent_session_id: None,
        profile: SessionProfileKey::BuiltIn("bash".parse::<BuiltInProfileKey>().unwrap()),
        display_name: "one".to_owned(),
        working_directory: None,
        mode: SessionMode::RawPty,
        process: ProcessState::Running,
        controller: ControllerState::Detached,
        managed: None,
        active_turn_id: None,
        attention: AttentionState::default(),
        pending_attachment_count: 0,
        capabilities: CapabilitySet::default(),
        project_id: project,
        handover: Default::default(),
        model: None,
        origin: Default::default(),
        created_at_ms: 1,
    }
}

/// Koru Terminal's four items, in its order (Q-E7 P8).
#[test]
fn the_session_menu_offers_the_original_four_items() {
    assert_eq!(
        context_menu::session_items(),
        vec![
            MenuAction::SessionOpenInNewTab,
            MenuAction::SessionMoveToNewWindow,
            MenuAction::SessionMoveToProject,
            MenuAction::SessionTerminate,
        ]
    );
}

/// Opening Add… snapshots live sessions into *Active ▸* under Shell or
/// Agents, partitioned by launch-profile kind — a RawPty Claude still sits
/// under Agents, not Shell.
#[test]
fn the_plus_menu_lists_running_sessions_in_their_kind_flyout() {
    let (mut v, mut workspace) = container();
    let shell = SessionId::mint();
    let agent = SessionId::mint();
    v.explore.set_agent_sessions(vec![
        a_running_session(shell, None),
        a_agent_session(agent, "Claude"),
    ]);
    assert!(v.key(&mut workspace.ws, &ch("a"), Some("a"), PRIMARY_SHIFT));

    let shells = context_menu::submenu(MenuAction::NewShell).expect("Shell flyout");
    assert_eq!(shells[0], MenuAction::ActiveShells);
    assert_eq!(shells[1], MenuAction::Divider);
    assert!(!shells.contains(&MenuAction::OpenRunningSession(shell)));
    assert!(!shells.contains(&MenuAction::OpenRunningSession(agent)));
    assert_eq!(
        context_menu::submenu(MenuAction::ActiveShells),
        Some(vec![MenuAction::OpenRunningSession(shell)])
    );

    let agents = context_menu::submenu(MenuAction::NewAgent).expect("Agents flyout");
    assert_eq!(agents[0], MenuAction::ActiveAgents);
    assert_eq!(agents[1], MenuAction::Divider);
    assert!(!agents.contains(&MenuAction::OpenRunningSession(agent)));
    assert_eq!(
        context_menu::submenu(MenuAction::ActiveAgents),
        Some(vec![MenuAction::OpenRunningSession(agent)])
    );

    context_menu::set_plus_snapshots(vec![], vec![], vec![], vec![]);
}

/// Hovering *Active* opens a second flyout of the running sessions — the
/// menu widget only holds one flyout, so this is a third panel on the
/// container, not a nested widget.
#[test]
fn the_plus_menu_active_row_opens_the_running_list() {
    let (mut v, mut workspace) = container();
    let shell = SessionId::mint();
    let agent = SessionId::mint();
    v.explore.set_agent_sessions(vec![
        a_running_session(shell, None),
        a_agent_session(agent, "Claude"),
    ]);
    assert!(v.key(&mut workspace.ws, &ch("a"), Some("a"), PRIMARY_SHIFT));

    let agents = v
        .menu
        .as_ref()
        .unwrap()
        .items()
        .iter()
        .position(|a| *a == MenuAction::NewAgent)
        .expect("Agents row");
    assert!(v.open_submenu(agents));
    let active = v
        .submenu
        .as_ref()
        .unwrap()
        .1
        .items()
        .iter()
        .position(|a| *a == MenuAction::ActiveAgents)
        .expect("Active row");
    assert!(v.open_nested_submenu(active));
    assert_eq!(
        v.nested.as_ref().unwrap().1.items(),
        &[MenuAction::OpenRunningSession(agent)]
    );

    let shells = v
        .menu
        .as_ref()
        .unwrap()
        .items()
        .iter()
        .position(|a| *a == MenuAction::NewShell)
        .expect("Shell row");
    assert!(v.open_submenu(shells));
    assert!(
        v.nested.is_none(),
        "switching flyout closes the nested list"
    );
    let active = v
        .submenu
        .as_ref()
        .unwrap()
        .1
        .items()
        .iter()
        .position(|a| *a == MenuAction::ActiveShells)
        .expect("Active row");
    assert!(v.open_nested_submenu(active));
    assert_eq!(
        v.nested.as_ref().unwrap().1.items(),
        &[MenuAction::OpenRunningSession(shell)]
    );

    context_menu::set_plus_snapshots(vec![], vec![], vec![], vec![]);
}

/// Picking a running session from Add… attaches (or focuses) it, the same
/// as clicking the row in the Agents tree.
#[test]
fn the_plus_menu_opens_a_running_session() {
    let (mut v, _workspace) = container();
    v.install_test_session_backend();
    let session = SessionId::mint();
    v.explore
        .set_agent_sessions(vec![a_running_session(session, None)]);
    let before = v.tabs().len();
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenRunningSession(session),));
    assert_eq!(v.tabs().len(), before + 1, "a session tab was opened");
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenRunningSession(session),));
    assert_eq!(
        v.tabs().len(),
        before + 1,
        "the same session does not mint a second tab"
    );
}

/// *Open in new tab* opens (or focuses) the session's tab — the same attach
/// clicking the row does (`open_session_tab_for_existing`), never a launch.
#[test]
fn open_in_new_tab_opens_the_session_tab() {
    let (mut v, _workspace) = container();
    v.install_test_session_backend();
    let session = SessionId::mint();
    v.explore
        .set_agent_sessions(vec![a_running_session(session, None)]);
    let before = v.tabs().len();
    assert!(v.run_menu_action(
        MenuSubject::AgentSession { session },
        MenuAction::SessionOpenInNewTab,
    ));
    assert_eq!(v.tabs().len(), before + 1, "a session tab was opened");
}

/// *Move to new window* detaches here and asks the shell for a window seated on
/// the session — the pop-out the shell drains.
#[test]
fn move_to_new_window_asks_the_shell_for_a_window() {
    use koru_workspace_host::View;
    let (mut v, _workspace) = container();
    v.install_test_session_backend();
    let session = SessionId::mint();
    v.explore
        .set_agent_sessions(vec![a_running_session(session, None)]);
    assert!(v.run_menu_action(
        MenuSubject::AgentSession { session },
        MenuAction::SessionMoveToNewWindow,
    ));
    assert_eq!(
        View::take_wants_session_popout(&mut v),
        Some(session),
        "the shell is asked to open a window for this session"
    );
}

/// *Move to project ▸* sends `AssignProject` — the same verb P5's drag uses —
/// with the chosen project, and `None` for *No project*.
#[test]
fn move_to_project_sends_assign_project() {
    let (mut v, _workspace) = container();
    let commands = v.install_test_session_backend();
    let session = SessionId::mint();
    let project = ProjectId::mint();
    assert!(v.run_menu_action(
        MenuSubject::AgentSession { session },
        MenuAction::SessionAssignProject(Some(project)),
    ));
    assert!(v.run_menu_action(
        MenuSubject::AgentSession { session },
        MenuAction::SessionAssignProject(None),
    ));
    match drain(&commands).as_slice() {
        [
            BackendCommand::AssignProject {
                session_id: a,
                project_id: pa,
            },
            BackendCommand::AssignProject {
                session_id: b,
                project_id: pb,
            },
        ] => {
            assert_eq!((*a, *pa), (session, Some(project)));
            assert_eq!((*b, *pb), (session, None));
        }
        other => panic!("expected two assign verbs, got {other:?}"),
    }
}

/// The project the session is already in is dimmed in the flyout — the check is
/// the explanation, Koru Terminal's `destination_rows`. *No project* and the
/// other projects stay choosable.
#[test]
fn move_to_project_dims_the_current_project() {
    let (mut v, _workspace) = container();
    v.install_test_session_backend();
    let session = SessionId::mint();
    let alpha = a_project_summary("Alpha");
    let beta = a_project_summary("Beta");
    let (alpha_id, beta_id) = (alpha.id, beta.id);
    v.explore.set_agent_projects(vec![alpha, beta]);
    v.explore
        .set_agent_sessions(vec![a_running_session(session, Some(alpha_id))]);

    // The container snapshots the names as it opens the menu.
    context_menu::set_session_project_names(v.explore.agent_menu_projects());
    let submenu = ContextMenu::new(
        &mut v.engine,
        MenuSubject::AgentSession { session },
        (0.0, 0.0),
        context_menu::submenu(MenuAction::SessionMoveToProject).expect("the flyout"),
    );
    let enabled = v.enabled_items(&submenu);
    let index = |target| {
        submenu
            .items()
            .iter()
            .position(|a| *a == MenuAction::SessionAssignProject(target))
            .expect("the row is offered")
    };
    assert_eq!(
        enabled[index(Some(alpha_id))],
        false,
        "the current project is dimmed"
    );
    assert_eq!(enabled[index(Some(beta_id))], true, "another project moves");
    assert_eq!(
        enabled[index(None)],
        true,
        "still an option: out of every project"
    );
}

/// *Terminate* is refused for a session another window controls, and allowed
/// for a running, detached one — Koru Terminal's `can_terminate_session`.
#[test]
fn terminate_is_refused_when_the_session_is_controlled_elsewhere() {
    let (mut v, _workspace) = container();
    v.install_test_session_backend();
    let session = SessionId::mint();
    let mut controlled = a_running_session(session, None);
    controlled.controller = koru_session_protocol::ControllerState::Controlled;
    v.explore.set_agent_sessions(vec![controlled]);
    assert!(
        v.session_terminate_refusal(session).is_some(),
        "controlled elsewhere: refused with a reason"
    );
    v.explore
        .set_agent_sessions(vec![a_running_session(session, None)]);
    assert!(
        v.session_terminate_refusal(session).is_none(),
        "running and detached: it can be terminated"
    );
}

/// *Terminate* on a session this window does not control takes the controller
/// first: it opens the session's tab and arms the confirm, sending nothing on
/// the wire until the attach lands and the person says yes.
#[test]
fn terminate_when_not_attached_takes_the_controller_first() {
    let (mut v, _workspace) = container();
    let commands = v.install_test_session_backend();
    let session = SessionId::mint();
    v.explore
        .set_agent_sessions(vec![a_running_session(session, None)]);
    let before = v.tabs().len();
    assert!(v.run_menu_action(
        MenuSubject::AgentSession { session },
        MenuAction::SessionTerminate,
    ));
    assert_eq!(
        v.tabs().len(),
        before + 1,
        "the session's tab was opened to take the controller"
    );
    assert!(
        v.confirm.is_none(),
        "the confirm waits until the attach lands"
    );
    assert!(
        drain(&commands)
            .iter()
            .all(|c| !matches!(c, BackendCommand::Terminate)),
        "nothing was terminated before the attach and the yes"
    );
}

/// A document's view mode is persisted per-document — but only when
/// `settings.workspace` is set, which is the key `doc_view_prefs` is filed
/// under. This is the condition `capture_geometry` now establishes in the live
/// app; without it, `upsert_doc_prefs` no-ops and the colour/mode is lost.
#[test]
fn a_documents_view_mode_is_persisted_when_the_workspace_is_known() {
    use koru_core::{WorkspaceDoc, WorkspaceId};
    use koru_draw::theme::ViewMode;
    use koru_workspace::Workspace;
    use winit::keyboard::{Key, NamedKey};

    let ws_id = WorkspaceId::new();
    let ws = WorkspaceDoc::create(ws_id, "t").expect("create");
    let mut workspace = Workspace::new(ws);
    let mut v = MainView::new();
    v.set_viewport(1200.0, 700.0);
    let doc = a_document(&mut workspace.ws, "Notes");
    v.open_node(&workspace.ws, doc);
    // Narrow is the default; F7 steps Page -> Narrow -> Full, so twice from
    // Narrow lands on Page — a real override to capture.
    v.key(&mut workspace.ws, &Key::Named(NamedKey::F7), None, NONE);
    v.key(&mut workspace.ws, &Key::Named(NamedKey::F7), None, NONE);

    let mut settings = Settings::default();
    settings.workspace = Some(ws_id);
    v.capture(&mut settings);
    let prefs = settings
        .doc_view_prefs
        .iter()
        .flat_map(|(_, e)| e.iter())
        .find(|(n, _)| *n == doc)
        .map(|(_, p)| p);
    assert_eq!(
        prefs.and_then(|p| p.mode),
        Some(ViewMode::Page),
        "the document's Page override was captured per-document"
    );

    // And it reopens in Page, not the Narrow default.
    let back = reopened(&mut workspace, &settings);
    let i = back
        .tabs()
        .iter()
        .position(|t| t.view.root() == Some(doc))
        .expect("the document tab came back");
    let mode = back
        .tabs()
        .iter()
        .nth(i)
        .and_then(|t| match &t.view {
            crate::mainview::tab::TabView::Leaf(l) => Some(koru_editor::Editing::view(l).view_mode),
            _ => None,
        })
        .expect("a leaf");
    assert_eq!(mode, ViewMode::Page, "the override was restored on reopen");
}

/// The settings authority answers *after* `restore`, and the profile file no
/// longer carries the default width — so a document that follows the default
/// is restored at the registry's `Full` whatever the person chose. The first
/// answer has to move it, or the next capture writes that `Full` back as an
/// override of the document's own and it opens Full on every later launch:
/// the "Apply to all does not survive a restart" bug.
#[test]
fn the_authoritys_first_answer_reaches_documents_that_follow_the_default() {
    use koru_core::{NodeId, WorkspaceDoc, WorkspaceId};
    use koru_draw::theme::ViewMode;
    use koru_workspace::Workspace;
    use koru_workspace_host::DocViewPrefs;

    fn mode_of(v: &MainView, root: NodeId) -> Option<ViewMode> {
        v.tabs()
            .iter()
            .find(|t| t.view.root() == Some(root))
            .and_then(|t| match &t.view {
                TabView::Leaf(l) => Some(koru_editor::Editing::view(l).view_mode),
                _ => None,
            })
    }

    let ws_id = WorkspaceId::new();
    let ws = WorkspaceDoc::create(ws_id, "t").expect("create");
    let mut workspace = Workspace::new(ws);
    let follows = a_document(&mut workspace.ws, "Follows the default");
    let pinned = a_document(&mut workspace.ws, "Pinned to Page");

    // What the last run left behind: two open documents, one with an override.
    let mut remembered = {
        let mut v = MainView::new();
        v.set_viewport(1200.0, 700.0);
        v.open_node(&workspace.ws, follows);
        v.open_node(&workspace.ws, pinned);
        let mut s = Settings::default();
        s.workspace = Some(ws_id);
        v.capture(&mut s);
        s
    };
    remembered.doc_view_prefs = vec![(
        ws_id,
        vec![(
            pinned,
            DocViewPrefs {
                mode: Some(ViewMode::Page),
                canvas_bg: None,
            },
        )],
    )];

    // Startup: the profile file has no default width, so `restore` sees Full.
    remembered.view_mode = ViewMode::Full;
    let mut v = reopened(&mut workspace, &remembered);
    assert_eq!(mode_of(&v, follows), Some(ViewMode::Full));

    // Then the authority says Narrow.
    let mut adopted = remembered.clone();
    adopted.view_mode = ViewMode::Narrow;
    v.adopt_startup_preferences(&adopted);
    assert_eq!(
        mode_of(&v, follows),
        Some(ViewMode::Narrow),
        "a document following the default now shows the person's default"
    );
    assert_eq!(
        mode_of(&v, pinned),
        Some(ViewMode::Page),
        "a document with an override of its own keeps it"
    );

    // And the next capture pins nothing behind the person's back.
    let mut captured = adopted.clone();
    v.capture(&mut captured);
    assert!(
        captured.doc_view_prefs_for(follows).is_none(),
        "no Full override was written for the document that follows the default"
    );
    assert_eq!(
        captured.doc_view_prefs_for(pinned).and_then(|p| p.mode),
        Some(ViewMode::Page),
        "the real override is still there"
    );
}

// ---------------------------------------- watching a delegated run's session

use crate::mainview::tab::TabView;
use koru_session_backend::BackendEvent;

/// A run's row in the navigator, naming the session it is being driven in.
fn a_run_row(run: koru_delegate_core::RunId, session: SessionId) -> koru_runs_ui::RunRow {
    koru_runs_ui::RunRow {
        run,
        name: koru_delegate_core::RunName::of(run).to_string(),
        objective: "Write the quarterly summary".into(),
        status: "Working".into(),
        needs_attention: false,
        session: Some(session),
        created_at_ms: 10,
        motion: koru_runs_ui::Motion::Working,
        results: 0,
    }
}

/// **A session a run is driving opens read-only, and is watched rather than
/// attached to** (ADR-361).
///
/// The orchestrator holds that session's controller for the whole of the run.
/// A tab that attached would be asking for the lease it has — the conflict
/// §7.4 exists to prevent — so the tab reads the session's event stream over
/// a connection that takes none, and says so on its face.
#[test]
fn a_runs_session_opens_read_only_and_is_watched_never_attached() {
    let (mut v, workspace) = container();
    let commands = v.install_test_session_backend();
    let session = SessionId::mint();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x51));
    v.explore.set_agent_runs(vec![a_run_row(run, session)]);

    assert!(v.apply(&workspace.ws, Outcome::OpenSession(session)));
    let sent = drain(&commands);
    assert!(
        sent.iter()
            .any(|c| matches!(c, BackendCommand::Observe { session_id } if *session_id == session)),
        "the tab watches the session: {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|c| matches!(c, BackendCommand::Select { .. })),
        "and never takes the controller the orchestrator holds: {sent:?}"
    );

    let TabView::Session(view) = &v.tabs().active_tab().view else {
        panic!("a session tab opened")
    };
    assert_eq!(view.session, Some(session));
    assert!(view.watches_run, "it knows it is a run's session");
    assert_eq!(view.surface.watching(), koru_agents_ui::Watching::Yes);

    // A session no run in the listing claims opens the ordinary way.
    let other = SessionId::mint();
    assert!(v.apply(&workspace.ws, Outcome::OpenSession(other)));
    let sent = drain(&commands);
    assert!(
        sent.iter().any(
            |c| matches!(c, BackendCommand::Select { session_id, .. } if *session_id == other)
        ),
        "an unrelated session is still attached to: {sent:?}"
    );
}

/// **A watched session reads *running*, not *controlled elsewhere***.
///
/// The catalog reconciles every session tab's status against the host's
/// summary, and a session whose controller somebody else holds is otherwise
/// a conflict — the sentence about another window having taken it. On a tab
/// that exists to watch that very controller work, it is the expected state
/// and not news.
#[test]
fn a_watched_session_is_running_rather_than_a_controller_conflict() {
    use koru_session_protocol::{
        AttentionState, CapabilitySet, ControllerState, ProcessState, SessionMode, SessionSummary,
    };

    let (mut v, workspace) = container();
    let _commands = v.install_test_session_backend();
    let session = SessionId::mint();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x53));
    v.explore.set_agent_runs(vec![a_run_row(run, session)]);
    assert!(v.apply(&workspace.ws, Outcome::OpenSession(session)));

    let summary = SessionSummary {
        id: session,
        parent_session_id: None,
        profile: koru_session_protocol::SessionProfileKey::BuiltIn(
            "codex"
                .parse::<koru_session_protocol::BuiltInProfileKey>()
                .unwrap(),
        ),
        display_name: "Codex (Embedded)".into(),
        working_directory: None,
        mode: SessionMode::Managed {
            provider: "codex".parse().unwrap(),
        },
        process: ProcessState::Running,
        // The orchestrator has it, which is the whole point of watching.
        controller: ControllerState::Controlled,
        managed: None,
        active_turn_id: None,
        attention: AttentionState::default(),
        pending_attachment_count: 0,
        capabilities: CapabilitySet::default(),
        project_id: None,
        handover: Default::default(),
        model: None,
        origin: Default::default(),
        created_at_ms: 1,
    };
    v.deliver_test_session_event(BackendEvent::Sessions(vec![summary]));

    let TabView::Session(view) = &v.tabs().active_tab().view else {
        panic!("a session tab")
    };
    assert_eq!(
        view.surface.status,
        koru_agents_ui::PaneStatus::Running,
        "the controller the tab is watching is not a conflict with it"
    );
}

/// **The button asks, and claims nothing until the host says** (§7.4).
///
/// Pressing *Take the session* sends P1.16's own `request_control` — the same
/// verb the run views' chip sends, on the same connection — and the surface
/// stays read-only, saying it is waiting. Only the host's `Handover` turns the
/// tab into the ordinary editable session, and a handover back makes it a
/// watcher again rather than leaving a person on a session they let go of.
#[test]
fn taking_the_session_asks_and_only_the_host_grants_it() {
    let (mut v, workspace) = container();
    let commands = v.install_test_session_backend();
    let session = SessionId::mint();
    let run = koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x52));
    v.explore.set_agent_runs(vec![a_run_row(run, session)]);
    assert!(v.apply(&workspace.ws, Outcome::OpenSession(session)));
    let _ = drain(&commands);

    // The one control at the foot of the chat, pressed.
    {
        let TabView::Session(view) = &mut v.tabs_mut().active_mut().view else {
            panic!("a session tab")
        };
        assert!(view.surface.activate_watch_control());
    }
    assert!(v.maintain_sessions());
    match drain(&commands)
        .into_iter()
        .find(|c| matches!(c, BackendCommand::RequestControl { .. }))
    {
        Some(BackendCommand::RequestControl {
            session_id,
            urgency,
        }) => {
            assert_eq!(session_id, session);
            assert_eq!(
                urgency,
                koru_session_protocol::TakeoverUrgency::AtTurnBoundary,
                "the foot of a chat never ends the step the agent is on"
            );
        }
        other => panic!("expected the handover request, got {other:?}"),
    }
    let TabView::Session(view) = &v.tabs().active_tab().view else {
        panic!("a session tab")
    };
    assert_eq!(
        view.surface.watching(),
        koru_agents_ui::Watching::Asked,
        "it says it is waiting; it does not say the session is yours"
    );

    // The host grants it: the same tab becomes the ordinary editable session
    // and takes the controller on the next settle.
    assert!(v.deliver_test_session_event(BackendEvent::Handover {
        session_id: session,
        handover: koru_session_protocol::HandoverState::Taken,
    }));
    {
        let TabView::Session(view) = &v.tabs().active_tab().view else {
            panic!("a session tab")
        };
        assert_eq!(view.surface.watching(), koru_agents_ui::Watching::No);
        assert!(view.watches_run, "and still remembers whose session it is");
    }
    v.maintain_sessions();
    let sent = drain(&commands);
    assert!(
        sent.iter().any(
            |c| matches!(c, BackendCommand::Select { session_id, .. } if *session_id == session)
        ),
        "granted control is an ordinary attach: {sent:?}"
    );

    // And handed back, it goes back to watching rather than sitting on a
    // session it no longer holds.
    assert!(v.deliver_test_session_event(BackendEvent::Handover {
        session_id: session,
        handover: koru_session_protocol::HandoverState::Returned,
    }));
    {
        let TabView::Session(view) = &v.tabs().active_tab().view else {
            panic!("a session tab")
        };
        assert_eq!(view.surface.watching(), koru_agents_ui::Watching::Yes);
    }
    v.maintain_sessions();
    let sent = drain(&commands);
    assert!(
        sent.iter()
            .any(|c| matches!(c, BackendCommand::Observe { session_id } if *session_id == session)),
        "watching again: {sent:?}"
    );
}

// ------------------------------------------------------------- the rail order

/// The square `mode` sits on in the rail now.
fn rail_square(
    v: &MainView,
    mode: crate::panes::explore::NavigatorMode,
) -> koru_draw::geometry::Rect {
    v.explore
        .apps_rail_rects(v.rail_area())
        .apps
        .into_iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, r)| r)
        .expect("the view is on the rail")
}

/// **The rail's order is this device's, and it survives a restart.** Dragged
/// with the pointer through the window — press, carry half a square past its
/// neighbour, release — it is in the session file at the next capture and
/// comes back on the next launch.
#[test]
fn a_rail_order_dragged_with_the_pointer_survives_a_restart() {
    use crate::panes::explore::NavigatorMode::{Documents, Files};
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    let (x, y) = rail_square(&v, Files).centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    assert_eq!(
        v.explore.navigator_mode(),
        Files,
        "the press shows the view"
    );
    // Half a square and the gap above it: Files is half over Documents.
    let half =
        crate::panes::sidebar::apps_rail::SQUARE * 0.5 + crate::panes::sidebar::apps_rail::GAP;
    v.mouse_move(x, y - half);
    v.mouse_up(&mut workspace.ws);
    assert_eq!(&v.explore.rail_modes()[..2], &[Files, Documents]);

    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert!(
        settings.rail_order.contains("files,documents"),
        "{}",
        settings.rail_order
    );
    let back = reopened(&mut workspace, &settings);
    assert_eq!(back.explore.rail_modes(), v.explore.rail_modes());
}

/// **Esc mid-drag puts the view back**, and the session has nothing to save.
#[test]
fn escape_mid_drag_leaves_the_rail_as_it_was() {
    use crate::panes::explore::NavigatorMode::Files;
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    let before = v.explore.rail_modes();
    let (x, y) = rail_square(&v, Files).centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    v.mouse_move(x, y - 60.0);
    assert_ne!(v.explore.rail_modes(), before, "carried past Documents");
    assert!(v.key(
        &mut workspace.ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        winit::keyboard::ModifiersState::empty(),
    ));
    assert!(!v.explore.rail_dragging());
    assert_eq!(v.explore.rail_modes(), before);
    // The release that follows has nothing left to drop.
    v.mouse_up(&mut workspace.ws);
    assert_eq!(v.explore.rail_modes(), before);
    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert_eq!(settings.rail_order, "");
}
