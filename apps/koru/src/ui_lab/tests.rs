// SPDX-License-Identifier: MPL-2.0

//! **That the Lab starts none of the ordinary app**, checked by structure.
//!
//! "No Store was opened" cannot be asserted by trying to reach one: a person
//! running the suite with Koru open has a Store, and the test would be
//! reporting on their desktop. What can be pinned is the shape of the code —
//! where `main` hands over, and what the Lab's own sources are able to name.
//! Both files are compiled in as text; nothing is read at run time.

/// The whole of `main.rs`, read as text.
const MAIN: &str = include_str!("../main.rs");

/// Every non-test source file of the Lab.
const LAB_SOURCES: &[(&str, &str)] = &[
    ("ui_lab.rs", include_str!("../ui_lab.rs")),
    ("ui_lab/placement.rs", include_str!("placement.rs")),
    ("ui_lab/registry.rs", include_str!("registry.rs")),
    ("ui_lab/registry/app.rs", include_str!("registry/app.rs")),
    (
        "ui_lab/registry/app/companion_apps.rs",
        include_str!("registry/app/companion_apps.rs"),
    ),
    (
        "ui_lab/registry/app/leaf.rs",
        include_str!("registry/app/leaf.rs"),
    ),
    (
        "ui_lab/registry/app/mainview.rs",
        include_str!("registry/app/mainview.rs"),
    ),
    (
        "ui_lab/registry/app/mini_app.rs",
        include_str!("registry/app/mini_app.rs"),
    ),
    (
        "ui_lab/registry/app/pages.rs",
        include_str!("registry/app/pages.rs"),
    ),
    (
        "ui_lab/registry/app/panes.rs",
        include_str!("registry/app/panes.rs"),
    ),
    (
        "ui_lab/registry/app/tasks_view.rs",
        include_str!("registry/app/tasks_view.rs"),
    ),
    (
        "ui_lab/registry/compositions.rs",
        include_str!("registry/compositions.rs"),
    ),
    (
        "ui_lab/registry/controls.rs",
        include_str!("registry/controls.rs"),
    ),
    (
        "ui_lab/registry/domain.rs",
        include_str!("registry/domain.rs"),
    ),
    (
        "ui_lab/registry/domain/agents.rs",
        include_str!("registry/domain/agents.rs"),
    ),
    (
        "ui_lab/registry/domain/ai.rs",
        include_str!("registry/domain/ai.rs"),
    ),
    (
        "ui_lab/registry/domain/app_shell.rs",
        include_str!("registry/domain/app_shell.rs"),
    ),
    (
        "ui_lab/registry/domain/appearance.rs",
        include_str!("registry/domain/appearance.rs"),
    ),
    (
        "ui_lab/registry/domain/artifacts.rs",
        include_str!("registry/domain/artifacts.rs"),
    ),
    (
        "ui_lab/registry/domain/canvas.rs",
        include_str!("registry/domain/canvas.rs"),
    ),
    (
        "ui_lab/registry/domain/chats.rs",
        include_str!("registry/domain/chats.rs"),
    ),
    (
        "ui_lab/registry/domain/editor.rs",
        include_str!("registry/domain/editor.rs"),
    ),
    (
        "ui_lab/registry/domain/markdown_view.rs",
        include_str!("registry/domain/markdown_view.rs"),
    ),
    (
        "ui_lab/registry/domain/runs.rs",
        include_str!("registry/domain/runs.rs"),
    ),
    (
        "ui_lab/registry/domain/search.rs",
        include_str!("registry/domain/search.rs"),
    ),
    (
        "ui_lab/registry/domain/session_view.rs",
        include_str!("registry/domain/session_view.rs"),
    ),
    (
        "ui_lab/registry/domain/settings.rs",
        include_str!("registry/domain/settings.rs"),
    ),
    (
        "ui_lab/registry/domain/shell_chrome.rs",
        include_str!("registry/domain/shell_chrome.rs"),
    ),
    (
        "ui_lab/registry/domain/socials.rs",
        include_str!("registry/domain/socials.rs"),
    ),
    (
        "ui_lab/registry/domain/voice.rs",
        include_str!("registry/domain/voice.rs"),
    ),
    (
        "ui_lab/registry/domain/webview.rs",
        include_str!("registry/domain/webview.rs"),
    ),
    (
        "ui_lab/registry/domain/window.rs",
        include_str!("registry/domain/window.rs"),
    ),
    (
        "ui_lab/registry/domain/workspace_host.rs",
        include_str!("registry/domain/workspace_host.rs"),
    ),
    (
        "ui_lab/registry/foundations.rs",
        include_str!("registry/foundations.rs"),
    ),
    (
        "ui_lab/registry/native.rs",
        include_str!("registry/native.rs"),
    ),
    (
        "ui_lab/registry/native/android.rs",
        include_str!("registry/native/android.rs"),
    ),
    ("ui_lab/shell.rs", include_str!("shell.rs")),
    ("ui_lab/story.rs", include_str!("story.rs")),
    ("ui_lab/tokens.rs", include_str!("tokens.rs")),
];

/// The body of `fn main`, so a name that also appears in a helper or a
/// comment above it cannot satisfy an ordering by accident.
fn main_body() -> &'static str {
    let start = MAIN
        .find("\nfn main() {")
        .expect("apps/koru/src/main.rs still has `fn main`");
    let body = &MAIN[start..];
    &body[..body.find("\n}\n").expect("`fn main` closes")]
}

fn position(needle: &str) -> usize {
    main_body()
        .find(needle)
        .unwrap_or_else(|| panic!("`fn main` no longer contains `{needle}`"))
}

/// The lines of `source` that are code: comments say what the Lab avoids, by
/// name, and must not be mistaken for doing it.
fn code_lines(source: &'static str) -> impl Iterator<Item = &'static str> {
    source
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
}

#[test]
fn the_lab_is_dispatched_before_every_piece_of_ordinary_startup() {
    let dispatch = position("ui_lab::run(");
    for later in [
        // The update reopen claim, a file under the channel root.
        "reopen_claim::hold(",
        // The personal file log and the Store diagnostic sink.
        "koru_log::init_file_and_stdout(",
        "set_diagnostic_sink(",
        // The taskbar hand-off to a running Koru, and the Jump List.
        "taskbar::prepare(",
        "taskbar::install_jump_list(",
        // The app-menu entry repair.
        "app_menu::repair(",
        // The Agent link, and the Store it is asked to start.
        "agent::AgentLink::connect(",
        ".ensure_store(",
        // Persisted search choices.
        "koru_search_ui::persist_choices_in(",
        // The fixture theme pin: the Lab draws in its own defaults.
        "themes::pin(",
        // The workspace host: settings, profile discovery, `App::new`.
        "koru_workspace_host::run(",
    ] {
        assert!(
            dispatch < position(later),
            "`--ui` must be dispatched before `{later}`"
        );
    }
}

/// The dispatch is on the parse's own result, and nothing after it can run:
/// the arm returns, and a rejected line exits 2 in the arm beside it.
#[test]
fn the_dispatch_is_the_parsers_answer_and_never_falls_through() {
    let body = main_body();
    let parse = position("koru_workspace_host::cli::parse(std::env::args())");
    let dispatch = position("ui_lab::run(");
    assert!(parse < dispatch);
    assert_eq!(
        body.matches("cli::parse(").count(),
        1,
        "`main` parses the command line once"
    );
    let arm = &body[parse..dispatch];
    assert!(
        arm.contains("Ok(koru_workspace_host::cli::Command::Ui(ui)) =>"),
        "the Lab is entered from the typed command, not from a scan of argv"
    );
    let after = &body[dispatch..];
    let returns = after.find("return;").expect("the UI arm returns");
    let rejected = after.find("Err(bad) =>").expect("a rejection has an arm");
    let exit = after
        .find("std::process::exit(2);")
        .expect("a rejection exits 2");
    assert!(returns < rejected && rejected < exit);
    assert!(exit < after.find("let run =").expect("the ordinary run follows"));
    // The typed command is the only way in: `main` never looks for the flag's
    // spelling itself.
    assert!(!body.contains("\"--ui\""));
}

/// What the Lab's sources may not name. Each is the entry to an effect the
/// Lab must not have, so a Lab that began to open a profile, a Store, a log
/// file, remembered geometry or a worker would have to spell one of these.
#[test]
fn the_lab_names_none_of_the_services_or_stores_it_must_not_start() {
    const FORBIDDEN: &[&str] = &[
        // The ordinary application and its startup.
        "koru_workspace_host::run",
        "koru_workspace_host::app",
        "koru_workspace_host::startup",
        "launch_settings_folder",
        "App::new",
        // Profile, Store, Agent and account.
        "koru_workspace::",
        "koru_store",
        "GlobalLink",
        "AgentLink",
        "ensure_store",
        "koru_account",
        "ProfileId",
        // Preferences, logs and anything else on disk.
        "koru_settings",
        "koru_log::",
        "std::fs",
        "std::io",
        "File::",
        "local_state_dir",
        "user_preferences_path",
        // Remembered window geometry.
        "window_geometry",
        "WindowRowWriter",
        "read_window_row",
        // Background work and timers.
        "std::thread",
        "thread::spawn",
        "tokio",
        "Warmup",
        "spelling",
        "maintenance_deadline",
        // The environment, the fixture theme, and command-line scans.
        "koru_env",
        "std::env",
        "KORU_FIXTURE",
        "themes::pin",
        "_from_args",
        // Network and telemetry.
        "reqwest",
        "koru_events",
        "telemetry",
    ];
    for (file, source) in LAB_SOURCES {
        for line in scanned_lines(file, source) {
            for forbidden in FORBIDDEN {
                assert!(
                    !line.contains(forbidden),
                    "{file} names `{forbidden}`: {line}"
                );
            }
        }
    }
}

/// **The registry's record files are data, and their data is paths.** A
/// record names the production symbol it describes as a string literal —
/// `"koru_settings_ui::SettingsSurface"` — and the inventory has to be able
/// to name every UI crate, including the ones the Lab itself must never
/// call. A literal is text the registry shows; it links nothing and runs
/// nothing. So in these files, and only these, the scans read each code
/// line with its string literals emptied: a `use`, a path expression or a
/// call of a forbidden crate is still refused, exactly as strictly as
/// anywhere else in the Lab, while a record may spell the path it records.
/// Every other Lab file — the shell, the story, the registry's own module
/// with the Button and the shell-facing functions — is scanned whole.
const RECORD_FILES: &[&str] = &[
    "ui_lab/registry/app.rs",
    "ui_lab/registry/app/companion_apps.rs",
    "ui_lab/registry/app/leaf.rs",
    "ui_lab/registry/app/mainview.rs",
    "ui_lab/registry/app/mini_app.rs",
    "ui_lab/registry/app/pages.rs",
    "ui_lab/registry/app/panes.rs",
    "ui_lab/registry/app/tasks_view.rs",
    "ui_lab/registry/compositions.rs",
    "ui_lab/registry/controls.rs",
    "ui_lab/registry/domain.rs",
    "ui_lab/registry/domain/agents.rs",
    "ui_lab/registry/domain/ai.rs",
    "ui_lab/registry/domain/app_shell.rs",
    "ui_lab/registry/domain/appearance.rs",
    "ui_lab/registry/domain/artifacts.rs",
    "ui_lab/registry/domain/canvas.rs",
    "ui_lab/registry/domain/chats.rs",
    "ui_lab/registry/domain/editor.rs",
    "ui_lab/registry/domain/markdown_view.rs",
    "ui_lab/registry/domain/runs.rs",
    "ui_lab/registry/domain/search.rs",
    "ui_lab/registry/domain/session_view.rs",
    "ui_lab/registry/domain/settings.rs",
    "ui_lab/registry/domain/shell_chrome.rs",
    "ui_lab/registry/domain/socials.rs",
    "ui_lab/registry/domain/voice.rs",
    "ui_lab/registry/domain/webview.rs",
    "ui_lab/registry/domain/window.rs",
    "ui_lab/registry/domain/workspace_host.rs",
    "ui_lab/registry/foundations.rs",
    "ui_lab/registry/native.rs",
    "ui_lab/registry/native/android.rs",
];

/// `line` with the contents of every `"…"` literal removed, quotes kept.
/// A record file holds no raw string, no character literal of a quote and
/// no literal that runs past its line — each would let text slip out of a
/// literal unseen — so meeting one fails rather than guessing.
fn outside_strings(file: &str, line: &str) -> String {
    let refuse = || panic!("{file}: a record file uses only plain string literals: {line}");
    let mut code = String::with_capacity(line.len());
    let mut inside = false;
    let mut escaped = false;
    let mut previous = ' ';
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if inside {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
                code.push(c);
            }
        } else {
            let starts_token = !(previous.is_alphanumeric() || previous == '_');
            // `r"…"` or `r#"…"#`: a raw literal, whose end this reading
            // would not find. A `'"'` character literal likewise.
            if (c == 'r' && starts_token && matches!(chars.peek(), Some('"' | '#')))
                || (c == '\'' && chars.peek() == Some(&'"'))
            {
                refuse();
            }
            if c == '"' {
                inside = true;
            }
            code.push(c);
        }
        previous = c;
    }
    assert!(
        !inside,
        "{file}: a record's string literal ends on its own line: {line}"
    );
    code
}

/// The code lines the scans read in `file`: whole, or with string literals
/// emptied for a record file.
fn scanned_lines(file: &'static str, source: &'static str) -> Vec<String> {
    let record = RECORD_FILES.contains(&file);
    code_lines(source)
        .map(|line| {
            if record {
                outside_strings(file, line)
            } else {
                line.to_owned()
            }
        })
        .collect()
}

/// The exemption above is narrow and does what it says: a record file's
/// literal may name a forbidden crate, its code may not, and every other
/// file is read with its literals intact.
#[test]
fn only_record_literals_may_spell_a_forbidden_path() {
    let record = "ui_lab/registry/domain/settings.rs";
    assert_eq!(
        outside_strings(record, r#"support("koru_settings_ui::SettingsSurface"),"#),
        r#"support(""),"#
    );
    assert_eq!(
        outside_strings(record, r#"plain("a \"quoted\" koru_settings", x)"#),
        r#"plain("", x)"#
    );
    assert!(
        outside_strings(record, "use koru_settings_ui::SettingsSurface;").contains("koru_settings")
    );
    assert!(
        outside_strings(record, r#"let _ = ("", koru_settings_ui::x());"#)
            .contains("koru_settings")
    );
    // Not a record file: read whole.
    assert!(!RECORD_FILES.contains(&"ui_lab/shell.rs"));
    assert!(!RECORD_FILES.contains(&"ui_lab/registry.rs"));
    // Every record file is a Lab source the scans read.
    for file in RECORD_FILES {
        assert!(
            LAB_SOURCES.iter().any(|(listed, _)| listed == file),
            "{file} is exempted but not scanned"
        );
    }
}

/// The Lab's only uses of the workspace host are the typed command and the
/// one size constant its window shares with `--window-size`.
#[test]
fn the_lab_takes_only_the_command_and_a_constant_from_the_workspace_host() {
    let mut uses: Vec<String> = LAB_SOURCES
        .iter()
        .flat_map(|(file, source)| scanned_lines(file, source))
        .filter(|line| line.contains("koru_workspace_host"))
        .collect();
    uses.sort_unstable();
    assert_eq!(
        uses,
        [
            "const MIN_SIZE: (f32, f32) = koru_workspace_host::settings::MIN_WINDOW_SIZE;",
            "use koru_workspace_host::cli::UiCommand;",
        ]
    );
}

/// Every source file under `ui_lab` is on the list the checks above read, so
/// a new module cannot be added outside them.
#[test]
fn every_lab_module_is_scanned() {
    let declared: Vec<&str> = LAB_SOURCES
        .iter()
        .flat_map(|(_, source)| code_lines(source))
        .filter_map(|line| {
            line.strip_prefix("pub(crate) mod ")
                .or_else(|| line.strip_prefix("mod "))
        })
        .filter_map(|rest| rest.strip_suffix(';'))
        .filter(|name| *name != "tests")
        .collect();
    for module in &declared {
        assert!(
            LAB_SOURCES
                .iter()
                .any(|(file, _)| file.ends_with(&format!("/{module}.rs"))),
            "`{module}` is a Lab module the isolation checks do not read"
        );
    }
    assert_eq!(declared.len() + 1, LAB_SOURCES.len());
}
