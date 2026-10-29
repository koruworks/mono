// SPDX-License-Identifier: MPL-2.0

//! **The simplified product, end to end in the container.**
//!
//! Every other suite in this crate runs with every surface on — the
//! `koru-features/testing` default, so the tests written before the flags
//! keep exercising tasks, agents, Koru AI, reminders and Koru Cloud. These
//! tests opt into the build a release ships with nothing baked
//! (`Features::ALL_OFF`) on their own thread, and prove what a person can
//! reach: documents, files and shell sessions, and nothing behind a flag.
//! See guide/contributing/FEATURE-FLAGS.md.

use super::*;
use crate::mainview::harness::*;
use crate::panes::explore::NavigatorMode;
use crate::panes::explore::rail::{Bucket, RailTarget};
use koru_features::{Features, testing};
use koru_session_profiles::built_in_profiles;
use koru_session_protocol::{ProfileKind, SessionId};
use koru_workspace_host::{Settings, TabRef, TasksTabRef};

fn off<R>(body: impl FnOnce() -> R) -> R {
    testing::with(Features::ALL_OFF, body)
}

/// The `+` menu is Welcome, Documents and Shell — no Canvases, no Agents, no
/// AI chat, no Delegate, no Chats, no Socials — and a hidden row is refused
/// even when it is asked for by name rather than picked from the menu.
#[test]
fn the_add_menu_offers_documents_shells_and_welcome_only() {
    off(|| {
        let (mut v, _workspace) = on_document();
        assert!(v.open_new_tab_menu());
        let items = v.menu.as_ref().expect("the menu opened").items().to_vec();
        assert_eq!(
            items,
            [
                MenuAction::OpenWelcome,
                MenuAction::Documents,
                MenuAction::NewShell,
                MenuAction::Divider,
                MenuAction::ImportAs,
            ]
        );
        v.menu = None;

        let shells = context_menu::shown(context_menu::shell_plus_items());
        assert!(!shells.is_empty(), "the shells are all there");
        let profiles = built_in_profiles();
        for row in &shells {
            if let MenuAction::TerminalProfile(index) = row {
                assert_ne!(profiles[*index].kind, ProfileKind::Agent);
            }
        }
        assert!(context_menu::shown(context_menu::agent_profile_items()).is_empty());

        let before = v.tabs().len();
        for hidden in [
            MenuAction::NewAiChat,
            MenuAction::Delegate,
            MenuAction::NewAgent,
            MenuAction::NewCanvas,
            MenuAction::Socials,
        ] {
            assert!(
                !v.run_menu_action(MenuSubject::NewTab, hidden),
                "{hidden:?} ran"
            );
        }
        if let Some(agent) = profiles
            .iter()
            .position(|profile| profile.kind == ProfileKind::Agent)
        {
            assert!(!v.run_menu_action(MenuSubject::NewTab, MenuAction::TerminalProfile(agent)));
            assert!(!v.open_session_tab_for_profile(agent, None));
        }
        assert_eq!(v.tabs().len(), before, "nothing opened");
    });
}

/// Every door a hidden tab or pane opens by says no: the Tasks and Reminders
/// tabs, an AI chat tab, the AI pane, a Run tab, the Tasks view a due chip
/// would reveal, and the delegation modal.
#[test]
fn hidden_tabs_and_panes_do_not_open() {
    off(|| {
        let (mut v, mut workspace) = on_document();
        let before = v.tabs().len();
        assert!(!v.open_tasks_tab(RailTarget::Bucket(Bucket::Today), false));
        assert!(!v.open_tasks_tab(RailTarget::Reminders, true));
        assert!(!v.open_ai_chat_tab());
        assert!(!v.toggle_ai());
        assert!(!v.ai_panel.is_open());
        assert!(!v.open_run_tab(koru_delegate_core::RunId::mint()));
        assert!(!v.open_all_reminders());
        let task = a_document(&mut workspace.ws, "a task");
        assert!(!v.reveal_task_in_tasks_view(&workspace.ws, task));
        assert_eq!(v.tabs().len(), before, "no tab opened");
        assert_eq!(v.explore.navigator_mode(), NavigatorMode::Documents);
    });
}

/// The context column offers History alone: no Tasks, no Reminders, no
/// Cloud sync — and so no tab row, because one tab is a button that does
/// nothing.
#[test]
fn the_context_column_offers_history_alone() {
    off(|| {
        use crate::panes::context::{ContextView, Section};
        assert_eq!(Section::visible(), [Section::History]);
        assert!(
            ContextView::section_rects(koru_draw::geometry::Rect::new(0.0, 0.0, 320.0, 600.0))
                .is_empty()
        );
        let mut view = ContextView::default();
        assert!(!view.select(Section::Tasks));
        assert!(!view.select(Section::CloudSync));
        assert_eq!(view.section(), Section::History);
    });
}

/// **A session written by a build that showed everything** comes back as
/// this build's product: the Tasks, Reminders and agent-session tabs are
/// dropped, the shell session and Welcome stay, the explore column reopens
/// on Documents rather than Tasks, and the AI pane stays shut.
#[test]
fn a_restored_session_drops_what_this_build_hides() {
    off(|| {
        let (_, workspace) = on_document();
        let profiles = built_in_profiles();
        let shell = profiles
            .iter()
            .find(|profile| profile.kind == ProfileKind::Shell)
            .expect("a shell profile");
        let mut settings = Settings::default();
        settings.tabs = vec![
            (TabRef::Welcome, 0.0),
            (TabRef::Tasks(TasksTabRef::Bucket("later")), 0.0),
            (TabRef::Tasks(TasksTabRef::Reminders), 0.0),
            (
                TabRef::Session {
                    session: SessionId::mint(),
                    profile: shell.id.clone(),
                    recovery: None,
                },
                0.0,
            ),
        ];
        if let Some(agent) = profiles
            .iter()
            .find(|profile| profile.kind == ProfileKind::Agent)
        {
            settings.tabs.push((
                TabRef::Session {
                    session: SessionId::mint(),
                    profile: agent.id.clone(),
                    recovery: None,
                },
                0.0,
            ));
        }
        settings.navigator_mode = "tasks".into();
        settings.ai_open = true;
        let mut v = MainView::new();
        View::restore(&mut v, &workspace, &settings);
        let kinds: Vec<&'static str> = v
            .tabs()
            .iter()
            .map(|tab| match &tab.view {
                TabView::Welcome(_) => "welcome",
                TabView::Session(_) => "session",
                TabView::Tasks(_) => "tasks",
                TabView::Reminders(_) => "reminders",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["welcome", "session"]);
        assert_eq!(v.explore.navigator_mode(), NavigatorMode::Documents);
        assert!(!v.ai_panel.is_open());
    });
}

/// The same session with every surface on brings every tab back — the
/// control for the test above, so a restore that dropped everything would
/// not pass for one that drops the right things.
#[test]
fn with_every_surface_on_the_same_session_comes_back_whole() {
    let (_, workspace) = on_document();
    let mut settings = Settings::default();
    settings.tabs = vec![
        (TabRef::Welcome, 0.0),
        (TabRef::Tasks(TasksTabRef::Bucket("later")), 0.0),
    ];
    let mut v = MainView::new();
    View::restore(&mut v, &workspace, &settings);
    assert!(
        v.tabs()
            .iter()
            .any(|tab| matches!(tab.view, TabView::Tasks(_)))
    );
}

/// The Manage tab with Koru Cloud hidden: no *Koru Cloud* row in its nav,
/// and asking for the Cloud page — which is what *Manage profiles…* does
/// before the window knows its Profile — lands somewhere else.
#[test]
fn the_cloud_page_is_unreachable() {
    off(|| {
        let (mut v, _workspace) = on_document();
        assert!(v.open_manage(crate::manage::Page::Cloud));
        let TabView::Manage(view) = &v.tabs().active_tab().view else {
            panic!("the Manage tab is in front");
        };
        assert_ne!(view.page(), Some(crate::manage::Page::Cloud));
    });
}

/// **Socials with `ENABLE_SOCIALS` off**: the one door a Socials tab opens by
/// says no — the `+` row, Explore's mode and its chord, *New social post*
/// from a document — no tab or navigator appears, and the tab and node menus
/// have no *New social post* row. The publisher in the Agent is not a door
/// and is not asked (ADR-450).
#[test]
fn socials_does_not_open() {
    off(|| {
        let (mut v, mut workspace) = on_document();
        let before = v.tabs().len();
        assert!(!v.open_socials_tab());
        assert!(!v.run_menu_action(MenuSubject::NewTab, MenuAction::Socials));
        if let Some(document) = v.active_document {
            assert!(!v.social_post_from_document(document));
        }
        v.key(
            &mut workspace.ws,
            &winit::keyboard::Key::Character("8".into()),
            Some("8"),
            koru_keys::VIEW,
        );
        assert_eq!(v.explore.navigator_mode(), NavigatorMode::Documents);
        assert!(
            !v.tabs()
                .iter()
                .any(|tab| matches!(tab.view, TabView::Socials(_)))
        );
        assert_eq!(v.tabs().len(), before, "no tab opened");
        assert!(
            !context_menu::shown(context_menu::node_items(1)).contains(&MenuAction::NewSocialPost)
        );
        assert!(!MenuAction::NewSocialPost.shown_in_product());
        assert!(!MenuAction::Socials.shown_in_product());
        assert!(!MenuAction::Canvases.shown_in_product());
        assert!(!MenuAction::NewCanvas.shown_in_product());
    });
}

/// The control: with every surface on, Socials and Canvases are offered.
#[test]
fn with_every_surface_on_socials_and_canvases_are_offered() {
    let (mut v, _workspace) = on_document();
    assert!(v.open_new_tab_menu());
    let items = v.menu.as_ref().expect("the menu opened").items().to_vec();
    assert!(items.contains(&MenuAction::Canvases));
    assert!(items.contains(&MenuAction::Socials));
    v.menu = None;
    assert!(v.open_socials_tab());
    assert!(matches!(v.tabs().active_tab().view, TabView::Socials(_)));
}
