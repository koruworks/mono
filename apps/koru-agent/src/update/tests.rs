// SPDX-License-Identifier: MPL-2.0

//! What the person is told, what they are allowed to do about it, and whether
//! the stack is down yet — none of which needs a socket, a process or a window.

use super::*;
use koru_update::{
    PackageFormat, ReleaseChannel, ReleaseNotes, UpdateFailure, UpdateManifest, UpdatePackage,
};

fn manifest() -> UpdateManifest {
    UpdateManifest {
        schema_version: 1,
        channel: ReleaseChannel::Nightly,
        platform: "windows".into(),
        architecture: "x86_64".into(),
        version: "0.0.4-nightly.20260815+abc1234".into(),
        build_number: 16,
        published_at: "2026-08-15T00:00:00Z".into(),
        commit: Some("abc1234".into()),
        source: None,
        minimum_updater_version: Some(1),
        minimum_supported_build: None,
        required: false,
        release_notes: ReleaseNotes {
            title: "Everyday editing, tidied up".into(),
            summary: vec!["Documents open faster.".into(), "Fewer stalls.".into()],
            url: Some("https://updates.koru.do/releases/0.0.4/index.html".into()),
        },
        package: UpdatePackage {
            url: "https://updates.koru.do/releases/0.0.4/koru-windows-x86_64.zip".into(),
            file_name: "koru-windows-x86_64.zip".into(),
            size: 48 * 1024 * 1024,
            sha256: "a".repeat(64),
            signature: "untrusted comment: signature\nRWQ...".into(),
            format: PackageFormat::Zip,
        },
    }
}

fn report(status: &UpdateStatus) -> UpdateReport {
    report_for(status, true, true, "nightly")
}

/// **What's new is shown before the person consents, not after**, and the
/// *report* carries it at both stages so no surface has to go without.
///
/// **That is not the same as showing it twice**, and the difference was a bug
/// report. What is offered here is the data; whether a given surface draws it is
/// that surface's decision, and both now say it once — see [`dialog_copy`] and
/// `banner_copy`'s `Ready` arm. Keeping the notes on the `Ready` report is still
/// right: a window that connects *after* the download finished has never seen
/// them, and it is the only thing that could tell it.
#[test]
fn both_consent_steps_carry_the_release_notes() {
    let ready = UpdateStatus::ReadyToInstall {
        manifest: manifest(),
        package: std::path::PathBuf::from("koru.zip"),
    };
    for status in [UpdateStatus::Available(manifest()), ready] {
        let report = report(&status);
        assert_eq!(report.title, "Everyday editing, tidied up");
        assert_eq!(report.summary.len(), 2);
        assert_eq!(report.build_number, 16);
        assert!(report.notes_url.is_some(), "View all changes has a target");
    }
}

/// **The notes are read once, at the question they are an answer to.**
///
/// With no window open the two consents are two `rfd` dialogs, and the second
/// used to repeat the whole of what the first had just said — asking somebody to
/// re-read the release notes in order to decide whether to be interrupted *now*,
/// which is the only thing left to decide by then. Reported from a real update.
///
/// Asserted here because nothing that draws a modal can be: `dialog_copy` was
/// split out of `offer_in_a_dialog` for exactly this, on the same
/// decisions-are-pure line the rest of this module follows.
#[test]
fn the_install_question_does_not_repeat_the_release_notes() {
    let (title, download) = dialog_copy(&report(&UpdateStatus::Available(manifest())));
    assert!(title.contains("available"), "{title}");
    assert!(
        download.contains("Everyday editing, tidied up") && download.contains("Fewer stalls."),
        "the first question carries what changed: {download}"
    );
    assert!(download.contains("Download it now?"), "{download}");

    let (title, install) = dialog_copy(&report(&UpdateStatus::ReadyToInstall {
        manifest: manifest(),
        package: std::path::PathBuf::from("koru.zip"),
    }));
    assert!(title.contains("ready"), "{title}");
    assert!(
        !install.contains("Everyday editing, tidied up") && !install.contains("Fewer stalls."),
        "the second question says it again: {install}"
    );
    assert!(install.contains("Install it now?"), "{install}");
    assert!(
        install.contains("close and restart"),
        "it still says what will happen: {install}"
    );
    // Both name the build, which is the one thing worth repeating — it is what
    // distinguishes this dialog from the one for the release after it.
    assert!(title.contains("0.0.4-nightly.20260815+abc1234"), "{title}");
}

/// The two-step consent, as an assertion rather than an intention: a shell that
/// sends `Install` on an offer it has not downloaded is refused.
#[test]
fn consent_cannot_skip_a_step() {
    assert!(allows(UpdateStage::Available, UpdateChoice::Download));
    assert!(
        !allows(
            UpdateStage::Available,
            UpdateChoice::Install {
                session_generation: 1
            }
        ),
        "nothing has been downloaded yet"
    );

    assert!(allows(
        UpdateStage::Ready,
        UpdateChoice::Install {
            session_generation: 1
        }
    ));
    assert!(
        !allows(UpdateStage::Ready, UpdateChoice::Download),
        "it is already here"
    );

    assert!(!allows(UpdateStage::Idle, UpdateChoice::Download));
    assert!(!allows(
        UpdateStage::Idle,
        UpdateChoice::Install {
            session_generation: 1
        }
    ));
    assert!(!allows(
        UpdateStage::Downloading,
        UpdateChoice::Install {
            session_generation: 1
        }
    ));
}

/// Two answers are about the agent rather than about an offer, so they are
/// always available — including from a window that has never seen a banner.
#[test]
fn checking_and_the_automatic_setting_need_no_offer() {
    for stage in [
        UpdateStage::Idle,
        UpdateStage::UpToDate,
        UpdateStage::Failed,
        UpdateStage::Downloading,
    ] {
        assert!(allows(stage, UpdateChoice::CheckNow));
        assert!(allows(stage, UpdateChoice::SetAutomatic { on: false }));
    }
}

#[test]
fn cancel_retry_and_dismiss_name_the_state_they_act_on() {
    assert!(allows(UpdateStage::Downloading, UpdateChoice::Cancel));
    assert!(!allows(UpdateStage::Ready, UpdateChoice::Cancel));
    assert!(allows(UpdateStage::Failed, UpdateChoice::Retry));
    assert!(!allows(UpdateStage::Idle, UpdateChoice::Retry));
    assert!(allows(
        UpdateStage::InstalledNewer,
        UpdateChoice::DismissNewer
    ));
    assert!(!allows(UpdateStage::Available, UpdateChoice::DismissNewer));
}

/// Every `UpdateStatus` becomes a stage, and the ones with no build to name
/// leave the offer fields empty rather than carrying a stale one.
#[test]
fn a_report_with_nothing_on_offer_names_no_build() {
    for (status, expected) in [
        (UpdateStatus::Idle, UpdateStage::Idle),
        (UpdateStatus::Checking, UpdateStage::Checking),
        (UpdateStatus::UpToDate, UpdateStage::UpToDate),
        (UpdateStatus::Verifying, UpdateStage::Verifying),
        (UpdateStatus::Installing, UpdateStage::Installing),
    ] {
        let report = report(&status);
        assert_eq!(report.stage, expected);
        assert!(report.version.is_empty());
        assert_eq!(report.build_number, 0);
        assert!(report.title.is_empty());
        assert!(report.download_url.is_none());
    }
}

/// The two stages that name a build without offering it. Neither is a bug, and
/// both would look like one to somebody reading the field alone.
#[test]
fn a_build_number_without_an_offer_is_deliberate() {
    let newer = report(&UpdateStatus::InstalledBuildNewer { available: 12 });
    assert_eq!(newer.stage, UpdateStage::InstalledNewer);
    assert_eq!(newer.build_number, 12, "what the channel has");
    assert!(
        newer.download_url.is_none(),
        "there is nothing here to fetch"
    );

    let deferred = report(&UpdateStatus::Deferred(16));
    assert_eq!(deferred.stage, UpdateStage::Deferred);
    assert_eq!(deferred.build_number, 16, "what was put off");
}

#[test]
fn progress_and_failure_reach_the_banner() {
    let downloading = report(&UpdateStatus::Downloading {
        received: 1_024,
        total: 4_096,
    });
    assert_eq!((downloading.received, downloading.total), (1_024, 4_096));

    let failed = report(&UpdateStatus::Failed(UpdateFailure::new(
        "The update could not be downloaded.",
        "connection reset",
        true,
    )));
    assert_eq!(failed.stage, UpdateStage::Failed);
    assert_eq!(
        failed.message.as_deref(),
        Some("The update could not be downloaded.")
    );
    assert_eq!(failed.detail.as_deref(), Some("connection reset"));
}

#[test]
fn an_offer_keeps_its_metadata_through_progress_deferral_and_failure() {
    let offer = manifest();
    for status in [
        UpdateStatus::Downloading {
            received: 1_024,
            total: 4_096,
        },
        UpdateStatus::Verifying,
        UpdateStatus::Installing,
        UpdateStatus::Deferred(16),
        UpdateStatus::Failed(UpdateFailure::new(
            "Download failed.",
            "connection reset",
            true,
        )),
    ] {
        let mut actual = report_for(&status, false, false, "nightly");
        actual.active_session_count = 3;
        actual.session_generation = 42;
        let expected = UpdateReport {
            version: "0.0.4-nightly.20260815+abc1234".into(),
            build_number: 16,
            title: "Everyday editing, tidied up".into(),
            summary: vec!["Documents open faster.".into(), "Fewer stalls.".into()],
            notes_url: Some("https://updates.koru.do/releases/0.0.4/index.html".into()),
            download_url: Some(
                "https://updates.koru.do/releases/0.0.4/koru-windows-x86_64.zip".into(),
            ),
            ..actual.clone()
        };

        retain_offer(&mut actual, Some(&offer));

        assert_eq!(
            actual, expected,
            "{status:?}: preserve progress, failure and consent/session facts while retaining the offer"
        );
    }
}

#[test]
fn cached_offer_metadata_does_not_leak_into_states_without_an_offer() {
    let cached = manifest();
    for status in [
        UpdateStatus::Idle,
        UpdateStatus::Checking,
        UpdateStatus::UpToDate,
        UpdateStatus::InstalledBuildNewer { available: 12 },
    ] {
        let mut actual = report(&status);
        let before = actual.clone();

        retain_offer(&mut actual, Some(&cached));

        assert_eq!(
            actual, before,
            "a stale manifest must not change {status:?}"
        );
    }
}

#[test]
fn a_deferred_report_does_not_name_a_different_cached_release() {
    let cached = manifest();
    for deferred_build in [15, 17] {
        let mut actual = report(&UpdateStatus::Deferred(deferred_build));
        let before = actual.clone();

        retain_offer(&mut actual, Some(&cached));

        assert_eq!(
            actual, before,
            "keep the deferred build without borrowed notes"
        );
    }
}

#[test]
fn a_missing_offer_leaves_progress_and_failure_reports_unchanged() {
    for status in [
        UpdateStatus::Downloading {
            received: 1,
            total: 2,
        },
        UpdateStatus::Verifying,
        UpdateStatus::Installing,
        UpdateStatus::Deferred(16),
        UpdateStatus::Failed(UpdateFailure::new("Check failed.", "no manifest", true)),
    ] {
        let mut actual = report(&status);
        let before = actual.clone();

        retain_offer(&mut actual, None);

        assert_eq!(actual, before, "no offer was received for {status:?}");
    }
}

#[test]
fn a_deferred_offer_can_be_downloaded_but_cannot_skip_install_consent() {
    assert!(allows(UpdateStage::Deferred, UpdateChoice::Download));
    assert!(!allows(
        UpdateStage::Deferred,
        UpdateChoice::Install {
            session_generation: 42,
        }
    ));
}

/// The app is *told* whether the swap is possible rather than probing for it —
/// so this has to survive the trip, including when the answer is no.
#[test]
fn whether_the_agent_can_install_is_carried_not_inferred() {
    let status = UpdateStatus::Available(manifest());
    assert!(report_for(&status, true, true, "nightly").can_install);

    let read_only = report_for(&status, false, true, "stable");
    assert!(!read_only.can_install);
    assert_eq!(read_only.channel, "stable");
    assert!(
        read_only.download_url.is_some(),
        "when the agent cannot install, the person needs somewhere to go"
    );
}

// ---------------------------------------------------------------- the handoff

fn now() -> Instant {
    Instant::now()
}

/// An agent alone in the notification area has nothing to wait for.
#[test]
fn with_no_windows_open_the_handoff_goes_straight_to_sessions() {
    assert_eq!(Handoff::begin(0, now()), Handoff::StoppingSessions);
}

#[test]
fn the_handoff_advances_as_windows_close() {
    let mut handoff = Handoff::begin(2, now());
    assert!(handoff.in_progress());
    handoff.shell_gone(1, now());
    assert!(matches!(
        handoff,
        Handoff::AskingShells { remaining: 1, .. }
    ));
    handoff.shell_gone(0, now());
    assert_eq!(handoff, Handoff::StoppingSessions);
}

/// **The wait is per window, not for the whole pass.** Every window puts up its
/// own blocking prompt, and a person who answers three of them slowly has not
/// abandoned the update — they have been doing exactly what was asked. With one
/// shared budget the fourth prompt was answered into an install that had
/// already given up.
#[test]
fn each_window_that_closes_buys_the_next_one_a_full_wait() {
    let start = now();
    let mut handoff = Handoff::begin(2, start);
    // Nearly out of time, and then one window goes.
    let answered = start + WAIT_FOR_SHELLS - Duration::from_secs(1);
    handoff.shell_gone(1, answered);
    // The moment the old single budget would have expired is now unremarkable.
    handoff.expire(start + WAIT_FOR_SHELLS);
    assert!(
        handoff.in_progress(),
        "answering one prompt must not be punished by the time it took"
    );
    handoff.expire(answered + WAIT_FOR_SHELLS);
    assert!(
        matches!(handoff, Handoff::Aborted(_)),
        "but silence still ends it"
    );
}

/// **Unsaved work always wins.** A window that says no stops the update; it is
/// not closed anyway, and the reason reaches the banner rather than being
/// swallowed.
#[test]
fn a_window_with_unsaved_work_stops_the_update() {
    let mut handoff = Handoff::begin(1, now());
    handoff.refused("a Koru window has unsaved work");
    match &handoff {
        Handoff::Aborted(why) => assert!(why.contains("unsaved work")),
        other => panic!("expected an abort, got {other:?}"),
    }
    assert!(
        !handoff.in_progress(),
        "an abort releases the handoff so it can be tried again"
    );
}

/// A window that never answers must not leave the update half-begun forever.
#[test]
fn a_window_that_never_closes_expires() {
    let start = now();
    let mut handoff = Handoff::begin(1, start);
    handoff.expire(start + WAIT_FOR_SHELLS - Duration::from_secs(1));
    assert!(handoff.in_progress(), "still within the window");
    handoff.expire(start + WAIT_FOR_SHELLS);
    assert!(matches!(handoff, Handoff::Aborted(_)));
}

/// The loop arms its wake-up from this, so only the waiting state has one.
#[test]
fn only_waiting_on_windows_has_a_deadline() {
    assert!(Handoff::begin(1, now()).deadline().is_some());
    assert!(Handoff::Idle.deadline().is_none());
    assert!(Handoff::StoppingSessions.deadline().is_none());
    assert!(Handoff::StoppingStore.deadline().is_none());
    assert!(Handoff::Aborted("no".into()).deadline().is_none());
}

/// An abort is not sticky: it is a state the next `Install` replaces, or the
/// person could never retry after saving their work.
#[test]
fn an_abort_does_not_block_the_next_attempt() {
    let aborted = Handoff::Aborted("unsaved work".into());
    assert!(!aborted.in_progress());
    assert_eq!(Handoff::begin(0, now()), Handoff::StoppingSessions);
}

#[test]
fn nothing_open_is_nothing_to_reopen() {
    assert!(Reopen::from_shells(&[]).is_empty());
}

#[test]
fn abort_recovery_keeps_only_windows_that_actually_closed() {
    let profile = koru_store_core::ProfileId::new();
    let still_open = koru_store_core::WorkspaceId::new();
    let closed = koru_store_core::WorkspaceId::new();
    let shell = Shell {
        connection: crate::registry::ConnectionId(1),
        pid: Some(10),
        profile,
        workspace: still_open,
        profile_name: "Profile".into(),
        workspace_name: "Still open".into(),
        connected_seq: 1,
        active_seq: 1,
        pinned: Vec::new(),
    };
    let mut reopen = Reopen {
        windows: vec![
            (profile.to_storage(), still_open.to_storage()),
            (profile.to_storage(), closed.to_storage()),
        ],
        ..Reopen::default()
    };

    reopen.retain_closed(&[&shell]);

    assert_eq!(
        reopen.windows,
        vec![(profile.to_storage(), closed.to_storage())]
    );
}

/// **The list has to outlive the process that wrote it.** The agent that closes
/// the windows exits so its own executable can be replaced; what reopens them is
/// a different run of a different build, so this goes through a file or the
/// windows simply never come back.
#[test]
fn the_windows_to_reopen_survive_the_process_that_recorded_them() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());

    let recorded = Reopen {
        windows: vec![
            ("profile-a".into(), "workspace-1".into()),
            ("profile-a".into(), "workspace-2".into()),
        ],
        ..Reopen::default()
    };
    recorded.save(&paths).unwrap();

    let read_back = Reopen::load(&paths);
    assert_eq!(read_back, recorded);

    // Readiness alone does not spend the record. Each successful launch removes
    // its own entry; a failure must survive for the next Agent.
    assert!(
        !Reopen::load(&paths).is_empty(),
        "reading readiness state consumed it before a launch succeeded"
    );
    Reopen::default().save(&paths).unwrap();
    assert!(Reopen::load(&paths).is_empty());
}

#[test]
fn a_window_replay_reuses_one_durable_v4_claim_until_hello() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let window = ("profile-a".to_string(), "workspace-1".to_string());
    let mut reopen = Reopen {
        windows: vec![window.clone()],
        ..Reopen::default()
    };

    let first = reopen.claim_window(&window);
    assert_eq!(uuid::Uuid::parse_str(&first).unwrap().get_version_num(), 4);
    reopen.save(&paths).unwrap();

    let mut after_crash = Reopen::load(&paths);
    assert_eq!(after_crash.claim_window(&window), first);
    assert!(!after_crash.acknowledge_claim(
        &window.0,
        &window.1,
        &uuid::Uuid::new_v4().to_string()
    ));
    assert!(!after_crash.is_empty());
    assert!(after_crash.acknowledge_claim(&window.0, &window.1, &first));
    after_crash.save(&paths).unwrap();
    assert!(Reopen::load(&paths).is_empty());
}

#[test]
fn an_invalid_or_orphaned_reopen_claim_is_never_replayed() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let path = Reopen::file(&paths);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        br#"{
          "windows": [["profile-a", "workspace-1"]],
          "claims": [
            {"profile":"profile-a","workspace":"workspace-1","token":"not-a-uuid"},
            {"profile":"profile-b","workspace":"workspace-2","token":"2f26d32a-46a7-4cdb-bd47-e5507cb19779"}
          ],
          "ai_chat": false
        }"#,
    )
    .unwrap();

    let window = ("profile-a".to_string(), "workspace-1".to_string());
    let mut reopen = Reopen::load(&paths);
    let replacement = reopen.claim_window(&window);
    assert_ne!(replacement, "not-a-uuid");
    assert_eq!(
        uuid::Uuid::parse_str(&replacement)
            .unwrap()
            .get_version_num(),
        4
    );
    assert_eq!(reopen.claims.len(), 1);
}

#[test]
fn an_aborted_handoff_clears_the_reopen_record() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    Reopen {
        windows: vec![("profile-a".into(), "workspace-1".into())],
        ..Reopen::default()
    }
    .save(&paths)
    .unwrap();
    Reopen::clear(&paths);
    assert!(Reopen::take(&paths).is_empty());
}

/// A pre-W5 build could leave a Koru Terminal reopen receipt behind. There is
/// no koru-terminal.exe to reopen any more, so a load consumes the stray file
/// and reports nothing owed.
#[test]
fn a_legacy_terminal_receipt_is_consumed_but_never_reopened() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let receipt = paths.agent_dir().join("reopen-terminal-after-update");
    std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    std::fs::write(&receipt, b"41").unwrap();
    assert!(Reopen::has_record(&paths) == false);
    assert!(Reopen::load(&paths).is_empty());
    assert!(!receipt.exists(), "the stray receipt was consumed");
}

/// Nothing to say, nothing written — an ordinary quit must not leave a file
/// that reopens windows on the next launch.
#[test]
fn an_empty_list_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    Reopen::default().save(&paths).unwrap();
    assert!(Reopen::load(&paths).is_empty());
}

/// **The retired Koru AI window is not reopened.** An older build recorded
/// whether it was open as `ai_chat`; the record still reads, the flag is
/// read past, and a record that owed only the window owes nothing.
#[test]
fn an_older_records_ai_chat_flag_is_read_past() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let path = Reopen::file(&paths);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, br#"{"windows": [], "ai_chat": true}"#).unwrap();
    assert!(Reopen::load(&paths).is_empty());
    std::fs::write(
        &path,
        br#"{"windows": [["profile-a", "workspace-1"]], "ai_chat": true}"#,
    )
    .unwrap();
    assert_eq!(
        Reopen::load(&paths).windows,
        vec![("profile-a".to_string(), "workspace-1".to_string())]
    );
}

#[test]
fn rewriting_reopen_state_is_atomic_and_leaves_the_latest_record() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let first = Reopen {
        windows: vec![("profile-a".into(), "workspace-1".into())],
        ..Reopen::default()
    };
    let second = Reopen {
        windows: vec![("profile-b".into(), "workspace-2".into())],
        ..Reopen::default()
    };

    first.save(&paths).unwrap();
    second.save(&paths).unwrap();

    assert_eq!(Reopen::load(&paths), second);
    let leftovers = std::fs::read_dir(paths.agent_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(".reopen-") && name.ends_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "temporary reopen files: {leftovers:?}"
    );
}

#[test]
fn reopen_persistence_reports_an_unwritable_parent() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    std::fs::write(paths.agent_dir(), b"not a directory").unwrap();
    let recorded = Reopen {
        windows: vec![("profile-a".into(), "workspace-1".into())],
        ..Reopen::default()
    };

    assert!(recorded.save(&paths).is_err());
}

#[test]
fn the_legacy_bare_window_vector_remains_readable() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let path = Reopen::file(&paths);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        br#"[["profile-a","workspace-1"],["profile-b","workspace-2"]]"#,
    )
    .unwrap();
    assert_eq!(
        Reopen::load(&paths),
        Reopen {
            windows: vec![
                ("profile-a".into(), "workspace-1".into()),
                ("profile-b".into(), "workspace-2".into()),
            ],
            ..Reopen::default()
        }
    );
}

// -------------------------------------------------------------- what's new

/// The far side of an install has to reach somebody, and the somebody may not
/// exist yet.
///
/// A person can consent to an install from the tray with no window open, let the
/// agent restart, quit it, shut the machine down, and open Koru on Thursday.
/// Every one of those loses a fact held in memory.
#[test]
fn what_was_installed_survives_the_process_that_installed_it() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());

    WhatsNew::owed(&paths, "0.0.4-nightly.20260816+3b71911");
    assert_eq!(
        WhatsNew::peek(&paths).version.as_deref(),
        Some("0.0.4-nightly.20260816+3b71911")
    );
}

/// **The one place this deliberately differs from [`Reopen::take`].**
///
/// Reading is not delivering: there is a socket between the two, and a window
/// can connect and die inside the same turn. A read-and-delete would spend the
/// only notice on it — the file gone, nothing drawn, and the person never told
/// about an update they agreed to. Somebody will propose collapsing `peek` and
/// `delivered` into one function for symmetry with `Reopen`; this is the answer.
#[test]
fn the_notice_survives_a_window_that_never_saw_it() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());

    WhatsNew::owed(&paths, "0.0.4");
    assert!(WhatsNew::peek(&paths).version.is_some());
    assert!(
        WhatsNew::peek(&paths).version.is_some(),
        "reading it is not delivering it"
    );

    WhatsNew::delivered(&paths);
    assert!(
        WhatsNew::peek(&paths).version.is_none(),
        "once a window has been told, it is never told again"
    );
}

/// An ordinary launch is not an install, and must not open a tab.
#[test]
fn a_launch_that_followed_no_install_owes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    assert!(WhatsNew::peek(&paths).version.is_none());
    // Clearing one that was never owed is not an error either — the delivery
    // path does it unconditionally on success.
    WhatsNew::delivered(&paths);
}

/// A marker nobody can read is a missing notice, never a refusal to start.
#[test]
fn an_unreadable_marker_is_ignored_rather_than_fatal() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    let path = paths.agent_dir().join("whats-new.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{ this is not a version }").unwrap();

    assert!(WhatsNew::peek(&paths).version.is_none());
}

/// A quote or a backslash in a release title must stay inside the AppleScript
/// string — an unescaped `"` would end the literal and hand the rest of the
/// manifest's text to `osascript` as script.
#[test]
fn the_mac_dialog_script_keeps_manifest_text_inside_its_literals() {
    let script = display_dialog_script(r#"Koru — "Quoted" \ slash"#, "Line one\nDownload it now?");
    assert!(script.starts_with("display dialog \"Line one\nDownload it now?\" with title "));
    assert!(
        script.contains(r#""Koru — \"Quoted\" \\ slash""#),
        "{script}"
    );
    assert!(script.ends_with(r#"buttons {"Later", "Yes"} default button "Yes" with icon note"#));
    // Exactly the four literals the template writes, plus the two in the title.
    let unescaped = script.matches('"').count() - script.matches("\\\"").count();
    assert_eq!(unescaped, 2 + 2 + 6, "{script}");
}

#[test]
fn the_quit_dialog_script_quotes_copy_and_localizes_both_choices() {
    let script = quit_dialog_script(
        r#"Quit "Koru""#,
        "One session\\path is active.",
        koru_i18n::Lang::En,
    );
    assert!(script.contains(r#"with title "Quit \"Koru\"""#), "{script}");
    assert!(
        script.contains(r#""One session\\path is active.""#),
        "{script}"
    );
    assert!(script.contains(r#"buttons {"Cancel", "Quit"}"#), "{script}");
    assert!(script.contains(r#"default button "Quit" cancel button "Cancel""#));
}

/// The script parses: `osascript` compiles it without displaying anything.
#[cfg(target_os = "macos")]
#[test]
fn the_mac_dialog_script_compiles() {
    let script = display_dialog_script(
        "A new Koru is ready — 1.2.3",
        "Install it now?\n\"Yes\" restarts.",
    );
    let output = std::process::Command::new("/usr/bin/osacompile")
        .args([
            "-o",
            &std::env::temp_dir()
                .join("koru-offer-test.scpt")
                .display()
                .to_string(),
            "-e",
            &script,
        ])
        .output()
        .expect("osacompile runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The bridge kill names only this installation's image, and it actually
/// kills. Asserted against real processes for the same reason
/// `a_running_executable_is_not_free` is: the whole point is a claim about
/// what Windows does, and matching by *name* instead of image path is the
/// regression this would silently allow — a dev tree's bridge dying because a
/// release installation updated itself.
#[cfg(windows)]
#[test]
fn the_bridge_kill_names_only_this_installations_image() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let this_install = dir.path().join("install");
    let other_install = dir.path().join("elsewhere");
    std::fs::create_dir_all(&this_install).unwrap();
    std::fs::create_dir_all(&other_install).unwrap();
    // Any real executable that waits will do, under the name the kill
    // matches. `ping -n` rather than `timeout /t`, because timeout.exe
    // refuses a redirected stdin and exits before anything can be asserted
    // about it; ping never reads stdin at all.
    let system =
        std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or("C:/Windows".into()));
    let source = system.join("System32/ping.exe");
    let ours = this_install.join(koru_update::MCP_EXE);
    let theirs = other_install.join(koru_update::MCP_EXE);
    std::fs::copy(&source, &ours).expect("copy a real exe");
    std::fs::copy(&source, &theirs).expect("copy a real exe");

    let spawn = |exe: &std::path::Path| {
        std::process::Command::new(exe)
            .args(["-n", "31", "127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the copy")
    };
    let mut our_bridge = spawn(&ours);
    let mut their_bridge = spawn(&theirs);

    // The image-path query fails with ERROR_GEN_FAILURE while the loader is
    // still mapping a just-spawned process, so the kill is polled rather
    // than called once — the same settling window the updater's write-open
    // test gives the loader. A real bridge has been running for minutes.
    let killed = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut killed = Vec::new();
        loop {
            killed.extend(kill_mcp_bridges(&this_install));
            if killed.contains(&our_bridge.id()) || Instant::now() >= deadline {
                break killed;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert!(
        killed.contains(&our_bridge.id()),
        "the installation's own bridge is killed: {killed:?}"
    );
    assert!(
        !killed.contains(&their_bridge.id()),
        "a same-named bridge elsewhere is not this installation's: {killed:?}"
    );

    // TerminateProcess is asynchronous only in the narrowest sense; give it a
    // bounded moment rather than asserting on a race.
    let deadline = Instant::now() + Duration::from_secs(5);
    let died = loop {
        match our_bridge.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => break false,
        }
    };
    let survived = matches!(their_bridge.try_wait(), Ok(None));
    let _ = our_bridge.kill();
    let _ = our_bridge.wait();
    let _ = their_bridge.kill();
    let _ = their_bridge.wait();
    assert!(died, "the killed bridge exits");
    assert!(survived, "the foreign bridge keeps running");
}
