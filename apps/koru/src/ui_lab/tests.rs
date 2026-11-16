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
    ("ui_lab/shell.rs", include_str!("shell.rs")),
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
        for line in code_lines(source) {
            for forbidden in FORBIDDEN {
                assert!(
                    !line.contains(forbidden),
                    "{file} names `{forbidden}`: {line}"
                );
            }
        }
    }
}

/// The Lab's only uses of the workspace host are the typed command and the
/// one size constant its window shares with `--window-size`.
#[test]
fn the_lab_takes_only_the_command_and_a_constant_from_the_workspace_host() {
    let mut uses: Vec<&str> = LAB_SOURCES
        .iter()
        .flat_map(|(_, source)| code_lines(source))
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
