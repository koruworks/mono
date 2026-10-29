// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::McpToolAllowlist;

/// The six the run endpoint publishes, in the spelling `run_mcp::tools`
/// uses. A seventh name here would be a tool the model is offered and the
/// endpoint refuses.
const RUN_TOOLS: [&str; 6] = [
    "attach_to_run",
    "create",
    "create_version",
    "get",
    "read",
    "search",
];

fn delegated() -> DelegationLaunch {
    DelegationLaunch {
        model: None,
        mcp_endpoint: "http://127.0.0.1:4321/v1/runs/r1/mcp".parse().unwrap(),
        credential: Some("koru_run_0123456789abcdef".parse().unwrap()),
        tools: McpToolAllowlist::new(RUN_TOOLS.iter().map(|name| name.parse().unwrap())).unwrap(),
        resume: None,
    }
}

fn strings(argv: &[OsString]) -> Vec<String> {
    argv.iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_manual_launch_is_unchanged_and_carries_no_environment() {
    for version in super::super::SUPPORTED_CODEX_VERSIONS {
        assert_eq!(
            render_argv(None, &version.parse().unwrap()),
            vec![OsString::from("app-server"), OsString::from("--stdio")]
        );
    }
    assert!(child_env(None).is_empty());
}

#[test]
fn a_delegated_launch_renders_the_six_tools_and_names_the_bearer_by_variable() {
    let delegation = delegated();
    let argv = strings(&render_argv(Some(&delegation), &"0.150.1".parse().unwrap()));
    assert_eq!(
        argv,
        vec![
            "app-server".to_owned(),
            "--stdio".to_owned(),
            "-c".to_owned(),
            "mcp_servers.koru_run.url='http://127.0.0.1:4321/v1/runs/r1/mcp'".to_owned(),
            "-c".to_owned(),
            "mcp_servers.koru_run.enabled_tools=['attach_to_run','create','create_version','get','read','search']".to_owned(),
            "-c".to_owned(),
            "mcp_servers.koru_run.bearer_token_env_var='KORU_RUN_MCP_TOKEN'".to_owned(),
            "-c".to_owned(),
            "features.multi_agent=false".to_owned(),
        ]
    );
    assert!(
        argv.iter().all(|value| !value.contains('"')),
        "a double quote would not survive a Windows .cmd shim"
    );
    let mut modern = argv;
    modern.extend(["-c".to_owned(), "features.multi_agent_v2=false".to_owned()]);
    for version in [
        "0.153.4", "0.154.0", "0.156.1", "0.158.0", "0.163.0", "0.165.0",
    ] {
        assert_eq!(
            strings(&render_argv(Some(&delegation), &version.parse().unwrap())),
            modern,
            "both native child-agent switches must be off on {version}"
        );
    }
}

/// The whole point of `bearer_token_env_var`: argv is world-readable on
/// every platform Koru runs on, so the credential must not be on it.
#[test]
fn the_bearer_reaches_the_child_by_environment_and_never_by_argv() {
    let delegation = delegated();
    let secret = delegation.credential.as_ref().unwrap().as_str().to_owned();

    for element in strings(&render_argv(Some(&delegation), &"0.154.0".parse().unwrap())) {
        assert!(
            !element.contains(&secret),
            "the bearer is on argv: {element}"
        );
    }

    let env = child_env(Some(&delegation));
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].0, OsString::from(TOKEN_ENV_VAR));
    assert_eq!(env[0].1, OsString::from(&secret));
}

/// An endpoint with no bearer is an endpoint that would answer `401`. The
/// override is left out rather than pointing Codex at a variable that will
/// not be set — an unset `bearer_token_env_var` is a startup failure, and
/// no header at all is the same refusal with a clearer cause.
#[test]
fn a_delegation_without_a_credential_names_no_variable() {
    let delegation = DelegationLaunch {
        model: None,
        credential: None,
        ..delegated()
    };
    let argv = strings(&render_argv(Some(&delegation), &"0.154.0".parse().unwrap()));
    assert!(
        !argv.iter().any(|value| value.contains("bearer_token")),
        "{argv:?}"
    );
    assert!(child_env(Some(&delegation)).is_empty());
}

/// An empty allowlist is a legal, read-only launch: the table is written
/// empty rather than omitted, so the default (every tool the server offers)
/// cannot leak back in.
#[test]
fn an_empty_allowlist_is_written_empty() {
    let delegation = DelegationLaunch {
        model: None,
        tools: McpToolAllowlist::default(),
        ..delegated()
    };
    assert!(
        strings(&render_argv(Some(&delegation), &"0.154.0".parse().unwrap()))
            .iter()
            .any(|value| value == "mcp_servers.koru_run.enabled_tools=[]")
    );
}

/// `Debug` on the launch says whether it is authenticated, never with what.
#[test]
fn the_launch_never_prints_its_bearer() {
    let delegation = delegated();
    let rendered = format!("{delegation:?}");
    assert!(
        !rendered.contains("koru_run_0123456789abcdef"),
        "{rendered}"
    );
    assert!(rendered.contains("authenticated: true"), "{rendered}");
}

#[test]
fn a_stdio_bridge_renders_as_command_and_args_overrides_in_literal_strings() {
    let argv = strings(&render_argv_with(
        None,
        &[crate::providers::McpServerLaunch {
            name: "koru".into(),
            command: "C:\\Koru\\koru-mcp.exe".into(),
            args: vec![
                "serve".into(),
                "--stdio".into(),
                "--root".into(),
                "D:\\root".into(),
            ],
            env: Vec::new(),
        }],
        &"0.156.1".parse().unwrap(),
    ));
    let overrides: Vec<&str> = argv
        .iter()
        .skip_while(|element| element.as_str() != "-c")
        .filter(|element| element.as_str() != "-c")
        .map(String::as_str)
        .collect();
    assert_eq!(
        overrides,
        vec![
            "mcp_servers.koru.command='C:\\Koru\\koru-mcp.exe'",
            "mcp_servers.koru.args=['serve','--stdio','--root','D:\\root']",
        ]
    );
    assert!(
        !argv
            .iter()
            .any(|element| element.contains("features.multi_agent")),
        "a manual launch carries no delegation override"
    );
}
