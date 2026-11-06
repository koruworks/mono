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
        AgentProviderStatus::NeedsAttention(AgentAttention::UnsupportedVersion {
            update: None,
            desktop_app: None,
        })
    );
    assert_eq!(
        status("kimi", Some(true)),
        AgentProviderStatus::NeedsAttention(AgentAttention::SignedOut { sign_in: None })
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

/// **Devin's rows name the command to run**: an unsupported version names
/// `devin update` and says the desktop app's copy updates with the app,
/// signed out names `devin auth login`, from its registration; an agent
/// with no such command keeps today's words.
#[test]
fn devins_attention_rows_name_the_command_to_run() {
    let incompatible = ExecutableAvailability::Incompatible {
        code: ErrorCode::ProviderIncompatible,
    };
    let signed_out = ExecutableAvailability::SignedOut {
        code: ErrorCode::ProviderFailed,
    };
    let lang = koru_i18n::Lang::En;
    let words = |key: &str, availability: &ExecutableAvailability| {
        let snapshot = vec![readiness(key, availability.clone())];
        agent_status(&profile(key), Some(true), Some(&snapshot)).words(lang)
    };
    assert_eq!(
        agent_status(
            &profile("devin"),
            Some(true),
            Some(&[readiness("devin", incompatible.clone())])
        ),
        AgentProviderStatus::NeedsAttention(AgentAttention::UnsupportedVersion {
            update: Some("devin update".into()),
            desktop_app: Some("Devin".into()),
        })
    );
    assert!(words("devin", &incompatible).contains("Run `devin update` to update it."));
    assert!(
        words("devin", &incompatible)
            .ends_with("If you use the Devin desktop app, its copy updates with the app.")
    );
    assert!(words("devin", &signed_out).contains("Run `devin auth login` to sign in."));
    // Codex knows no such command here: the row reads as it always did.
    assert!(words("codex", &incompatible).ends_with("This version isn't one Koru supports yet."));
    assert!(words("kimi", &signed_out).ends_with("Sign in from the agent's own CLI."));
}

/// **The page lists every coding agent Koru supports** — the *Agents ›*
/// menu's ten, in its order — each with a setup guide and on by default.
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
            "Muse",
            "Devin"
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
    // A canvas's agent menu leaves it out too (D42).
    assert!(
        !view
            .canvas_runs
            .set_agent_settings(vec!["grok".to_owned()], view.agent_order()),
        "the canvas runs have it"
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
        ..koru_ai_ui::AgentDefaults::default()
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

/// **The default agent is stored on this machine and reaches the chat
/// surfaces** (D40); turned off, the page says new chats fall back to the
/// last agent used, and Restore defaults goes back to Auto.
#[test]
fn the_default_agent_is_stored_and_falls_back_when_turned_off() {
    let mut view = MainView::new();
    assert!(view.open_settings_tab());
    view.perform_settings_request(koru_settings_ui::Request::SetDefaultAgent(Some(
        "codex".into(),
    )));
    assert_eq!(
        view.delegate_folder
            .authoritative()
            .default_agent
            .as_deref(),
        Some("codex")
    );
    assert_eq!(settings_defaults(&view).agent.as_deref(), Some("codex"));
    assert_eq!(settings_defaults(&view).agent_note, None);
    assert!(
        !view.ai_chat.set_agent_defaults(view.chat_agent_defaults()),
        "the pane has it"
    );
    assert_eq!(view.chat_agent_defaults().agent.as_deref(), Some("codex"));

    view.perform_settings_request(koru_settings_ui::Request::SetAgentEnabled {
        key: "codex".into(),
        on: false,
    });
    let note = settings_defaults(&view).agent_note.expect("a note");
    assert!(note.starts_with("Codex"), "{note}");
    assert_eq!(
        note,
        koru_i18n::fill(
            koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::SettingsDefaultAgentOff
            ),
            &["Codex"]
        )
    );

    view.perform_settings_request(koru_settings_ui::Request::RestoreAgentDefaults);
    assert_eq!(view.delegate_folder.authoritative().default_agent, None);
    assert_eq!(settings_defaults(&view), AgentDefaults::default());
}

/// A default agent that is not installed is said too; Auto, and an agent
/// that can be had, say nothing.
#[test]
fn the_default_agent_note_names_an_agent_that_is_not_installed() {
    let row = |key: &str, name: &str, status, on| AgentProviderRow {
        key: key.into(),
        name: name.into(),
        status,
        guide: None,
        on,
        install: false,
    };
    let rows = [
        row("claude", "Claude", AgentProviderStatus::Ready, true),
        row(
            "grok",
            "Grok",
            AgentProviderStatus::NotInstalled {
                program: "grok".into(),
            },
            true,
        ),
    ];
    let lang = crate::format::lang();
    assert_eq!(
        default_agent_note(Some("grok"), &rows),
        Some(koru_i18n::fill(
            koru_i18n::t(lang, koru_i18n::Key::SettingsDefaultAgentMissing),
            &["Grok"]
        ))
    );
    assert_eq!(default_agent_note(Some("claude"), &rows), None);
    assert_eq!(default_agent_note(None, &rows), None);
}

/// **The order is Claude first, then the menu's, until it is dragged**, and
/// an order names every agent once: a stored one's keys first, the rest after
/// in the built-in order, a key no agent has left out.
#[test]
fn the_agent_order_fills_in_the_built_in_order() {
    let built_in = agent_order(&[]);
    assert_eq!(built_in[0], "claude");
    assert_eq!(built_in.len(), koru_session_profiles::coding_agents().len());
    let order = agent_order(&[
        "muse".into(),
        "nobody".into(),
        "codex".into(),
        "muse".into(),
    ]);
    assert_eq!(&order[..2], ["muse", "codex"]);
    assert_eq!(order[2], "claude");
    assert_eq!(order.len(), built_in.len());
    assert_eq!(order_rank(&order, Some("codex")), 1);
    assert_eq!(order_rank(&order, None), usize::MAX, "a shell sorts last");
}

/// **The order applies everywhere agents are listed** (D41): the page's
/// rows, the *Agents ›* menu, the chat pane and later chat tabs, stored on
/// this machine — and nothing stored for the built-in order. Restore
/// defaults puts the built-in order back.
#[test]
fn the_agent_order_reaches_the_menu_the_chats_and_the_page() {
    let mut view = MainView::new();
    assert!(view.open_settings_tab());
    let mut order = agent_order(&[]);
    order.swap(0, 4);
    let moved = order.clone();
    view.perform_settings_request(koru_settings_ui::Request::SetAgentOrder(order));
    assert_eq!(view.delegate_folder.authoritative().agent_order, moved);
    assert_eq!(
        settings_rows(&view)
            .iter()
            .map(|row| row.key.clone())
            .collect::<Vec<_>>(),
        moved,
        "the page, and so the Default agent chips"
    );
    let menu: Vec<String> =
        agents_menu()
            .into_iter()
            .map(str::to_owned)
            .fold(Vec::new(), |mut keys, key| {
                if !keys.contains(&key) {
                    keys.push(key);
                }
                keys
            });
    assert_eq!(menu, moved, "Agents › follows it");
    assert!(
        !view.ai_chat.set_agent_order(moved.clone()),
        "the chat pane's rail has it"
    );
    // A tab opened later reads a process-wide copy other tests write in
    // parallel; the pane is the one asserted.

    // Dragged back to the built-in order: nothing is stored.
    view.perform_settings_request(koru_settings_ui::Request::SetAgentOrder(agent_order(&[])));
    assert!(view.delegate_folder.authoritative().agent_order.is_empty());

    view.perform_settings_request(koru_settings_ui::Request::SetAgentOrder(moved));
    view.perform_settings_request(koru_settings_ui::Request::RestoreAgentDefaults);
    assert!(view.delegate_folder.authoritative().agent_order.is_empty());
    assert_eq!(settings_rows(&view)[0].key, "claude");
    assert_eq!(agents_menu()[0], "claude");
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

/// **The picker's favorites are stored roaming and reach every chat**
/// (D46): a star pressed in a chat is one Global patch through the
/// window's link — the roaming half, `preferences.conf` — and the stored
/// list is handed to the chat pane, so a chat on this window and the next
/// show the same stars. *Restore defaults* on Agent providers leaves them,
/// as they are not that page's.
#[test]
fn the_favorite_models_are_stored_roaming_and_reach_the_chats() {
    let mut view = MainView::new();
    assert!(
        !view.save_favorite_models_from_chat(),
        "no star pressed: nothing stored"
    );
    let favorites = vec![
        koru_settings::FavoriteModel::new("claude", "opus"),
        koru_settings::FavoriteModel::new("codex", "gpt-5"),
    ];
    let patch = koru_settings::GlobalPatch {
        favorite_models: Some(favorites.clone()),
        ..koru_settings::GlobalPatch::default()
    };
    assert!(patch.touches_roaming() && !patch.touches_device());
    view.save_agent_settings(patch);
    assert_eq!(
        view.delegate_folder.authoritative().favorite_models,
        favorites
    );
    let shown = chat_favorites(&favorites);
    assert_eq!(view.favorite_models(), shown);
    assert!(
        !view.ai_chat.set_favorite_models(shown.clone()),
        "the chat pane already has them"
    );

    view.perform_settings_request(koru_settings_ui::Request::RestoreAgentDefaults);
    assert_eq!(
        view.delegate_folder.authoritative().favorite_models,
        favorites,
        "not Agent providers' to restore"
    );
}
