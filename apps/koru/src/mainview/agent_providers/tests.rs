// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::tab::TabView;
use koru_session_protocol::{CapabilitySet, ErrorCode, ProviderKind};

fn profile(key: &str) -> LaunchProfile {
    koru_session_profiles::coding_agents()
        .into_iter()
        .map(|(_, profile)| profile)
        .find(|profile| profile.label.agent_key() == Some(key))
        .unwrap_or_else(|| panic!("{key} is a coding agent"))
}

fn readiness(kind: &str, availability: ExecutableAvailability) -> ProviderReadiness {
    ProviderReadiness {
        kind: kind.parse::<ProviderKind>().unwrap(),
        availability,
        capabilities: CapabilitySet::default(),
        failure: None,
    }
}

/// **Each status from what Koru already knows**: the presence probe for the
/// agent's own program, then — for an agent with a managed route — the
/// session host's readiness row, in the page's words.
#[test]
fn an_agents_status_comes_from_the_probe_and_the_readiness_row() {
    let ready = |kind| {
        readiness(
            kind,
            ExecutableAvailability::Available {
                version: Some("1.0.0".into()),
            },
        )
    };
    let snapshot = vec![
        ready("claude"),
        readiness(
            "codex",
            ExecutableAvailability::Incompatible {
                code: ErrorCode::ProviderIncompatible,
            },
        ),
        readiness(
            "kimi",
            ExecutableAvailability::SignedOut {
                code: ErrorCode::ProviderFailed,
            },
        ),
        readiness("antigravity", ExecutableAvailability::Missing),
        readiness(
            "grok",
            ExecutableAvailability::Failed {
                code: ErrorCode::ProviderFailed,
            },
        ),
        readiness("opencode", ExecutableAvailability::Checking),
    ];
    let status = |key: &str, present| agent_status(&profile(key), present, Some(&snapshot));
    assert_eq!(status("claude", None), AgentProviderStatus::Checking);
    assert_eq!(
        status("grok", Some(false)),
        AgentProviderStatus::NotInstalled {
            program: "grok".into()
        }
    );
    assert_eq!(status("claude", Some(true)), AgentProviderStatus::Ready);
    assert_eq!(
        status("codex", Some(true)),
        AgentProviderStatus::NeedsAttention(AgentAttention::UnsupportedVersion)
    );
    assert_eq!(
        status("kimi", Some(true)),
        AgentProviderStatus::NeedsAttention(AgentAttention::SignedOut)
    );
    assert_eq!(
        status("antigravity", Some(true)),
        AgentProviderStatus::NeedsAttention(AgentAttention::AdapterMissing {
            program: "agy_acp_server".into()
        })
    );
    assert_eq!(
        status("grok", Some(true)),
        AgentProviderStatus::NeedsAttention(AgentAttention::CheckFailed)
    );
    assert_eq!(
        status("opencode", Some(true)),
        AgentProviderStatus::Checking
    );
    // A terminal-only agent is ready when its program is there.
    assert_eq!(status("muse", Some(true)), AgentProviderStatus::Ready);
    // A managed agent before the host's first snapshot is still checking.
    assert_eq!(
        agent_status(&profile("claude"), Some(true), None),
        AgentProviderStatus::Checking
    );
}

/// **The page lists every coding agent Koru supports** — the *Agents ›*
/// menu's nine, in its order — each with a setup guide and on by default.
#[test]
fn the_page_lists_every_coding_agent_with_a_guide() {
    let mut view = MainView::new();
    assert!(view.open_settings_tab());
    let rows = settings_rows(&view);
    assert_eq!(
        rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
        [
            "Claude",
            "OpenCode",
            "Grok",
            "Kimi",
            "Codex",
            "GitHub Copilot",
            "Qwen Code",
            "Antigravity",
            "Muse"
        ]
    );
    for row in &rows {
        assert!(row.on, "{} is on by default", row.name);
        let guide = row
            .guide
            .unwrap_or_else(|| panic!("{} has a guide", row.name));
        assert!(guide.starts_with("https://"), "{guide}");
    }
}

fn settings_rows(view: &MainView) -> Vec<AgentProviderRow> {
    view.tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::Settings(settings) => Some(settings.agent_providers().to_vec()),
            _ => None,
        })
        .expect("a Settings tab")
}

/// The *Agents ›* flyout's agent rows, by agent key.
fn agents_menu() -> Vec<&'static str> {
    let profiles = koru_session_profiles::built_in_profiles();
    context_menu::agent_profile_items()
        .into_iter()
        .filter_map(|action| match action {
            context_menu::MenuAction::TerminalProfile(index) => profiles[index].label.agent_key(),
            _ => None,
        })
        .collect()
}

/// **Turning an agent off hides it from the chat's rail, the *Agents ›*
/// menu and the delegate list, and keeps a chat on it working**; Restore
/// defaults brings every one back. The switch is stored on this machine.
#[test]
fn turning_an_agent_off_hides_it_everywhere_and_restore_brings_it_back() {
    let mut view = MainView::new();
    assert!(view.open_settings_tab());
    assert!(agents_menu().contains(&"grok"));

    view.perform_settings_request(koru_settings_ui::Request::SetAgentEnabled {
        key: "grok".into(),
        on: false,
    });
    assert_eq!(view.disabled_agents(), ["grok"], "stored on this machine");
    assert!(!agents_menu().contains(&"grok"), "gone from Agents ›");
    assert!(agents_menu().contains(&"claude"));
    assert!(
        !settings_rows(&view)
            .iter()
            .find(|row| row.key == "grok")
            .unwrap()
            .on
    );
    // The chat pane's surface and a chat tab opened afterwards both hide it.
    assert!(!view.ai_chat.set_hidden_agents(["grok".to_owned()]));
    let mut later = crate::ai_chat_view::AiChatView::new();
    assert!(!later.set_hidden_agents(vec!["grok".to_owned()]));

    view.perform_settings_request(koru_settings_ui::Request::SetAgentEnabled {
        key: "codex".into(),
        on: false,
    });
    assert_eq!(view.disabled_agents(), ["codex", "grok"]);
    // Both Codex rows are one agent: neither terminal row is offered.
    assert!(!agents_menu().contains(&"codex"));

    view.perform_settings_request(koru_settings_ui::Request::RestoreAgentDefaults);
    assert!(view.disabled_agents().is_empty());
    assert!(agents_menu().contains(&"grok") && agents_menu().contains(&"codex"));
    assert!(settings_rows(&view).iter().all(|row| row.on));
    assert!(
        !view.ai_chat.set_hidden_agents(Vec::new()),
        "the pane shows all"
    );
}

fn settings_defaults(view: &MainView) -> AgentDefaults {
    view.tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::Settings(settings) => Some(settings.agent_defaults().clone()),
            _ => None,
        })
        .expect("a Settings tab")
}

/// **The default permission is stored on this machine and reaches every
/// chat surface** (D39): the side pane's and a chat tab opened afterwards
/// both start new chats from it, and Restore defaults puts both back on
/// Manual.
#[test]
fn the_default_permission_is_stored_and_reaches_every_chat_surface() {
    let mut view = MainView::new();
    assert!(view.open_settings_tab());
    assert_eq!(settings_defaults(&view), AgentDefaults::default());

    view.perform_settings_request(koru_settings_ui::Request::SetAgentPermission {
        other: false,
        permission: AgentPermission::Yolo,
    });
    view.perform_settings_request(koru_settings_ui::Request::SetAgentPermission {
        other: true,
        permission: AgentPermission::Yolo,
    });
    // An agent with no auto mode is never set to Auto.
    view.perform_settings_request(koru_settings_ui::Request::SetAgentPermission {
        other: true,
        permission: AgentPermission::Auto,
    });
    let section = view.delegate_folder.authoritative();
    assert_eq!(section.default_permission, AgentPermission::Yolo);
    assert_eq!(section.default_permission_other, AgentPermission::Yolo);
    assert_eq!(settings_defaults(&view).permission, AgentPermission::Yolo);
    let yolo = koru_ai_ui::AgentDefaults {
        permission: koru_ai_ui::NewChatPermission::Yolo,
        permission_other: koru_ai_ui::NewChatPermission::Yolo,
    };
    // A tab opened later reads the same defaults from a process-wide copy
    // other tests write in parallel, so only the pane is asserted here.
    assert!(!view.ai_chat.set_agent_defaults(yolo), "the pane has it");

    view.perform_settings_request(koru_settings_ui::Request::RestoreAgentDefaults);
    let section = view.delegate_folder.authoritative();
    assert_eq!(section.default_permission, AgentPermission::Manual);
    assert_eq!(section.default_permission_other, AgentPermission::Manual);
    assert_eq!(settings_defaults(&view), AgentDefaults::default());
    assert!(
        !view
            .ai_chat
            .set_agent_defaults(koru_ai_ui::AgentDefaults::default())
    );
}

/// **Agents › ends in Manage…, after a rule**, wherever the flyout opens.
#[test]
fn the_agents_menu_ends_in_manage_after_a_rule() {
    let items = context_menu::agent_plus_items();
    assert_eq!(items.last(), Some(&context_menu::MenuAction::ManageAgents));
    assert_eq!(
        items.get(items.len() - 2),
        Some(&context_menu::MenuAction::Divider)
    );
    assert_eq!(
        koru_ui::MenuItem::label(&context_menu::MenuAction::ManageAgents),
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::MenuManageAgents)
    );
}

fn showing_agent_providers(view: &MainView) -> bool {
    matches!(
        &view.tabs().active_tab().view,
        TabView::Settings(settings) if settings.page() == koru_settings_ui::PageId::AgentProviders
    )
}

/// **Both doors open the page**: *Agents › Manage…* from the `+` menu and
/// from the Agents tree, and the **+** at the end of a chat picker's rail.
#[test]
fn manage_and_the_pickers_plus_open_agent_providers() {
    let mut view = MainView::new();
    assert!(view.run_menu_action(
        context_menu::MenuSubject::NewTab,
        context_menu::MenuAction::ManageAgents
    ));
    assert!(showing_agent_providers(&view));

    let mut view = MainView::new();
    assert!(view.run_menu_action(
        context_menu::MenuSubject::AgentTree { project: None },
        context_menu::MenuAction::ManageAgents
    ));
    assert!(showing_agent_providers(&view));

    let mut view = MainView::new();
    assert!(!view.open_agent_settings_from_chat(), "nothing asked");
    view.ai_chat.ask_for_agent_settings();
    assert!(view.open_agent_settings_from_chat());
    assert!(showing_agent_providers(&view));
}
