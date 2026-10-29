// SPDX-License-Identifier: MPL-2.0

//! *Install Koru MCP*, from the right-click menu of an agent's tab
//! ([ADR-472](../../../../docs/decisions/ADR-472-installmcp.md)).
//!
//! Claude Code, Codex, Copilot, Qwen Code and OpenCode are given Koru's bridge
//! for the one launch (ADR-470). Grok and Antigravity have no such form, only
//! a registration in their own configuration, which Koru makes **when asked**:
//! the person chose the tab's menu row, the command is the agent's own
//! `mcp add`, and nothing is written otherwise. The command runs on a worker
//! thread so the window never waits on another program, with a deadline, and
//! the answer is a toast.

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use koru_i18n::{Key, fill, t};
use koru_session_protocol::{SessionProfileKey, built_in_profile};

use super::MainView;
use super::tab::TabView;

/// Longer than an agent's registration takes by a wide margin, short enough
/// that a hung one does not hold the menu row disabled for the session.
const DEADLINE: Duration = Duration::from_secs(60);
/// The name the bridge is registered under, the one a chat and a tab call it.
const SERVER: &str = "koru";

/// An agent that takes the bridge by registration.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Target {
    Grok,
    Antigravity,
}

impl Target {
    pub(super) fn of(profile: &SessionProfileKey) -> Option<Self> {
        match profile {
            SessionProfileKey::BuiltIn(key) if key.as_str() == built_in_profile::GROK => {
                Some(Self::Grok)
            }
            SessionProfileKey::BuiltIn(key) if key.as_str() == built_in_profile::ANTIGRAVITY => {
                Some(Self::Antigravity)
            }
            _ => None,
        }
    }

    pub(super) fn program(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::Antigravity => "agy",
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Grok => "Grok",
            Self::Antigravity => "Antigravity",
        }
    }
}

/// The arguments after the program: `mcp add koru -- <bridge> serve --stdio
/// [--root <root>]`. Both agents document `--` for a command whose own
/// arguments begin with a dash, and both `add` verbs update an entry of the
/// same name, so asking twice leaves one entry.
pub(super) fn arguments(bridge: &Path, root: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "mcp".to_owned(),
        "add".to_owned(),
        SERVER.to_owned(),
        "--".to_owned(),
        bridge.display().to_string(),
        "serve".to_owned(),
        "--stdio".to_owned(),
    ];
    if let Some(root) = root {
        args.push("--root".to_owned());
        args.push(root.display().to_string());
    }
    args
}

/// What the worker found, as the words the toast says.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Installed,
    Failed(String),
    Removed,
    RemoveFailed(String),
    /// What the agent's listing said: whether `koru` is registered.
    Listed(bool),
}

#[derive(Default)]
pub(super) struct Install {
    running: Option<(Target, Receiver<Outcome>)>,
    /// Whether each agent has `koru` registered, as last seen. Absent until
    /// the first look; the menu says *Install* until then.
    registered: std::collections::HashMap<Target, bool>,
}

/// Run the agent's own `mcp` command to its end or its deadline, and give back
/// what it printed.
fn exec(target: Target, args: &[String]) -> Result<String, String> {
    let Some(program) = koru_session_profiles::resolve_executable(target.program()) else {
        return Err(format!("{} is not on PATH", target.program()));
    };
    // The same boundary a tab's launch goes through, so a `.cmd` shim is run
    // the way Koru runs one and an argument it cannot trust is refused.
    let Some(argv) = koru_session_profiles::platform_launch_argv(program, args) else {
        return Err("the command line cannot be passed safely".into());
    };
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Err(error.to_string()),
    };
    // The output is read on this worker thread; a second thread holds the
    // deadline, because `wait_with_output` blocks until the program ends.
    let id = child.id();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(child.wait_with_output());
    });
    let output = match finished.recv_timeout(DEADLINE) {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(error.to_string()),
        Err(_) => {
            kill(id);
            return Err("it did not finish in a minute".into());
        }
    };
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = if said.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        said
    };
    let last = said
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it exited with an error");
    Err(last.chars().take(160).collect())
}

/// Run the registration.
fn run(target: Target, bridge: &Path, root: Option<&Path>) -> Outcome {
    match exec(target, &arguments(bridge, root)) {
        Ok(_) => Outcome::Installed,
        Err(why) => Outcome::Failed(why),
    }
}

/// Take the registration out again: the agent's own `mcp remove koru`.
fn remove(target: Target) -> Outcome {
    let args = ["mcp".to_owned(), "remove".to_owned(), SERVER.to_owned()];
    match exec(target, &args) {
        Ok(_) => Outcome::Removed,
        Err(why) => Outcome::RemoveFailed(why),
    }
}

/// Whether the agent lists a server named `koru`. Both agents print one per
/// line, the name first (Grok as `koru: <command>`, Antigravity as a table).
fn lists_koru(listing: &str) -> bool {
    listing.lines().any(|line| {
        line.split(|c: char| c.is_whitespace() || c == ':')
            .find(|word| !word.is_empty())
            == Some(SERVER)
    })
}

/// Ask the agent what it has registered.
fn probe(target: Target) -> Outcome {
    let args = ["mcp".to_owned(), "list".to_owned()];
    match exec(target, &args) {
        Ok(listing) => Outcome::Listed(lists_koru(&listing)),
        // A failed look is not an answer: the row stays *Install*.
        Err(_) => Outcome::Listed(false),
    }
}

/// End a registration that outlived its deadline.
fn kill(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

impl MainView {
    /// The agent a tab is, when it is one that takes the bridge by
    /// registration.
    pub(super) fn agent_mcp_target(&self, tab: usize) -> Option<Target> {
        match &self.tabs().get(tab)?.view {
            TabView::Session(view) => view.profile_key.as_ref().and_then(Target::of),
            _ => None,
        }
    }

    pub(super) fn agent_mcp_running(&self) -> bool {
        self.agent_mcp.running.is_some()
    }

    /// Whether the tab's agent has Koru's bridge registered, as last seen.
    pub(super) fn agent_mcp_registered(&self, tab: usize) -> bool {
        self.agent_mcp_target(tab)
            .and_then(|target| self.agent_mcp.registered.get(&target).copied())
            .unwrap_or(false)
    }

    fn start_agent_mcp(&mut self, target: Target, work: impl FnOnce() -> Outcome + Send + 'static) {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        self.agent_mcp.running = Some((target, receiver));
    }

    /// *Remove Koru MCP*: the registration, taken out again.
    pub(super) fn remove_agent_mcp(&mut self, tab: usize) -> bool {
        let Some(target) = self.agent_mcp_target(tab) else {
            return false;
        };
        if self.agent_mcp_running() {
            return true;
        }
        self.start_agent_mcp(target, move || remove(target));
        true
    }

    /// Look once at each kind of agent a tab shows, so the menu knows whether
    /// to say *Install* or *Remove*.
    fn probe_agent_mcp(&mut self) {
        let unseen = (0..self.tabs().len())
            .filter_map(|tab| self.agent_mcp_target(tab))
            .find(|target| !self.agent_mcp.registered.contains_key(target));
        if let Some(target) = unseen {
            self.start_agent_mcp(target, move || probe(target));
        }
    }

    /// *Install Koru MCP*: start the registration for the tab's agent.
    pub(super) fn install_agent_mcp(&mut self, tab: usize) -> bool {
        let Some(target) = self.agent_mcp_target(tab) else {
            return false;
        };
        if self.agent_mcp_running() {
            return true;
        }
        let (bridge, root) = crate::welcome::mcp::bridge();
        self.start_agent_mcp(target, move || run(target, &bridge, root.as_deref()));
        true
    }

    /// Collect the worker's answer and say it. Called every frame; a fast
    /// return while nothing runs.
    pub(super) fn poll_agent_mcp(&mut self) {
        let Some((target, receiver)) = &self.agent_mcp.running else {
            self.probe_agent_mcp();
            return;
        };
        let target = *target;
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Outcome::Failed("the worker stopped".into()),
        };
        self.agent_mcp.running = None;
        let lang = crate::format::lang();
        let message = match outcome {
            Outcome::Listed(found) => {
                self.agent_mcp.registered.insert(target, found);
                return;
            }
            Outcome::Installed => {
                self.agent_mcp.registered.insert(target, true);
                fill(t(lang, Key::AgentMcpInstalled), &[target.name()])
            }
            Outcome::Failed(why) => fill(t(lang, Key::AgentMcpFailed), &[target.name(), &why]),
            Outcome::Removed => {
                self.agent_mcp.registered.insert(target, false);
                fill(t(lang, Key::AgentMcpRemoved), &[target.name()])
            }
            Outcome::RemoveFailed(why) => {
                fill(t(lang, Key::AgentMcpRemoveFailed), &[target.name(), &why])
            }
        };
        self.toast = Some((
            koru_ui::Toast::new(message, None, Instant::now()),
            super::ToastPayload::None,
        ));
    }
}

#[cfg(test)]
mod tests;
