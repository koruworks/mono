// SPDX-License-Identifier: MPL-2.0

//! How a Codex launch is spelled on the command line — and the one thing
//! that is deliberately not spelled there.
//!
//! The app-server accepts `-c key=value` configuration overrides, each
//! parsed as TOML, so Koru's MCP endpoint, the tool allowlist and the
//! multi-agent switch all ride argv elements rather than a file under the
//! person's `CODEX_HOME` — which would have needed a private home and lost
//! their login, or edited a config they own. Nothing here is a shell
//! string: every element is passed as one argv entry and never reparsed.
//!
//! **The run's bearer is the exception, and it rides the environment.**
//! Codex 0.153.4's `RawMcpServerConfig` has three ways to authenticate a
//! streamable-HTTP server — `bearer_token_env_var`, `http_headers` and
//! `env_http_headers` — and refuses a fourth, `bearer_token`, in as many
//! words: *"uses unsupported `bearer_token`; set `bearer_token_env_var`"*.
//! Only the first two would put the credential itself on argv, where every
//! process on the machine can read it; `bearer_token_env_var` names a
//! variable instead, and [`child_env`] is what sets that variable on the
//! child and on nothing else (ADR-359).

use std::ffi::OsString;

use koru_session_protocol::DelegationLaunch;

use super::ProviderVersion;

/// The name under which Koru's run endpoint appears in the app-server's
/// `mcp_servers` table. Stable so a tool call can be attributed to it, and
/// **not `koru`**: a person who uses Codex with Koru already has a stdio
/// `[mcp_servers.koru]` — the desktop bridge — in their own `config.toml`,
/// and an override that merges a `url` into that table is refused by the
/// CLI ("url is not supported for stdio"). The run's endpoint is a
/// different server with a different authority, and it gets its own name.
pub const KORU_MCP_SERVER_NAME: &str = "koru_run";

/// The variable the child reads its bearer from, named to Codex by
/// `mcp_servers.koru_run.bearer_token_env_var`. Declared in `koru-env` so
/// `koru.bat envs` and `reference/ENV.md` account for it, and marked
/// a secret there so no listing ever prints its value.
pub const TOKEN_ENV_VAR: &str = koru_env::name::KORU_RUN_MCP_TOKEN;

/// Argv after the executable. Without a delegation this is exactly the
/// manual profile's launch; with one, the overrides follow it. The tests'
/// shorthand for [`render_argv_with`] with no MCP servers of Koru's own.
#[cfg(test)]
pub(super) fn render_argv(
    delegation: Option<&DelegationLaunch>,
    executable_version: &ProviderVersion,
) -> Vec<OsString> {
    render_argv_with(delegation, &[], executable_version)
}

/// The `-c` overrides that put stdio MCP servers of Koru's own into a Codex
/// launch: `mcp_servers.<name>.command` and `.args`, each a TOML literal
/// string in single quotes (see [`overrides`] for why), so a path with
/// backslashes rides unchanged and a quote can never need escaping. The
/// app-server launch appends these after its own; a raw `codex` tab takes
/// them bare, after the profile's program (`tab_bridge` in
/// `koru-session-host`).
pub fn mcp_server_overrides(mcp_servers: &[crate::providers::McpServerLaunch]) -> Vec<OsString> {
    let mut argv = Vec::new();
    for server in mcp_servers {
        let literal = |value: &OsString| format!("'{}'", value.to_string_lossy().replace('\'', ""));
        argv.push("-c".into());
        argv.push(
            format!(
                "mcp_servers.{}.command={}",
                server.name,
                literal(&server.command)
            )
            .into(),
        );
        argv.push("-c".into());
        argv.push(
            format!(
                "mcp_servers.{}.args=[{}]",
                server.name,
                server
                    .args
                    .iter()
                    .map(literal)
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .into(),
        );
    }
    argv
}

/// The launch with stdio MCP servers of Koru's own beside any delegation:
/// each becomes `mcp_servers.<name>.command` and `.args` overrides, in the
/// same TOML literal-string form as the delegated endpoint's, so a path
/// with backslashes rides unchanged and a quote can never need escaping.
pub(super) fn render_argv_with(
    delegation: Option<&DelegationLaunch>,
    mcp_servers: &[crate::providers::McpServerLaunch],
    executable_version: &ProviderVersion,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = koru_session_profiles::CODEX_REGISTRATION
        .arguments
        .iter()
        .map(OsString::from)
        .collect();
    if let Some(delegation) = delegation {
        for element in overrides(delegation, executable_version) {
            argv.push("-c".into());
            argv.push(element.into());
        }
    }
    argv.extend(mcp_server_overrides(mcp_servers));
    argv
}

/// What the child process gets in its environment that this one does not:
/// the run endpoint's bearer, under [`TOKEN_ENV_VAR`].
///
/// Empty for a manual profile and for a delegation that carries no
/// credential — an endpoint that would refuse every call anyway. The value
/// exists only in this vector and in the child's own environment block; it
/// is never logged, never written to a file, and never an argv element.
pub(super) fn child_env(delegation: Option<&DelegationLaunch>) -> Vec<(OsString, OsString)> {
    delegation
        .and_then(|delegation| delegation.credential.as_ref())
        .map(|bearer| vec![(TOKEN_ENV_VAR.into(), bearer.as_str().into())])
        .unwrap_or_default()
}

/// The `key=value` overrides a delegated launch carries, each a TOML value
/// on the right of the `=`. The endpoint is a streamable-HTTP server; the
/// allowlist becomes `enabled_tools`, so a tool Koru did not name is not
/// offered to the model; the bearer is named by the variable it will be
/// found in, never by its value; and the app-server's own agent spawning is
/// turned off, because a run's hierarchy is Koru's to manage (§7.3).
///
/// **Strings are TOML literal strings, in single quotes.** On Windows the
/// npm-installed `codex` is a `.cmd` shim, and a double quote inside an
/// argument is interpreted by every layer between here and node — the
/// checked-in fixture shim loses them outright. A single quote is a plain
/// character to all of them, and `McpEndpoint` and `McpToolName` both
/// refuse it, so the value can never need escaping.
fn overrides(delegation: &DelegationLaunch, executable_version: &ProviderVersion) -> Vec<String> {
    let tools = delegation
        .tools
        .iter()
        .map(|tool| format!("'{}'", tool.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let mut overrides = vec![
        format!(
            "mcp_servers.{KORU_MCP_SERVER_NAME}.url='{}'",
            delegation.mcp_endpoint.as_str()
        ),
        format!("mcp_servers.{KORU_MCP_SERVER_NAME}.enabled_tools=[{tools}]"),
    ];
    if delegation.credential.is_some() {
        overrides.push(format!(
            "mcp_servers.{KORU_MCP_SERVER_NAME}.bearer_token_env_var='{TOKEN_ENV_VAR}'"
        ));
    }
    overrides.push("features.multi_agent=false".to_owned());
    // These reviewed versions expose a separate v2 switch. Disable it
    // explicitly instead of depending on the person's configuration.
    // Preserve the older pin's launch: its v2 switch was not verified.
    // 0.156.1: `codex features list` on the installed CLI names
    // `multi_agent_v2` (stable, off by default) and accepts the override
    // (2026-09-25); 0.158.0 the same (2026-09-29); 0.163.0 the same, with
    // `multi_agent_v2_dynamic_tools` beside it, under development and off
    // (2026-10-27).
    if matches!(
        executable_version.as_str(),
        "0.153.4" | "0.154.0" | "0.156.1" | "0.158.0" | "0.163.0" | "0.165.0"
    ) {
        overrides.push("features.multi_agent_v2=false".to_owned());
    }
    overrides
}

#[cfg(test)]
mod tests;
