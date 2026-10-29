// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;

use super::*;

fn bridge() -> McpServerLaunch {
    McpServerLaunch {
        name: "koru".into(),
        command: r"C:\Program Files\Koru\koru-mcp.exe".into(),
        args: vec!["serve".into(), "--stdio".into()],
        env: Vec::new(),
    }
}

fn files() -> PathBuf {
    PathBuf::from(r"C:\Users\A B\AppData\Local\Koru\agent\sessions\bridge")
}

fn parsed(content: &str) -> serde_json::Value {
    serde_json::from_str(content).expect("the file is JSON")
}

#[test]
fn claude_and_qwen_take_the_mcp_servers_file_by_flag() {
    for (label, name) in [
        (ProfileLabel::Claude, "claude.json"),
        (ProfileLabel::QwenCode, "qwen.json"),
    ] {
        let (file, content) = config_file(label, &bridge()).unwrap();
        assert_eq!(file, name);
        let json = parsed(&content);
        assert_eq!(
            json["mcpServers"]["koru"]["command"],
            r"C:\Program Files\Koru\koru-mcp.exe"
        );
        assert_eq!(
            json["mcpServers"]["koru"]["args"],
            serde_json::json!(["serve", "--stdio"])
        );
        assert!(
            json["mcpServers"]["koru"].get("type").is_none(),
            "stdio is the default these two assume; a type field is Copilot's"
        );

        let launch = launch(label, &bridge(), &files()).unwrap();
        assert_eq!(
            launch.args,
            ["--mcp-config", &format!(r"{}\{name}", files().display())]
        );
        assert!(launch.env.is_empty());
    }
}

#[test]
fn copilot_takes_its_own_shape_through_an_at_file() {
    let (file, content) = config_file(ProfileLabel::Copilot, &bridge()).unwrap();
    assert_eq!(file, "copilot.json");
    let server = &parsed(&content)["mcpServers"]["koru"];
    assert_eq!(server["type"], "local");
    assert_eq!(server["command"], r"C:\Program Files\Koru\koru-mcp.exe");
    assert_eq!(server["args"], serde_json::json!(["serve", "--stdio"]));
    assert_eq!(server["tools"], serde_json::json!(["*"]));

    let launch = launch(ProfileLabel::Copilot, &bridge(), &files()).unwrap();
    assert_eq!(
        launch.args,
        [
            "--additional-mcp-config",
            &format!(r"@{}\copilot.json", files().display())
        ]
    );
    assert!(launch.env.is_empty());
}

#[test]
fn codex_tabs_take_the_app_servers_overrides_bare() {
    for label in [ProfileLabel::CodexEmbedded, ProfileLabel::CodexCli] {
        assert!(config_file(label, &bridge()).is_none(), "no file for Codex");
        let launch = launch(label, &bridge(), &files()).unwrap();
        assert_eq!(
            launch.args,
            [
                "-c",
                r"mcp_servers.koru.command='C:\Program Files\Koru\koru-mcp.exe'",
                "-c",
                "mcp_servers.koru.args=['serve','--stdio']",
            ]
        );
        assert!(launch.env.is_empty());
    }
}

#[test]
fn opencode_takes_an_inline_configuration_in_its_environment() {
    assert!(config_file(ProfileLabel::OpenCode, &bridge()).is_none());
    let launch = launch(ProfileLabel::OpenCode, &bridge(), &files()).unwrap();
    assert!(launch.args.is_empty(), "nothing on the command line");
    let [(name, value)] = launch.env.as_slice() else {
        panic!("one variable, got {:?}", launch.env);
    };
    assert_eq!(name, "OPENCODE_CONFIG_CONTENT");
    let json = parsed(value.to_str().unwrap());
    assert_eq!(json["mcp"]["koru"]["type"], "local");
    assert_eq!(
        json["mcp"]["koru"]["command"],
        serde_json::json!([r"C:\Program Files\Koru\koru-mcp.exe", "serve", "--stdio"])
    );
    assert_eq!(json["mcp"]["koru"]["enabled"], true);
}

#[test]
fn clis_without_a_per_process_form_and_shells_get_nothing() {
    for label in [
        ProfileLabel::Grok,
        ProfileLabel::Kimi,
        ProfileLabel::Antigravity,
        ProfileLabel::Muse,
        ProfileLabel::Gemini,
        ProfileLabel::PlatformShell,
        ProfileLabel::PowerShell,
        ProfileLabel::Bash,
        ProfileLabel::Custom,
    ] {
        assert!(program(label).is_none(), "{label:?}");
        assert!(config_file(label, &bridge()).is_none(), "{label:?}");
        assert!(launch(label, &bridge(), &files()).is_none(), "{label:?}");
    }
}

#[test]
fn every_rendered_argument_is_transparent_to_the_windows_shim_boundary() {
    for label in [
        ProfileLabel::Claude,
        ProfileLabel::QwenCode,
        ProfileLabel::Copilot,
        ProfileLabel::CodexEmbedded,
        ProfileLabel::CodexCli,
    ] {
        let launch = launch(label, &bridge(), &files()).unwrap();
        for argument in &launch.args {
            assert!(
                koru_session_profiles::cmd_transparent(argument),
                "{label:?}: {argument}"
            );
        }
    }
}

#[test]
fn the_profiles_argv_comes_first_and_a_missing_bridge_changes_nothing() {
    let own = vec!["claude".to_owned()];
    assert_eq!(argv(&own, None), own);
    let bridge = TabBridge {
        args: vec!["--mcp-config".into(), "x.json".into()],
        env: Vec::new(),
    };
    assert_eq!(
        argv(&own, Some(&bridge)),
        ["claude", "--mcp-config", "x.json"]
    );
}

#[test]
fn a_path_that_is_not_utf8_is_refused_rather_than_rendered_lossily() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let mut odd = bridge();
        odd.command = OsString::from_vec(vec![0x66, 0x6f, 0x80]);
        assert!(config_file(ProfileLabel::Claude, &odd).is_none());
        assert!(launch(ProfileLabel::CodexEmbedded, &odd, &files()).is_none());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        let mut odd = bridge();
        odd.command = OsString::from_wide(&[0x66, 0xd800]);
        assert!(config_file(ProfileLabel::Claude, &odd).is_none());
        assert!(launch(ProfileLabel::CodexEmbedded, &odd, &files()).is_none());
    }
}
