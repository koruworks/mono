// SPDX-License-Identifier: MPL-2.0

//! Koru's tools in a terminal tab: the chat's bridge
//! ([ADR-430](../../../../docs/decisions/ADR-430-chatagentbridge.md)), handed
//! to a raw agent per launch, in whatever per-process form its CLI takes.
//!
//! A chat's agent gets `koru-mcp` in `session/new`. A tab runs the bare CLI,
//! which otherwise knows only what the person configured by hand — so the
//! same person who can write documents from a chat could not from a tab.
//! Every CLI that can take an MCP server for one process now gets the bridge
//! on its command line or in its environment. Nothing is written into the
//! person's own configuration, so there is nothing to remove when the tab
//! closes, and a tab opened outside Koru is exactly what it was (ADR-470).
//!
//! The carriers, as each CLI documents them:
//!
//! - **Claude Code** `--mcp-config <file>` and **Qwen Code**
//!   `--mcp-config <file>`: a `{"mcpServers": {…}}` file;
//! - **GitHub Copilot** `--additional-mcp-config @<file>`: its own shape,
//!   with `"type": "local"` and the tool filter that admits every tool;
//! - **Codex** `-c mcp_servers.koru.command=… -c …args=[…]`: the overrides
//!   the app-server launch already renders;
//! - **OpenCode** `OPENCODE_CONFIG_CONTENT`: an inline `opencode.json`.
//!
//! Grok, Kimi, Antigravity and Muse have no per-process form (their `mcp`
//! verbs write the person's configuration), so their tabs are unchanged.
//!
//! The files are written once, when the host starts, under the sessions
//! directory; their content is a pure function ([`config_file`]), so the
//! tests never touch disk. On Windows the npm-installed CLIs are `.cmd`
//! shims, and every argument rendered here is transparent to cmd.exe
//! (`koru_session_profiles::cmd_transparent`): a flag, a path, a TOML
//! literal in single quotes.

use std::ffi::OsString;
use std::path::Path;

use koru_session_profiles::ProfileLabel;
use koru_session_runtime::providers::McpServerLaunch;

/// The variable OpenCode reads an inline configuration from.
const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";

/// What a raw launch adds for the bridge: arguments after the profile's own
/// and variables set on the child's environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TabBridge {
    pub args: Vec<String>,
    pub env: Vec<(OsString, OsString)>,
}

/// The program a tab's label runs, for the bridge's paging decision, or
/// `None` for a label whose CLI has no per-process form.
pub(super) fn program(label: ProfileLabel) -> Option<&'static str> {
    Some(match label {
        ProfileLabel::Claude => "claude",
        ProfileLabel::CodexEmbedded | ProfileLabel::CodexCli => "codex",
        ProfileLabel::Copilot => "copilot",
        ProfileLabel::QwenCode => "qwen",
        ProfileLabel::OpenCode => "opencode",
        ProfileLabel::PlatformShell
        | ProfileLabel::PowerShell
        | ProfileLabel::Bash
        | ProfileLabel::Grok
        | ProfileLabel::Kimi
        | ProfileLabel::Gemini
        | ProfileLabel::Antigravity
        | ProfileLabel::Muse
        | ProfileLabel::Custom => return None,
    })
}

/// The configuration file a label's CLI reads, as `(file name, content)`,
/// for the CLIs that take one. Codex takes overrides and OpenCode an
/// environment variable, so they have none.
pub(super) fn config_file(
    label: ProfileLabel,
    bridge: &McpServerLaunch,
) -> Option<(&'static str, String)> {
    let command = bridge.command.to_str()?;
    let args = bridge
        .args
        .iter()
        .map(|argument| argument.to_str())
        .collect::<Option<Vec<_>>>()?;
    let (name, server) = match label {
        ProfileLabel::Claude => (
            "claude.json",
            serde_json::json!({ "command": command, "args": args }),
        ),
        ProfileLabel::QwenCode => (
            "qwen.json",
            serde_json::json!({ "command": command, "args": args }),
        ),
        ProfileLabel::Copilot => (
            "copilot.json",
            serde_json::json!({
                "type": "local",
                "command": command,
                "args": args,
                "tools": ["*"],
            }),
        ),
        _ => return None,
    };
    let mut servers = serde_json::Map::new();
    servers.insert(bridge.name.clone(), server);
    let body = serde_json::json!({ "mcpServers": servers });
    Some((name, serde_json::to_string_pretty(&body).ok()?))
}

/// Everything a raw launch of `label` adds, given the directory the files
/// were written to. `None` for a CLI with no per-process form, and for a
/// path that is not UTF-8: an argument is never rendered lossily.
pub(super) fn launch(
    label: ProfileLabel,
    bridge: &McpServerLaunch,
    files: &Path,
) -> Option<TabBridge> {
    match label {
        ProfileLabel::Claude | ProfileLabel::QwenCode | ProfileLabel::Copilot => {
            let (name, _) = config_file(label, bridge)?;
            let path = files.join(name).to_str()?.to_owned();
            let args = match label {
                ProfileLabel::Copilot => {
                    vec!["--additional-mcp-config".to_owned(), format!("@{path}")]
                }
                _ => vec!["--mcp-config".to_owned(), path],
            };
            Some(TabBridge {
                args,
                env: Vec::new(),
            })
        }
        ProfileLabel::CodexEmbedded | ProfileLabel::CodexCli => {
            // The overrides render a path lossily, as the app-server launch
            // may; a tab's argument is refused instead.
            std::iter::once(&bridge.command)
                .chain(&bridge.args)
                .try_for_each(|argument| argument.to_str().map(|_| ()))?;
            let args =
                koru_session_runtime::providers::mcp_server_overrides(std::slice::from_ref(bridge))
                    .into_iter()
                    .map(|argument| argument.into_string().ok())
                    .collect::<Option<Vec<_>>>()?;
            Some(TabBridge {
                args,
                env: Vec::new(),
            })
        }
        ProfileLabel::OpenCode => {
            let command = std::iter::once(&bridge.command)
                .chain(&bridge.args)
                .map(|argument| argument.to_str())
                .collect::<Option<Vec<_>>>()?;
            let mut servers = serde_json::Map::new();
            servers.insert(
                bridge.name.clone(),
                serde_json::json!({ "type": "local", "command": command, "enabled": true }),
            );
            let body = serde_json::json!({ "mcp": servers });
            Some(TabBridge {
                args: Vec::new(),
                env: vec![(OPENCODE_CONFIG_CONTENT.into(), body.to_string().into())],
            })
        }
        _ => None,
    }
}

/// Write every file-taking label's file under `directory`, once per host
/// start. `bridge_for` is the bridge as that program's session starts it,
/// paged or not. The content is deterministic, so rewriting is idempotent
/// and a tab that is already open keeps reading the same thing.
pub(super) fn write_files(
    directory: &Path,
    bridge_for: impl Fn(&str) -> McpServerLaunch,
) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    for label in [
        ProfileLabel::Claude,
        ProfileLabel::QwenCode,
        ProfileLabel::Copilot,
    ] {
        let Some(program) = program(label) else {
            continue;
        };
        if let Some((name, content)) = config_file(label, &bridge_for(program)) {
            std::fs::write(directory.join(name), content)?;
        }
    }
    Ok(())
}

/// The argv a raw launch is resolved from: the profile's own, then the
/// bridge's arguments.
pub(super) fn argv(profile_argv: &[String], bridge: Option<&TabBridge>) -> Vec<String> {
    let mut argv = profile_argv.to_vec();
    if let Some(bridge) = bridge {
        argv.extend(bridge.args.iter().cloned());
    }
    argv
}

#[cfg(test)]
mod tests;
