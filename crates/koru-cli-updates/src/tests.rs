// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::catalogue::KNOWN;
use crate::controller::{attention, update_argv};
use crate::latest::{GrokCheck, parse_grok_check, parse_npm_latest};
use crate::report::{Activity, Status, row};

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

fn status(id: &str, installed: &str, latest: Result<&str, &str>) -> Status {
    Status {
        cli: catalogue::known(id).unwrap(),
        installed: Some(v(installed)),
        latest: latest.map(v).map_err(str::to_owned),
    }
}

// ------------------------------------------------------------ catalogue

#[test]
fn every_known_cli_has_a_unique_id_a_brand_name_and_a_program() {
    for (index, cli) in KNOWN.iter().enumerate() {
        assert!(
            !KNOWN[..index].iter().any(|other| other.id == cli.id),
            "{} listed twice",
            cli.id
        );
        assert!(!cli.name().is_empty(), "{} has no name", cli.id);
        assert_ne!(cli.name(), cli.id, "{} has no brand name", cli.id);
        assert!(!cli.program.is_empty());
    }
}

#[test]
fn an_updater_is_the_cli_itself_or_npm_and_never_a_shell_string() {
    for cli in KNOWN {
        let (program, args) = update_argv(cli);
        assert!(program == cli.program || program == "npm", "{}", cli.id);
        for arg in &args {
            assert!(
                !arg.contains(['&', '|', ';', '>', '<', ' ']),
                "{}: {arg:?} would need a shell",
                cli.id
            );
        }
        if program == "npm" {
            assert_eq!(args[..2], ["install", "-g"]);
            assert!(args[2].ends_with("@latest"), "{}", cli.id);
        }
    }
}

// -------------------------------------------------------------- version

#[test]
fn the_version_is_found_in_every_cli_s_own_version_line() {
    for (printed, expected) in [
        ("2.1.312 (Claude Code)\n", "2.1.312"),
        ("codex-cli 0.163.0\n", "0.163.0"),
        ("0.59.0\n", "0.59.0"),
        ("grok 1.0.60 (01a8e1c591c0) [stable]\n", "1.0.60"),
        (
            "GitHub Copilot CLI 1.0.83.\nRun 'copilot update' to check for updates.\n",
            "1.0.83",
        ),
        ("2.3.3\n", "2.3.3"),
        ("v1.4.8", "1.4.8"),
        ("tool 0.1.0-alpha.2 (dev)", "0.1.0-alpha.2"),
    ] {
        assert_eq!(Version::first_in(printed), Some(v(expected)), "{printed:?}");
    }
}

#[test]
fn a_line_with_no_version_in_it_is_no_version() {
    for printed in [
        "",
        "Error: opencode-ai's postinstall script was not run.",
        "muse: unsupported platform: MINGW64_NT-10.0-26300 x86_64",
        "Usage: foo [options]",
    ] {
        assert_eq!(Version::first_in(printed), None, "{printed:?}");
    }
}

#[test]
fn versions_order_as_releases_do() {
    assert!(v("0.165.0") > v("0.163.0"));
    assert!(v("1.0.70") > v("1.0.60"));
    assert!(v("1.0.104") > v("1.0.83"), "numeric, not lexical");
    assert!(v("2.0.0") > v("2.0.0-beta.1"));
}

// --------------------------------------------------------------- latest

#[test]
fn the_registry_s_latest_document_names_the_version() {
    let body = r#"{"name":"@openai/codex","version":"0.165.0","dist":{"tarball":"x"}}"#;
    assert_eq!(parse_npm_latest(body), Ok(v("0.165.0")));
    assert!(parse_npm_latest(r#"{"name":"x"}"#).is_err());
    assert!(parse_npm_latest("<html>").is_err());
}

#[test]
fn grok_s_check_document_is_read_whole() {
    // Verbatim from `grok update --check --json`, 1.0.60, 2026-10-27.
    let body = r#"{"currentVersion":"1.0.60","latestVersion":"1.0.70","updateAvailable":true,"installer":"internal","channel":"stable","autoUpdate":true,"error":null}"#;
    assert_eq!(
        parse_grok_check(body),
        Ok(GrokCheck {
            current: Some(v("1.0.60")),
            latest: v("1.0.70"),
            update_available: true,
        })
    );
    assert!(
        parse_grok_check(r#"{"error":"offline"}"#)
            .unwrap_err()
            .contains("offline")
    );
}

// ---------------------------------------------------------------- rows

#[test]
fn a_newer_release_is_an_offer_and_the_same_or_older_is_not() {
    let newer = row(
        &status("codex", "0.163.0", Ok("0.165.0")),
        &Activity::Idle,
        None,
    );
    assert_eq!(newer.stage, CliStage::Available);
    assert_eq!(
        (newer.installed.as_str(), newer.latest.as_str()),
        ("0.163.0", "0.165.0")
    );
    assert_eq!(newer.name, "Codex");

    let same = row(
        &status("claude", "2.1.312", Ok("2.1.312")),
        &Activity::Idle,
        None,
    );
    assert_eq!(same.stage, CliStage::UpToDate);

    let ahead = row(
        &status("claude", "2.2.0", Ok("2.1.312")),
        &Activity::Idle,
        None,
    );
    assert_eq!(ahead.stage, CliStage::UpToDate, "a dev build is not stale");
}

#[test]
fn a_channel_that_could_not_be_read_is_not_an_offer_and_keeps_the_reason() {
    let unread = row(
        &status("codex", "0.163.0", Err("registry returned 503")),
        &Activity::Idle,
        None,
    );
    assert_eq!(unread.stage, CliStage::UpToDate);
    assert_eq!(unread.latest, "");
    assert_eq!(unread.detail.as_deref(), Some("registry returned 503"));
}

#[test]
fn a_dismissed_offer_stays_closed_until_a_newer_release() {
    let s = status("grok", "1.0.60", Ok("1.0.70"));
    assert_eq!(
        row(&s, &Activity::Idle, Some("1.0.70")).stage,
        CliStage::UpToDate
    );
    assert_eq!(
        row(&s, &Activity::Idle, Some("1.0.65")).stage,
        CliStage::Available
    );
}

#[test]
fn an_update_in_progress_or_finished_outranks_the_offer() {
    let s = status("grok", "1.0.60", Ok("1.0.70"));
    let updating = row(
        &s,
        &Activity::Updating {
            last_line: Some("Downloading 1.0.70".into()),
        },
        None,
    );
    assert_eq!(updating.stage, CliStage::Updating);
    assert_eq!(updating.message.as_deref(), Some("Downloading 1.0.70"));

    let updated = row(
        &s,
        &Activity::Updated {
            version: v("1.0.70"),
        },
        None,
    );
    assert_eq!(updated.stage, CliStage::Updated);
    assert_eq!(
        updated.installed, "1.0.70",
        "the card names what is installed now"
    );

    let failed = row(
        &s,
        &Activity::Failed {
            message: Some("EACCES".into()),
            detail: "exit 1\nEACCES".into(),
        },
        None,
    );
    assert_eq!(failed.stage, CliStage::Failed);
    assert_eq!(failed.message.as_deref(), Some("EACCES"));
    assert_eq!(failed.detail.as_deref(), Some("exit 1\nEACCES"));
}

// ----------------------------------------------------------- controller

#[test]
fn before_the_first_check_the_report_says_nothing_was_checked() {
    let controller = Controller::detached();
    assert_eq!(controller.report(), Report::default());
    assert_eq!(controller.update("codex"), Err(ChoiceError::NoSuchCli));
    assert_eq!(controller.dismiss("codex"), Err(ChoiceError::NoSuchCli));
}

#[test]
fn a_check_lists_the_installed_clis_in_catalogue_order_and_attention_skips_the_current() {
    let controller = Controller::detached();
    controller.inject_check(vec![
        status("claude", "2.1.312", Ok("2.1.312")),
        status("codex", "0.163.0", Ok("0.165.0")),
        status("grok", "1.0.60", Ok("1.0.70")),
    ]);
    let report = controller.report();
    assert!(report.checked);
    assert_eq!(
        report
            .clis
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["claude", "codex", "grok"]
    );
    assert_eq!(
        attention(&report)
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["codex", "grok"]
    );
}

#[test]
fn dismissing_an_offer_remembers_its_version_and_a_newer_check_reopens_it() {
    let controller = Controller::detached();
    controller.inject_check(vec![status("codex", "0.163.0", Ok("0.165.0"))]);
    assert_eq!(controller.dismiss("codex"), Ok(()));
    assert_eq!(
        controller.dismissed().get("codex").map(String::as_str),
        Some("0.165.0")
    );
    assert_eq!(controller.report().clis[0].stage, CliStage::UpToDate);

    controller.inject_check(vec![status("codex", "0.163.0", Ok("0.166.0"))]);
    assert_eq!(controller.report().clis[0].stage, CliStage::Available);
    assert_eq!(controller.dismiss("unknown"), Err(ChoiceError::NoSuchCli));
}

#[test]
fn an_offer_with_nothing_to_offer_cannot_be_dismissed() {
    let controller = Controller::detached();
    controller.inject_check(vec![status("codex", "0.163.0", Err("offline"))]);
    assert_eq!(controller.dismiss("codex"), Err(ChoiceError::WrongStage));
}

#[test]
fn progress_then_a_result_move_the_row_and_a_result_moves_the_installed_version() {
    let controller = Controller::detached();
    controller.inject_check(vec![status("grok", "1.0.60", Ok("1.0.70"))]);
    // The real `update` spawns the CLI; the events it would send are injected.
    controller.inject_progress("grok", "");
    assert_eq!(
        controller.report().clis[0].message,
        None,
        "a blank line is not progress"
    );

    controller.inject_done(
        "grok",
        Activity::Updated {
            version: v("1.0.70"),
        },
    );
    let row = &controller.report().clis[0];
    assert_eq!(row.stage, CliStage::Updated);
    assert_eq!(row.installed, "1.0.70");

    // Putting the finished card away leaves an up-to-date row, not an offer:
    // the installed fact moved with the install.
    assert_eq!(controller.dismiss("grok"), Ok(()));
    assert_eq!(controller.report().clis[0].stage, CliStage::UpToDate);
    assert!(
        controller.dismissed().is_empty(),
        "a finished run records no refusal"
    );
}

#[test]
fn a_failed_run_is_put_away_by_dismissing_it() {
    let controller = Controller::detached();
    controller.inject_check(vec![status("grok", "1.0.60", Ok("1.0.70"))]);
    controller.inject_done(
        "grok",
        Activity::Failed {
            message: Some("no network".into()),
            detail: "exit 1".into(),
        },
    );
    assert_eq!(controller.report().clis[0].stage, CliStage::Failed);
    assert_eq!(controller.dismiss("grok"), Ok(()));
    assert_eq!(
        controller.report().clis[0].stage,
        CliStage::Available,
        "the offer is still there"
    );
}

#[test]
fn a_cli_that_disappears_between_checks_takes_its_run_with_it() {
    let controller = Controller::detached();
    controller.inject_check(vec![status("grok", "1.0.60", Ok("1.0.70"))]);
    controller.inject_done(
        "grok",
        Activity::Updated {
            version: v("1.0.70"),
        },
    );
    controller.inject_check(vec![status("codex", "0.163.0", Ok("0.165.0"))]);
    let report = controller.report();
    assert_eq!(report.clis.len(), 1);
    assert_eq!(report.clis[0].id, "codex");
}

// ---------------------------------------------------------------- state

#[test]
fn the_dismissal_file_round_trips_and_a_missing_one_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent").join("cli-updates.json");
    assert_eq!(state::State::load(&path), state::State::default());
    let mut state = state::State::default();
    state.dismissed.insert("codex".into(), "0.165.0".into());
    state.save(&path).unwrap();
    assert_eq!(state::State::load(&path), state);
    assert!(
        !path.with_extension("json.part").exists(),
        "renamed into place"
    );
}

// ------------------------------------------------------------------ run

#[test]
fn a_run_keeps_the_tail_and_the_last_line_that_said_something() {
    let finished = run::Finished {
        success: false,
        exit_code: Some(1),
        timed_out: false,
        tail: vec!["npm ERR! code EACCES".into(), "".into(), "   ".into()],
    };
    assert_eq!(finished.last_line(), Some("npm ERR! code EACCES"));
    assert_eq!(finished.detail(), "npm ERR! code EACCES\n\n   ");
}
