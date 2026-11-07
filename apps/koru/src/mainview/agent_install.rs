// SPDX-License-Identifier: MPL-2.0

//! **Settings → Agent providers → *Install***: the vendor's install command,
//! run in a Koru terminal tab.
//!
//! The command comes from the one table in `agent_providers`
//! ([`installer`](super::agent_providers::installer)); this module turns it
//! into a session the person can watch. **A terminal tab rather than a
//! background job**, because the installers are interactive — the npm ones
//! can ask for a password or a confirmation, Claude's prints where it put
//! the program — and a person who pressed *Install* should see what ran
//! and what it said. The tab is an ordinary session tab
//! ([`open_launch_tab`](MainView::open_launch_tab)) over a custom profile
//! whose argv is the command, the path Koru Terminal's own custom profiles
//! take.
//!
//! **The script waits for Enter before it ends.** A session that ends closes
//! its tab (Q-E6 P2), which would take the installer's output with it; so the
//! last line asks the person to press Enter, and the tab closes when they
//! have read it.
//!
//! **Its end is the re-check.** The session catalog says when the install's
//! session is gone ([`watch_agent_installs`](MainView::watch_agent_installs),
//! on the catalog pass that would close its tab), and the agents' status is
//! read again then — the presence probe and every managed route's readiness,
//! *Refresh status*'s two checks — so the row reads *Ready* without a restart.
//! That covers the person closing the tab early too: the process outlives
//! its tab, and its end is still in the catalog.

use koru_i18n::{Key, fill, t};
use koru_session_protocol::{ProcessState, SessionId, SessionProfileId, SessionSummary};

use super::MainView;
use super::agent_providers::{InstallStep, Installer, install_plan};
use super::tab::TabView;

/// One *Install* tab that has not ended yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentInstall {
    /// The agent's key — a second press brings this tab forward instead of
    /// starting a second installer.
    key: String,
    /// The tab, while it is open.
    tab: u64,
    /// The session, once the tab has learnt it — kept so a tab the person
    /// closed early can still be followed to its end.
    session: Option<SessionId>,
}

/// The shells a script is run in: a POSIX one on macOS and Linux,
/// PowerShell on Windows, where `irm … | iex` is the vendor's command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shell {
    /// `program -c script`, with `-l` first when `login`.
    Posix {
        login: bool,
    },
    PowerShell,
}

impl Shell {
    fn this_machine() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            // **Login on macOS, for the reason the platform shell is**
            // (`koru_session_profiles::platform_shell_argv`): a Finder-launched
            // app has launchd's four directories, and `npm` from Homebrew or
            // nodejs.org is only on the login shell's `PATH`.
            Self::Posix {
                login: cfg!(target_os = "macos"),
            }
        }
    }
}

/// The sentences the script prints, in the person's language.
pub(super) struct InstallWords {
    /// *Installing X needs Node.js and npm…*, for an npm-installed CLI.
    pub(super) needs_node: String,
    /// *Chats with X in Koru need Node.js and npm…*, for a chat adapter.
    pub(super) adapter_needs_node: String,
    /// *Press Enter to close this tab…*
    pub(super) close: String,
}

impl InstallWords {
    fn new(lang: koru_i18n::Lang, agent: &str) -> Self {
        Self {
            needs_node: fill(t(lang, Key::SettingsAgentInstallNeedsNode), &[agent]),
            adapter_needs_node: fill(t(lang, Key::SettingsAgentInstallAdapterNeedsNode), &[agent]),
            close: fill(t(lang, Key::SettingsAgentInstallClose), &[agent]),
        }
    }
}

/// **The POSIX shell the script runs in**: the person's own `$SHELL` when it
/// is one that reads a POSIX script — so its login files set `PATH` as their
/// terminal does — and `/bin/sh` otherwise (fish, nushell), which every Mac
/// and Linux machine has.
fn posix_shell(shell: Option<&str>) -> String {
    const POSIX: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];
    shell
        .filter(|path| {
            std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| POSIX.contains(&name))
        })
        .unwrap_or("/bin/sh")
        .to_owned()
}

/// A POSIX single-quoted word: every byte literal, a `'` closed, escaped
/// and reopened.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A PowerShell single-quoted string. PowerShell ends one at any of the four
/// single-quote marks, typographic ones included — a French apostrophe would
/// otherwise end the sentence — and a doubled mark is the mark itself.
fn ps_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// The command a step runs, as it is shown and as it is run.
fn command(step: InstallStep, shell: Shell) -> String {
    match step {
        InstallStep::Script { posix, powershell } => match shell {
            Shell::Posix { .. } => posix.to_owned(),
            Shell::PowerShell => powershell.to_owned(),
        },
        InstallStep::Npm {
            package,
            version: Some(version),
        } => format!("npm install -g {package}@{version}"),
        InstallStep::Npm {
            package,
            version: None,
        } => format!("npm install -g {package}"),
    }
}

/// **The argv that runs `plan`**: each step's command printed and then run,
/// an npm step only where `npm` is on `PATH` — and otherwise the sentence
/// saying Node.js is needed, rather than `npm: command not found` — and last
/// the line that waits for Enter, so the output stays until it is read.
///
/// Pure over its inputs, so every shell's script is tested on every machine.
pub(super) fn install_argv(
    plan: Installer,
    shell: Shell,
    login_shell: Option<&str>,
    words: &InstallWords,
) -> Vec<String> {
    let steps = std::iter::once((plan.cli, &words.needs_node))
        .chain(plan.adapter.map(|step| (step, &words.adapter_needs_node)));
    match shell {
        Shell::Posix { login } => {
            let mut lines = Vec::new();
            for (step, missing) in steps {
                let command = command(step, shell);
                let run = format!("printf '\\n$ %s\\n' {}; {command}", sh_quote(&command));
                lines.push(match step {
                    InstallStep::Script { .. } => run,
                    InstallStep::Npm { .. } => format!(
                        "if command -v npm >/dev/null 2>&1; then {run}; else printf '\\n%s\\n' {}; fi",
                        sh_quote(missing)
                    ),
                });
            }
            lines.push(format!("printf '\\n%s ' {}", sh_quote(&words.close)));
            lines.push("read -r koru_reply".to_owned());
            let mut argv = vec![posix_shell(login_shell)];
            if login {
                argv.push("-l".to_owned());
            }
            argv.push("-c".to_owned());
            argv.push(lines.join("\n"));
            argv
        }
        Shell::PowerShell => {
            let mut statements = Vec::new();
            for (step, missing) in steps {
                let command = command(step, shell);
                let show = format!(
                    "Write-Host ''; Write-Host {}",
                    ps_quote(&format!("> {command}"))
                );
                statements.push(match step {
                    // **In a PowerShell of its own**: an installer script run
                    // through `iex` that says `exit` would end this one, and
                    // with it the steps after and the wait for Enter.
                    InstallStep::Script { .. } => format!(
                        "{show}; powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -Command {}",
                        ps_quote(&command)
                    ),
                    InstallStep::Npm { .. } => format!(
                        "if (Get-Command npm -ErrorAction SilentlyContinue) {{ {show}; {command} }} else {{ Write-Host ''; Write-Host {} }}",
                        ps_quote(missing)
                    ),
                });
            }
            statements.push("Write-Host ''".to_owned());
            statements.push(format!("$null = Read-Host {}", ps_quote(&words.close)));
            // `-ExecutionPolicy Bypass` for this process only: `npm` resolves
            // to `npm.ps1` first, which a machine on the default policy
            // refuses to run.
            vec![
                "powershell.exe".to_owned(),
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-ExecutionPolicy".to_owned(),
                "Bypass".to_owned(),
                "-Command".to_owned(),
                statements.join("; "),
            ]
        }
    }
}

impl MainView {
    /// ***Install*** on an agent's row: open a terminal tab running the
    /// vendor's install command for what is missing. A tab already
    /// installing that agent comes forward instead of a second installer
    /// starting. Answers whether anything changed.
    pub(super) fn install_agent(&mut self, key: &str) -> bool {
        if let Some(tab) = self
            .agent_installs
            .iter()
            .find(|install| install.key == key)
            .map(|install| install.tab)
            && self.bring_tab_forward(tab)
        {
            return true;
        }
        let rows = self.agent_provider_rows();
        let Some(row) = rows.iter().find(|row| row.key == key) else {
            return false;
        };
        let Some(plan) = install_plan(key, &row.status) else {
            return false;
        };
        let lang = crate::format::lang();
        let argv = install_argv(
            plan,
            Shell::this_machine(),
            std::env::var("SHELL").ok().as_deref(),
            &InstallWords::new(lang, &row.name),
        );
        let name = fill(t(lang, Key::SettingsAgentInstallTab), &[&row.name]);
        let profile =
            koru_session_profiles::LaunchProfile::custom(SessionProfileId::mint(), name, argv);
        let tab = self.open_launch_tab(profile, None, None);
        self.agent_installs.retain(|install| install.key != key);
        self.agent_installs.push(AgentInstall {
            key: key.to_owned(),
            tab,
            session: None,
        });
        true
    }

    /// Bring the tab `id` forward, wherever it is. Answers whether it was
    /// found.
    fn bring_tab_forward(&mut self, id: u64) -> bool {
        let Some((path, index)) = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .position(|tab| tab.id == id)
                .map(|index| (path, index))
        }) else {
            return false;
        };
        self.panes.set_focus(path);
        self.select(index);
        true
    }

    /// The session the tab `id` is showing, when it is a session tab that
    /// has learnt it.
    fn session_of_tab_id(&self, id: u64) -> Option<SessionId> {
        self.panes.iter_groups().find_map(|(_, group)| {
            group.tabs().iter().find_map(|tab| match &tab.view {
                TabView::Session(view) if tab.id == id => view.session,
                _ => None,
            })
        })
    }

    /// **Has an *Install* ended?** Read on every session catalog: an install
    /// whose session the catalog no longer has running — or whose tab
    /// closed before it learnt its session — is over, and the agents'
    /// status is read again (`refresh_agent_status`), so the row the person
    /// pressed *Install* on reads what is true now. Answers whether any
    /// ended.
    pub(super) fn watch_agent_installs(&mut self, sessions: &[SessionSummary]) -> bool {
        if self.agent_installs.is_empty() {
            return false;
        }
        let mut installs = std::mem::take(&mut self.agent_installs);
        for install in &mut installs {
            if install.session.is_none() {
                install.session = self.session_of_tab_id(install.tab);
            }
        }
        let tab_open = |view: &MainView, id: u64| {
            view.panes
                .iter_groups()
                .any(|(_, group)| group.tabs().iter().any(|tab| tab.id == id))
        };
        let before = installs.len();
        installs.retain(|install| match install.session {
            Some(session) => sessions.iter().any(|summary| {
                summary.id == session
                    && matches!(
                        summary.process,
                        ProcessState::Starting | ProcessState::Running
                    )
            }),
            None => tab_open(self, install.tab),
        });
        let ended = installs.len() < before;
        self.agent_installs = installs;
        if ended {
            self.refresh_agent_status();
        }
        ended
    }
}

#[cfg(test)]
mod tests;
