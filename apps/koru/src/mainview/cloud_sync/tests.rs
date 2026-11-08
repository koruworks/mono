// SPDX-License-Identifier: MPL-2.0

//! What each Store answer becomes on the screen.
//!
//! The lines are the whole product of this module, so they are what is
//! asserted — in English, against `Lang::En`, because a test that asserted the
//! key rather than the sentence would pass while the sentence said nothing.

use super::*;
use koru_store_wire::internal::BlobTransferReport;
use koru_store_wire::internal::{
    CloudDocumentActivity, CloudLocalChange, CloudPassActivity, CloudSyncState,
};

/// A fixed "now", so a relative time is a fact rather than a race.
const NOW: i64 = 1_700_000_000_000;

fn panel() -> CloudSyncPanel {
    let mut panel = CloudSyncPanel::default();
    panel.lang = Lang::En;
    panel.visible = true;
    panel
}

fn value(panel: &CloudSyncPanel, label: &str) -> Option<String> {
    panel
        .lines
        .iter()
        .find(|line| line.label == label)
        .map(|line| line.value.clone())
}

fn labels(panel: &CloudSyncPanel) -> Vec<String> {
    panel.lines.iter().map(|line| line.label.clone()).collect()
}

fn with_status(status: CloudSyncStatus) -> CloudSyncPanel {
    let mut panel = panel();
    panel.status = Some(status);
    panel.rebuild(NOW);
    panel
}

fn local(origin: CloudChangeOrigin, device_id: Option<&str>) -> CloudSyncStatus {
    CloudSyncStatus {
        local: Some(CloudLocalChange {
            changed_at: NOW - 60_000,
            origin,
            device_id: device_id.map(str::to_owned),
        }),
        ..CloudSyncStatus::default()
    }
}

/// Cloud sync on for this Profile, as a live permit reports it.
fn on() -> Option<CloudSyncState> {
    Some(CloudSyncState {
        enabled: true,
        stage: Some("settled".into()),
        error_code: None,
    })
}

fn files(
    images_synced: u32,
    images_total: u32,
    files_synced: u32,
    files_total: u32,
) -> CloudDocumentFiles {
    CloudDocumentFiles {
        images_total,
        images_synced,
        files_total,
        files_synced,
    }
}

fn coverage(synced: Option<bool>, files: Option<CloudDocumentFiles>) -> CloudDocumentCoverage {
    CloudDocumentCoverage {
        synced,
        files,
        ..CloudDocumentCoverage::default()
    }
}

/// A panel over a syncing Profile, with this document's coverage beside the
/// report — the two halves of one poll.
fn with_coverage(
    status: CloudSyncStatus,
    coverage: Option<CloudDocumentCoverage>,
) -> CloudSyncPanel {
    let mut panel = panel();
    panel.status = Some(CloudSyncStatus {
        sync: on(),
        ..status
    });
    panel.coverage = coverage;
    panel.rebuild(NOW);
    panel
}

// --------------------------------------------------------------- the origins

#[test]
fn a_local_client_change_reads_as_this_device() {
    let panel = with_status(local(CloudChangeOrigin::ThisDevice, None));
    assert_eq!(
        value(&panel, "Local").as_deref(),
        Some("Changed 1 min · from this device")
    );
}

/// The store writing a document itself — an import, a restore, a lifecycle
/// move — is still this machine, and saying "from the Store" would name a
/// component rather than answer the question.
#[test]
fn a_store_change_also_reads_as_this_device() {
    let panel = with_status(local(CloudChangeOrigin::Store, None));
    assert_eq!(
        value(&panel, "Local").as_deref(),
        Some("Changed 1 min · from this device")
    );
}

#[test]
fn a_cloud_change_names_koru_cloud() {
    let panel = with_status(local(CloudChangeOrigin::Cloud, None));
    assert_eq!(
        value(&panel, "Local").as_deref(),
        Some("Changed 1 min · from Koru Cloud")
    );
}

#[test]
fn a_peer_change_names_the_device_when_the_agent_has_told_us_its_name() {
    let mut panel = panel();
    panel.context.devices = vec![("dev-1".into(), "WIN10-rootB".into())];
    panel.status = Some(local(CloudChangeOrigin::PeerDevice, Some("dev-1")));
    panel.rebuild(NOW);
    assert_eq!(
        value(&panel, "Local").as_deref(),
        Some("Changed 1 min · from WIN10-rootB")
    );
}

/// **An unnamed peer is "another device", never a uuid.** A row of hex is not
/// an answer, and guessing a name would be worse than admitting there is none.
#[test]
fn an_unknown_peer_stays_generic() {
    let panel = with_status(local(CloudChangeOrigin::PeerDevice, Some("dev-9")));
    assert_eq!(
        value(&panel, "Local").as_deref(),
        Some("Changed 1 min · from another device")
    );
}

/// **The clipboard says the moment the column abbreviates.** "1 min" is
/// unusable in a message; the exact moment is the point of copying at all.
#[test]
fn the_copied_local_line_carries_the_exact_moment() {
    let panel = with_status(local(CloudChangeOrigin::ThisDevice, None));
    let line = panel
        .lines
        .iter()
        .find(|line| line.label == "Local")
        .expect("a line");
    assert!(!line.detail.is_empty(), "the exact moment is spelled out");
    assert_ne!(line.detail, line.value);
}

// ------------------------------------------------------------ this document

/// **Synced is the coverage claim for this document**: its CRDT state has
/// reached Koru Cloud at the state this device holds. Anything else — never
/// carried, edited since — is *not synced yet*.
#[test]
fn the_document_line_says_whether_koru_cloud_holds_it() {
    let synced = with_coverage(
        local(CloudChangeOrigin::ThisDevice, None),
        Some(coverage(Some(true), None)),
    );
    assert_eq!(value(&synced, "This document").as_deref(), Some("Synced"));
    // The local line stays beside it: when it changed, and where from.
    assert_eq!(
        value(&synced, "Local").as_deref(),
        Some("Changed 1 min · from this device")
    );

    let behind = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(Some(false), None)),
    );
    assert_eq!(
        value(&behind, "This document").as_deref(),
        Some("Not synced yet")
    );
}

/// **Not measured is not "no".** A Store that has not computed coverage since
/// it started — or would not say — is *not checked*, and a Profile that does
/// not sync says so rather than promising a sync that is not coming.
#[test]
fn an_unmeasured_or_unsynced_profile_is_said_as_what_it_is() {
    let unmeasured = with_coverage(CloudSyncStatus::default(), Some(coverage(None, None)));
    assert_eq!(
        value(&unmeasured, "This document").as_deref(),
        Some("Not checked against Koru Cloud yet")
    );
    let no_answer = with_coverage(CloudSyncStatus::default(), None);
    assert_eq!(
        value(&no_answer, "This document").as_deref(),
        Some("Not checked against Koru Cloud yet")
    );

    let mut off = panel();
    off.status = Some(CloudSyncStatus::default());
    off.coverage = Some(coverage(Some(false), Some(files(0, 3, 0, 1))));
    off.rebuild(NOW);
    assert_eq!(
        value(&off, "This document").as_deref(),
        Some("Off, or not signed in on this device")
    );
    // Nothing is going to Koru Cloud, so there are no counts toward it.
    assert_eq!(value(&off, "Images"), None);
    assert_eq!(value(&off, "Files"), None);
}

/// A stage's stable code rides the document line, verbatim: it is what a
/// support conversation searches for.
#[test]
fn a_sync_error_code_is_carried_on_the_document_line() {
    let mut panel = panel();
    panel.status = Some(CloudSyncStatus {
        sync: Some(CloudSyncState {
            enabled: true,
            stage: Some("unavailable".into()),
            error_code: Some("sync_unavailable".into()),
        }),
        ..CloudSyncStatus::default()
    });
    panel.coverage = Some(coverage(Some(false), None));
    panel.rebuild(NOW);
    assert_eq!(
        value(&panel, "This document").as_deref(),
        Some("Not synced yet · sync_unavailable")
    );
}

/// **The column says what Manage's Profile row says.** An unavailable
/// connection, or a code meaning Koru Cloud refuses service, puts the same
/// sentence under the lines; a connection that is only connecting adds none.
#[test]
fn an_unavailable_profile_is_said_in_the_words_manage_uses() {
    let unavailable = "Koru Cloud unavailable · local content is available";
    assert_eq!(Connection::Unavailable.sentence(Lang::En), unavailable);
    let status = CloudSyncStatus {
        sync: on(),
        ..CloudSyncStatus::default()
    };
    let mut panel = panel();
    panel.status = Some(status.clone());
    panel.context.connection = Some(Connection::Unavailable);
    panel.rebuild(NOW);
    assert_eq!(panel.note.as_deref(), Some(unavailable));

    for connection in [
        Some(Connection::Connecting),
        Some(Connection::Syncing),
        None,
    ] {
        panel.context.connection = connection;
        panel.rebuild(NOW);
        assert_eq!(panel.note, None, "{connection:?}");
    }

    // The Store's own code says it too, with the code kept for support.
    let mut refused = status;
    refused.sync.as_mut().unwrap().error_code = Some("sync_unavailable".into());
    panel.status = Some(refused);
    panel.rebuild(NOW);
    assert_eq!(panel.note.as_deref(), Some(unavailable));
    assert!(
        value(&panel, "This document")
            .unwrap()
            .ends_with("· sync_unavailable")
    );
    assert_eq!(hold_sentence(Lang::En, "cloud_unavailable"), unavailable);

    // A Profile that does not sync is not told Koru Cloud is unavailable.
    panel.status = Some(CloudSyncStatus::default());
    panel.context.connection = Some(Connection::Unavailable);
    panel.rebuild(NOW);
    assert_eq!(panel.note, None);
}

/// [`Connection::of`] is the one decision; each input that means refused
/// reads as unavailable, and only an unanswered connection as connecting.
#[test]
fn a_connection_is_read_from_the_account_in_one_place() {
    use koru_agent_wire::{AccountProfileCloud, AccountStage, AccountState, CloudConnectionStage};
    let profile = "p1";
    let mut account = AccountState {
        stage: AccountStage::SignedIn,
        ..Default::default()
    };
    assert_eq!(
        Connection::of(&account, profile, false),
        Connection::Connecting
    );
    for (stage, expected) in [
        (CloudConnectionStage::Connecting, Connection::Connecting),
        (CloudConnectionStage::Syncing, Connection::Syncing),
        (CloudConnectionStage::Retrying, Connection::Unavailable),
        (
            CloudConnectionStage::WaitingForConsent,
            Connection::WaitingForConsent,
        ),
    ] {
        account.cloud_profiles = vec![AccountProfileCloud {
            discovery: None,
            profile_id: profile.into(),
            stage,
        }];
        assert_eq!(Connection::of(&account, profile, false), expected);
    }
    account.cloud_profiles[0].stage = CloudConnectionStage::Syncing;
    assert_eq!(
        Connection::of(&account, profile, true),
        Connection::Unavailable
    );
    for stage in [
        AccountStage::Unavailable,
        AccountStage::Failed,
        AccountStage::SignedOut,
    ] {
        account.stage = stage;
        assert_eq!(
            Connection::of(&account, profile, false),
            Connection::Unavailable
        );
    }
}

/// **A build with no Koru Cloud is not an outage.** Every reader of
/// [`Connection::of`] says *isn't set up in this build* — never *unavailable*
/// or *retrying* — and the rows under it treat it as stopped.
#[test]
fn a_build_without_koru_cloud_reads_as_not_configured_everywhere() {
    use koru_agent_wire::AccountState;
    let account = AccountState::not_configured();
    for refused in [false, true] {
        assert_eq!(
            Connection::of(&account, "p1", refused),
            Connection::NotConfigured
        );
    }
    let connection = Connection::NotConfigured;
    assert!(connection.is_stopped());
    assert_eq!(
        connection.sentence(Lang::En),
        "Koru Cloud isn’t set up in this build"
    );
    assert_ne!(connection.key(), Connection::Unavailable.key());
    assert!(!Connection::Connecting.is_stopped());

    // The column's note says the same thing, not the retrying sentence.
    let mut panel = panel();
    panel.status = Some(CloudSyncStatus {
        sync: on(),
        ..CloudSyncStatus::default()
    });
    panel.context.connection = Some(connection);
    panel.rebuild(NOW);
    assert_eq!(
        panel.note.as_deref(),
        Some("Koru Cloud isn’t set up in this build")
    );
}

// ------------------------------------------------------- images and files

/// **Images and files, each as synced-of-total**, and only this document's.
#[test]
fn images_and_files_are_counted_apart() {
    let panel = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(Some(true), Some(files(7, 7, 1, 1)))),
    );
    assert_eq!(value(&panel, "Images").as_deref(), Some("7 of 7 synced"));
    assert_eq!(value(&panel, "Files").as_deref(), Some("1 of 1 synced"));

    let partial = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(Some(true), Some(files(3, 7, 0, 2)))),
    );
    assert_eq!(value(&partial, "Images").as_deref(), Some("3 of 7 synced"));
    assert_eq!(value(&partial, "Files").as_deref(), Some("0 of 2 synced"));
}

/// **A document with no files draws no lines about them** — *0 of 0* is a
/// sentence about nothing — and one with only images draws no Files line.
#[test]
fn a_kind_the_document_has_none_of_draws_no_line() {
    let none = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(Some(true), Some(files(0, 0, 0, 0)))),
    );
    assert_eq!(value(&none, "Images"), None);
    assert_eq!(value(&none, "Files"), None);

    let only_images = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(Some(true), Some(files(2, 2, 0, 0)))),
    );
    assert_eq!(
        value(&only_images, "Images").as_deref(),
        Some("2 of 2 synced")
    );
    assert_eq!(value(&only_images, "Files"), None);

    // References the Store has not read yet are unknown, not none.
    let unknown = with_coverage(CloudSyncStatus::default(), Some(coverage(Some(true), None)));
    assert_eq!(value(&unknown, "Images"), None);
    assert_eq!(value(&unknown, "Files"), None);
}

/// **Before anything has been measured, the counts are not zero.** The
/// totals are known and the synced half is not, and *0 of 7* would read as
/// seven pictures lost.
#[test]
fn unmeasured_counts_are_not_zero() {
    let panel = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(None, Some(files(0, 7, 0, 1)))),
    );
    assert_eq!(
        value(&panel, "Images").as_deref(),
        Some("Not checked against Koru Cloud yet")
    );
    assert_eq!(
        value(&panel, "Files").as_deref(),
        Some("Not checked against Koru Cloud yet")
    );
}

/// **Two facts about one set of pictures, side by side.** Koru Cloud's count
/// first; then what this device is missing and who has it — the placeholder
/// the editor is painting — with the note that makes it read as a wait.
#[test]
fn missing_images_ride_the_images_line_and_the_wait_is_named_once() {
    let mut shown = panel();
    shown.context.missing_images = 2;
    shown.context.missing_image_devices = vec!["WIN10-MAIN".into()];
    shown.status = Some(CloudSyncStatus {
        sync: on(),
        ..CloudSyncStatus::default()
    });
    shown.coverage = Some(coverage(Some(true), Some(files(5, 7, 0, 0))));
    shown.rebuild(NOW);
    assert_eq!(
        value(&shown, "Images").as_deref(),
        Some("5 of 7 synced · 2 not on this device · waiting for WIN10-MAIN")
    );
    assert_eq!(
        shown.note.as_deref(),
        Some("Pictures travel after the document, so one can still be on its way.")
    );

    // Before the Store has answered, the editor's own fact is still a fact.
    let mut loading = panel();
    loading.target = Some((WorkspaceId::new(), DocumentId::new()));
    loading.context.missing_images = 1;
    loading.rebuild(NOW);
    assert_eq!(
        value(&loading, "Images").as_deref(),
        Some("1 not on this device")
    );
}

/// Nothing missing, nothing said — the note is a footnote to a problem, not a
/// standing announcement.
#[test]
fn a_document_holding_every_image_adds_no_note() {
    let panel = with_coverage(
        CloudSyncStatus::default(),
        Some(coverage(Some(true), Some(files(3, 3, 0, 0)))),
    );
    assert_eq!(panel.note, None);
}

// ------------------------------------------- nothing about the workspace

/// **The section is about the open document and nothing else** (maintainers,
/// 2026-09-24). A report carrying every workspace-wide fact — the dataset's
/// last pass, the workspace's file counts and transfer, the enable summary —
/// draws none of them, and none of their notes either: the workspace's view
/// lives in Manage Profiles.
#[test]
fn nothing_about_the_workspace_reaches_the_section() {
    let panel = with_coverage(
        CloudSyncStatus {
            document_activity: Some(CloudDocumentActivity {
                last_uploaded_at: Some(NOW - 60_000),
                ..CloudDocumentActivity::default()
            }),
            pass: Some(CloudPassActivity {
                started_at: Some(NOW - 130_000),
                finished_at: Some(NOW - 120_000),
                settled: 9,
                uploaded: 1,
                ..CloudPassActivity::default()
            }),
            blobs: Some(BlobTransferReport {
                referenced: 270,
                durable: 169,
                uploading: 1,
                direction: Some("upload".into()),
                path: Some("cloud".into()),
                preflight: Some(koru_store_wire::internal::BlobPreflightReport {
                    files: 270,
                    upload_files: 101,
                    upload_bytes: 16_000_000,
                    download_files: 0,
                    unreadable: 0,
                    at_millis: NOW,
                }),
                ..blobs()
            }),
            ..local(CloudChangeOrigin::ThisDevice, None)
        },
        Some(coverage(Some(true), Some(files(7, 7, 1, 1)))),
    );
    assert_eq!(
        labels(&panel),
        ["This document", "Local", "Images", "Files"],
        "only the document's own lines"
    );
    let everything: String = panel
        .lines
        .iter()
        .map(|line| format!("{} {}", line.value, line.detail))
        .collect();
    for workspace_fact in ["169", "270", "sending", "settled 9", "MB"] {
        assert!(
            !everything.contains(workspace_fact),
            "{workspace_fact} is about the workspace: {everything}"
        );
    }
    assert_eq!(
        panel.note, None,
        "the workspace's note is not this section's"
    );
}

// --------------------------------------------------------- what is on screen

/// The acknowledgement is said once and then stops — it is an answer to a
/// press, not a state the column stays in.
#[test]
fn copying_is_acknowledged_once() {
    let mut panel = with_status(CloudSyncStatus::default());
    panel.acknowledge_copy();
    assert_eq!(panel.note.as_deref(), Some("Copied."));
    panel.rebuild(NOW);
    assert_eq!(panel.note, None);
}

/// A hidden tab has no deadline and therefore makes no request at all.
#[test]
fn a_hidden_tab_has_no_deadline() {
    let now = Instant::now();
    let target = (WorkspaceId::new(), DocumentId::new());
    let mut panel = CloudSyncPanel::default();
    panel.maintain(Some(target), true, &CloudContext::default(), now);
    assert_eq!(panel.deadline(), Some(now + REFRESH_EVERY));
    panel.maintain(Some(target), false, &CloudContext::default(), now);
    assert_eq!(panel.deadline(), None);
}

/// **Switching tabs costs nothing.** A hidden section contributes no deadline
/// and therefore no wake, and nothing about it walks a Workspace or opens a
/// socket — the bargain History keeps and the reason this one polls at all.
#[test]
fn a_hidden_cloud_tab_has_no_clock_and_asks_for_nothing() {
    let mut panel = with_status(CloudSyncStatus::default());
    panel.target = Some((WorkspaceId::new(), DocumentId::new()));
    panel.visible = false;
    assert_eq!(panel.deadline(), None);
    assert!(panel.receiver.is_none(), "no request is in flight");
    assert!(!panel.in_flight);

    // Made visible and then hidden again, with no client: the poll is armed
    // and never fires, and the panel still reaches nothing.
    let now = Instant::now();
    panel.maintain(panel.target, true, &CloudContext::default(), now);
    panel.maintain(panel.target, false, &CloudContext::default(), now);
    assert_eq!(panel.deadline(), None);
    assert!(!panel.in_flight);
}

/// An answer about a document nobody is looking at any more is dropped, not
/// shown — the `(target, generation)` stamp doing its job, for the coverage
/// half as much as for the report.
#[test]
fn a_stale_answer_does_not_leak_between_documents() {
    let now = Instant::now();
    let old = (WorkspaceId::new(), DocumentId::new());
    let new = (old.0, DocumentId::new());
    let mut panel = panel();
    panel.target = Some(old);
    let (send, receive) = mpsc::channel();
    panel.receiver = Some(receive);
    panel.in_flight = true;
    send.send(StampedAnswer {
        target: old,
        generation: panel.generation,
        answer: Answer::Status(
            Box::new(local(CloudChangeOrigin::Cloud, None)),
            Some(Box::new(coverage(Some(true), Some(files(7, 7, 0, 0))))),
        ),
    })
    .unwrap();

    panel.maintain(Some(new), true, &CloudContext::default(), now);
    assert!(
        panel.status.is_none(),
        "the old document's answer is dropped"
    );
    assert!(panel.coverage.is_none(), "and so is its coverage");
    assert!(panel.lines.is_empty());
}

/// **Switching documents re-targets at once.** The last document's lines are
/// gone on the same frame, and the next poll is due now rather than at the
/// next five-second mark.
#[test]
fn switching_documents_forgets_the_last_one_and_asks_now() {
    let now = Instant::now();
    let first = (WorkspaceId::new(), DocumentId::new());
    let second = (first.0, DocumentId::new());
    let mut panel = panel();
    panel.maintain(Some(first), true, &CloudContext::default(), now);
    panel.status = Some(CloudSyncStatus {
        sync: on(),
        ..CloudSyncStatus::default()
    });
    panel.coverage = Some(coverage(Some(true), Some(files(7, 7, 1, 1))));
    panel.rebuild(NOW);
    assert_eq!(value(&panel, "This document").as_deref(), Some("Synced"));

    let later = now + Duration::from_secs(2);
    assert!(panel.maintain(Some(second), true, &CloudContext::default(), later));
    assert!(panel.status.is_none());
    assert!(panel.coverage.is_none());
    assert_eq!(value(&panel, "This document"), None);
    assert_eq!(panel.note.as_deref(), Some("Reading Cloud sync…"));
    // With no client there is no worker to start, so the deadline stands at
    // the moment it was re-armed: the switch, not five seconds after it.
    assert_eq!(panel.deadline(), Some(later + REFRESH_EVERY));
}

/// A new answer that differs only in its coverage is still news.
#[test]
fn a_coverage_that_moved_redraws_the_lines() {
    let now = Instant::now();
    let target = (WorkspaceId::new(), DocumentId::new());
    let mut panel = panel();
    panel.maintain(Some(target), true, &CloudContext::default(), now);
    let status = CloudSyncStatus {
        sync: on(),
        ..CloudSyncStatus::default()
    };
    for (synced, expected) in [(false, "Not synced yet"), (true, "Synced")] {
        let (send, receive) = mpsc::channel();
        panel.receiver = Some(receive);
        panel.in_flight = true;
        send.send(StampedAnswer {
            target,
            generation: panel.generation,
            answer: Answer::Status(
                Box::new(status.clone()),
                Some(Box::new(coverage(Some(synced), None))),
            ),
        })
        .unwrap();
        assert!(panel.maintain(Some(target), true, &CloudContext::default(), now));
        assert_eq!(value(&panel, "This document").as_deref(), Some(expected));
    }
}

// ------------------------------------ the shared workspace sentences (D256)
//
// The section no longer draws a transfer, but Manage Profiles' per-workspace
// rows do, through these two functions. Pinned here, where they live.

fn blobs() -> koru_store_wire::internal::BlobTransferReport {
    koru_store_wire::internal::BlobTransferReport {
        referenced: 0,
        present: 0,
        durable: 0,
        uploading: 0,
        downloading: 0,
        missing: 0,
        bytes_complete: 0,
        bytes_total: 0,
        pending_reason: None,
        last_failure_code: None,
        direction: None,
        path: None,
        object_host: None,
        oldest_retry_at_millis: None,
        unreadable_references: 0,
        walks: 1,
        passes: 1,
        passes_skipped: 0,
        paused: false,
        preflight: None,
        // No claim computed yet, which is deliberately **not** the same value
        // as "nothing is missing".
        coverage: None,
    }
}

/// **A file nobody has uploaded yet is a wait, not damage**, and a code with
/// no sentence of its own is shown verbatim rather than guessed at.
#[test]
fn holds_are_worded_and_unknown_codes_are_shown() {
    let waiting = hold_sentence(Lang::En, "object_missing");
    assert_eq!(waiting, "Waiting for the device that has these files");
    for forbidden in ["missing", "lost", "fail", "error", "broken"] {
        assert!(
            !waiting.to_lowercase().contains(forbidden),
            "the sentence suggests {forbidden}, and nothing is: {waiting}",
        );
    }
    assert_eq!(
        hold_sentence(Lang::En, "stale_incarnation"),
        "Waiting: stale_incarnation"
    );
}

// ------------------------------------------ one selected file (D259 §4)

fn selected(name: &str, mime: &str) -> SelectedFile {
    SelectedFile {
        name: name.to_owned(),
        document: DocumentId::new(),
        mime: mime.to_owned(),
    }
}

fn file_report(here: bool, cloud: Option<&str>, hold: Option<&str>) -> BlobFileReport {
    BlobFileReport {
        hash: "b3:1111".to_owned(),
        size: here.then_some(1_992_294_400),
        mime: Some("application/octet-stream".to_owned()),
        filename: Some("archive.zip".to_owned()),
        here,
        cloud: cloud.map(str::to_owned),
        hold: hold.map(str::to_owned),
    }
}

/// Build a panel showing one selected file, with a document context beside it
/// so the test can prove the document's lines give way.
fn with_selected_file(file: Option<BlobFileReport>) -> CloudSyncPanel {
    let mut panel = panel();
    panel.context.file = Some(selected("archive.zip", "application/octet-stream"));
    panel.status = Some(CloudSyncStatus {
        sync: on(),
        local: Some(CloudLocalChange {
            changed_at: NOW - 60_000,
            origin: CloudChangeOrigin::ThisDevice,
            device_id: None,
        }),
        file,
        ..CloudSyncStatus::default()
    });
    panel.coverage = Some(coverage(Some(true), Some(files(2, 2, 1, 1))));
    panel.rebuild(NOW);
    panel
}

/// **Clicking a file explains that file** (D259 §4).
///
/// The maintainers' words: *"when I click on a file, the context pane should update
/// to show the context of the selected file."* The document's lines are true
/// sentences about something nobody had selected, so they give way.
#[test]
fn selecting_a_file_shows_that_files_context() {
    let panel = with_selected_file(Some(file_report(true, Some("uploading"), None)));

    assert_eq!(value(&panel, "File").as_deref(), Some("archive.zip"));
    assert_eq!(
        value(&panel, "Type").as_deref(),
        Some("application/octet-stream · 1.9 GB"),
    );
    assert_eq!(
        value(&panel, "Where").as_deref(),
        Some("On this device, going to Koru Cloud now"),
    );
    assert_eq!(
        panel.note.as_deref(),
        Some("This file is still on its way to Koru Cloud."),
    );
    assert_eq!(labels(&panel), ["File", "Type", "Where"]);
}

/// **And clearing the selection restores the document's context.**
///
/// Nothing is stored: the lines are a projection of what is selected now, so
/// the way back is the absence of a selection.
#[test]
fn clearing_the_selection_restores_the_documents_context() {
    let mut panel = with_selected_file(Some(file_report(true, Some("durable"), None)));
    assert!(value(&panel, "File").is_some());

    panel.context.file = None;
    if let Some(status) = panel.status.as_mut() {
        status.file = None;
    }
    panel.rebuild(NOW);

    assert_eq!(
        labels(&panel),
        ["This document", "Local", "Images", "Files"]
    );
}

/// **Where the bytes are, as two facts rather than one word.** *On this
/// laptop only* and *on Koru Cloud only* are the two answers a person most
/// needs told apart: one means there is no second copy, the other means this
/// machine can fetch it.
#[test]
fn a_selected_file_says_where_its_bytes_are() {
    let said = |here, cloud| {
        let panel = with_selected_file(Some(file_report(here, cloud, None)));
        value(&panel, "Where").expect("a Where line")
    };
    assert_eq!(
        said(true, Some("durable")),
        "On this device and on Koru Cloud"
    );
    assert_eq!(said(true, Some("missing")), "On this device only");
    assert_eq!(
        said(false, Some("durable")),
        "On Koru Cloud, not on this device"
    );
    assert_eq!(
        said(false, Some("missing")),
        "Not on this device, and not yet on Koru Cloud"
    );
    // **Unmeasured is not missing.**
    assert_eq!(
        said(true, None),
        "On this device · Not checked against Koru Cloud yet"
    );
    assert_eq!(
        said(false, None),
        "Not on this device · Not checked against Koru Cloud yet"
    );
}

/// **The hold explains the file it is holding.** Telling somebody a file is
/// not on Koru Cloud while withholding *because this device is not using
/// Koru Cloud for this Workspace* is §13's failure at the scale of one file.
#[test]
fn a_held_file_says_why_rather_than_only_where() {
    let panel = with_selected_file(Some(file_report(
        false,
        Some("durable"),
        Some("mode_refuses_transport"),
    )));
    assert_eq!(
        value(&panel, "Where").as_deref(),
        Some("On Koru Cloud, not on this device"),
    );
    assert_eq!(
        panel.note.as_deref(),
        Some("Not syncing — this Workspace stays on this device"),
        "the reason outranks a description of what it caused",
    );

    let paused = with_selected_file(Some(file_report(
        true,
        Some("missing"),
        Some("paused_by_person"),
    )));
    assert_eq!(paused.note.as_deref(), Some("Paused by you"));
}

/// The answer for the *previous* file must never be drawn under this one's
/// name. A pane that did would be inventing a fact about somebody's file for
/// exactly as long as they were looking at it.
#[test]
fn a_file_with_no_answer_yet_claims_nothing_about_its_bytes() {
    let panel = with_selected_file(None);
    assert_eq!(value(&panel, "File").as_deref(), Some("archive.zip"));
    assert_eq!(
        value(&panel, "Type").as_deref(),
        Some("application/octet-stream"),
        "no size is claimed for bytes nobody has measured",
    );
    assert_eq!(
        value(&panel, "Where").as_deref(),
        Some("Not checked against Koru Cloud yet"),
    );
}
