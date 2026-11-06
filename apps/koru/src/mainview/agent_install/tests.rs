// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::agent_providers::installer;
use koru_settings_ui::{AgentAttention, AgentProviderStatus};

fn missing(program: &str) -> AgentProviderStatus {
    AgentProviderStatus::NotInstalled {
        program: program.into(),
    }
}

fn adapter_missing() -> AgentProviderStatus {
    AgentProviderStatus::NeedsAttention(AgentAttention::AdapterMissing {
        program: "claude-agent-acp".into(),
    })
}

fn words() -> InstallWords {
    InstallWords {
        needs_node: "Installing Claude needs Node.js.".into(),
        adapter_needs_node: "Chats with Claude need Node.js.".into(),
        // An apostrophe, both kinds: the quoting has to survive the
        // languages that use them.
        close: "Press Enter, c'est l’heure.".into(),
    }
}

const ADAPTER: &str = "npm install -g @agentclientprotocol/claude-agent-acp@0.76.0";

/// **Every coding agent is either installable or deliberately not** — a new
/// agent fails here until somebody decides which, rather than silently
/// offering no *Install* or a guessed one.
#[test]
fn every_coding_agent_has_an_installer_or_is_left_out_on_purpose() {
    const LEFT_OUT: &[&str] = &["grok", "antigravity", "muse", "devin"];
    for (_, profile) in koru_session_profiles::coding_agents() {
        let key = profile.label.agent_key().expect("an agent key");
        assert_eq!(
            installer(key).is_some(),
            !LEFT_OUT.contains(&key),
            "{key}: an installer exactly when it is not left out"
        );
    }
}

/// **The npm agents install the package the update check reads**, and Claude
/// its native installer, then the adapter at the release its chat route pins.
#[test]
fn the_table_names_each_vendors_own_command() {
    let npm = |package| InstallStep::Npm {
        package,
        version: None,
    };
    for (key, package) in [
        ("codex", "@openai/codex"),
        ("copilot", "@github/copilot"),
        ("kimi", "@moonshot-ai/kimi-code"),
        ("opencode", "opencode-ai"),
        ("qwen", "@qwen-code/qwen-code"),
    ] {
        assert_eq!(
            installer(key),
            Some(Installer {
                cli: npm(package),
                adapter: None
            }),
            "{key}"
        );
    }
    let claude = installer("claude").expect("Claude installs");
    assert_eq!(
        command(claude.cli, Shell::Posix { login: false }),
        "curl -fsSL https://claude.ai/install.sh | bash"
    );
    assert_eq!(
        command(claude.cli, Shell::PowerShell),
        "irm https://claude.ai/install.ps1 | iex"
    );
    let adapter = claude.adapter.expect("and its chat adapter");
    assert_eq!(command(adapter, Shell::PowerShell), ADAPTER);
    assert_eq!(command(adapter, Shell::Posix { login: true }), ADAPTER);
}

/// **What *Install* runs follows the status**: the CLI and its adapter when
/// the CLI is missing, the adapter alone when only it is, and nothing when
/// nothing is missing or Koru knows no command for what is.
#[test]
fn the_plan_installs_only_what_is_missing() {
    let claude = installer("claude").unwrap();
    assert_eq!(install_plan("claude", &missing("claude")), Some(claude));
    assert_eq!(
        install_plan("claude", &adapter_missing()),
        Some(Installer {
            cli: claude.adapter.unwrap(),
            adapter: None
        }),
        "the CLI is here: only the adapter"
    );
    assert!(install_plan("claude", &AgentProviderStatus::Ready).is_none());
    assert!(install_plan("claude", &AgentProviderStatus::Checking).is_none());
    assert!(install_plan("codex", &missing("codex")).is_some());
    assert!(
        install_plan("codex", &adapter_missing()).is_none(),
        "Codex has no adapter to install"
    );
    for key in ["grok", "antigravity", "muse", "devin"] {
        assert!(install_plan(key, &missing(key)).is_none(), "{key}");
    }
    assert!(
        install_plan("antigravity", &adapter_missing()).is_none(),
        "Antigravity's ACP server has no command Koru knows"
    );
}

/// **The POSIX script**: each command shown, then run; npm only where it is
/// on `PATH`, else the sentence saying Node.js is needed; the wait for
/// Enter last. A login shell on macOS, the person's own when it reads POSIX.
#[test]
fn the_posix_script_shows_each_command_guards_npm_and_waits() {
    let plan = installer("claude").unwrap();
    let argv = install_argv(
        plan,
        Shell::Posix { login: true },
        Some("/bin/zsh"),
        &words(),
    );
    assert_eq!(&argv[..3], ["/bin/zsh", "-l", "-c"]);
    assert_eq!(argv.len(), 4);
    let script: Vec<&str> = argv[3].lines().collect();
    assert_eq!(
        script,
        [
            "printf '\\n$ %s\\n' 'curl -fsSL https://claude.ai/install.sh | bash'; \
             curl -fsSL https://claude.ai/install.sh | bash",
            &format!(
                "if command -v npm >/dev/null 2>&1; then printf '\\n$ %s\\n' '{ADAPTER}'; \
                 {ADAPTER}; else printf '\\n%s\\n' 'Chats with Claude need Node.js.'; fi"
            ),
            "printf '\\n%s ' 'Press Enter, c'\\''est l’heure.'",
            "read -r koru_reply",
        ]
    );

    // An npm-only agent says the other sentence, and Linux is no login.
    let argv = install_argv(
        installer("codex").unwrap(),
        Shell::Posix { login: false },
        Some("/usr/bin/bash"),
        &words(),
    );
    assert_eq!(&argv[..2], ["/usr/bin/bash", "-c"]);
    assert!(argv[2].contains("'Installing Claude needs Node.js.'"));
    assert!(argv[2].contains(
        "then printf '\\n$ %s\\n' 'npm install -g @openai/codex'; npm install -g @openai/codex;"
    ));
}

/// A shell that does not read POSIX scripts, or none named, runs `/bin/sh`.
#[test]
fn a_shell_that_is_not_posix_falls_back_to_sh() {
    assert_eq!(posix_shell(Some("/opt/homebrew/bin/fish")), "/bin/sh");
    assert_eq!(posix_shell(Some("/usr/bin/nu")), "/bin/sh");
    assert_eq!(posix_shell(None), "/bin/sh");
    assert_eq!(posix_shell(Some("/bin/bash")), "/bin/bash");
}

/// **The PowerShell command**: `irm … | iex` in a PowerShell of its own, so
/// an installer's `exit` cannot end the steps after it; npm guarded; the
/// wait last; and no double quote anywhere, so the Windows command line the
/// session host builds cannot split it.
#[test]
fn the_powershell_command_isolates_the_installer_and_waits() {
    let argv = install_argv(
        installer("claude").unwrap(),
        Shell::PowerShell,
        None,
        &words(),
    );
    assert_eq!(
        &argv[..6],
        [
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command"
        ]
    );
    assert_eq!(argv.len(), 7);
    let script = &argv[6];
    assert!(!script.contains('"'), "{script}");
    assert!(script.starts_with(
        "Write-Host ''; Write-Host '> irm https://claude.ai/install.ps1 | iex'; \
         powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -Command \
         'irm https://claude.ai/install.ps1 | iex'; "
    ));
    assert!(script.contains(&format!(
        "if (Get-Command npm -ErrorAction SilentlyContinue) {{ Write-Host ''; \
         Write-Host '> {ADAPTER}'; {ADAPTER} }} else {{ Write-Host ''; \
         Write-Host 'Chats with Claude need Node.js.' }}"
    )));
    assert!(
        script.ends_with("; $null = Read-Host 'Press Enter, c''est l’’heure.'"),
        "{script}"
    );
}

fn a_session(id: SessionId, process: ProcessState) -> SessionSummary {
    use koru_session_protocol::*;
    SessionSummary {
        id,
        parent_session_id: None,
        profile: SessionProfileKey::Custom(SessionProfileId::mint()),
        display_name: "Install Claude".to_owned(),
        working_directory: None,
        mode: SessionMode::RawPty,
        process,
        controller: ControllerState::Detached,
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
    }
}

/// A view on which Claude is not installed, as the probe says — injected,
/// never this machine's `PATH`.
fn view_without_claude() -> MainView {
    let view = MainView::new();
    view.profile_probe.set_for_test(
        koru_session_profiles::built_in_profiles()
            .iter()
            .map(|profile| profile.label.agent_key() != Some("claude"))
            .collect(),
    );
    view
}

/// **Install opens a terminal tab running the install command**, named for
/// the agent; a second press brings that tab forward instead of starting a
/// second installer; an agent with nothing to install opens nothing.
#[test]
fn install_opens_one_terminal_tab_running_the_command() {
    let mut view = view_without_claude();
    let before = view.tabs().len();
    assert!(view.install_agent("claude"));
    assert_eq!(view.tabs().len(), before + 1);
    let tab = view.tabs().active_tab();
    let lang = crate::format::lang();
    assert_eq!(
        tab.label,
        fill(t(lang, Key::SettingsAgentInstallTab), &["Claude"])
    );
    let TabView::Session(session) = &tab.view else {
        panic!("a session tab")
    };
    let launch = session.pending_launch.as_ref().expect("a launch");
    assert!(matches!(
        launch.profile.id,
        koru_session_protocol::SessionProfileKey::Custom(_)
    ));
    assert_eq!(
        launch.profile.argv,
        install_argv(
            installer("claude").unwrap(),
            Shell::this_machine(),
            std::env::var("SHELL").ok().as_deref(),
            &InstallWords::new(lang, "Claude"),
        )
    );

    view.select(0);
    assert!(view.install_agent("claude"));
    assert_eq!(view.tabs().len(), before + 1, "no second installer");
    assert_eq!(
        view.tabs().active_tab().label,
        fill(t(lang, Key::SettingsAgentInstallTab), &["Claude"]),
        "the install tab came forward"
    );

    assert!(
        !view.install_agent("antigravity"),
        "installed, and no command Koru knows"
    );
    assert_eq!(view.tabs().len(), before + 1);
}

/// **The install's end is the re-check**: while the catalog has its session
/// running it is followed; once the session is gone — whether or not its tab
/// still stands — it is over and the agents are checked again. A tab closed
/// before it learnt its session is over too.
#[test]
fn an_install_ends_when_its_session_does() {
    let mut view = view_without_claude();
    assert!(view.install_agent("claude"));
    let session = SessionId::mint();
    if let TabView::Session(install) = &mut view.tabs_mut().active_mut().view {
        install.session = Some(session);
    }
    assert!(!view.watch_agent_installs(&[a_session(session, ProcessState::Running)]));
    assert_eq!(view.agent_installs.len(), 1, "still running");

    // The person closes the tab early: the process outlives it, and is
    // still followed by the session it learnt.
    let index = view.tabs().active();
    assert!(view.close(index));
    assert!(!view.watch_agent_installs(&[a_session(session, ProcessState::Running)]));
    assert_eq!(view.agent_installs[0].session, Some(session));
    assert!(view.watch_agent_installs(&[]), "gone from the catalog");
    assert!(view.agent_installs.is_empty());

    // A tab that closed before it learnt its session.
    assert!(view.install_agent("claude"));
    let index = view.tabs().active();
    assert!(view.close(index));
    assert!(view.watch_agent_installs(&[]));
    assert!(view.agent_installs.is_empty());
}
