// SPDX-License-Identifier: MPL-2.0

use super::fixtures::*;
use super::*;

const EN: Lang = Lang::En;
/// The connection a row's own facts are tested under: no Profile-level word
/// added, and *Connecting…* where nothing has been measured.
const CONNECTING: Connection = Connection::Connecting;

/// The open Profile's row subtitle and its workspace rows' details, after
/// the Agent reports `stage` for it and the poll answers with `rows`.
fn subtitle_and_rows(
    stage: Option<koru_agent_wire::CloudConnectionStage>,
    rows: Vec<WorkspaceProgress>,
) -> (String, Vec<String>, String) {
    let mut c = controller_with(true);
    let work = c.profiles[0].clone();
    c.account.cloud_profiles = stage
        .map(|stage| koru_agent_wire::AccountProfileCloud {
            discovery: None,
            profile_id: work.id.to_string(),
            stage,
        })
        .into_iter()
        .collect();
    c.maintain_cloud_progress(Instant::now());
    c.progress.show(work.id, rows);
    let lines = c.cloud_progress_lines().expect("the section shows");
    (
        c.cloud_sync_subtitle(&work)
            .expect("a syncing Profile says something"),
        lines.rows.into_iter().map(|row| row.detail).collect(),
        lines.summary,
    )
}

/// A Workspace enabled for Cloud with nothing measured yet: no pass, no
/// coverage, no hold. Its own report cannot say why.
fn unmeasured() -> CloudSyncStatus {
    cloud_on(blobs(0, 0))
}

/// **The bug this pins**: the Profile row said *Koru Cloud unavailable* while
/// every workspace row under it said *Connecting to Koru Cloud…*. Both now
/// read [`ProfilesController::cloud_connection`].
#[test]
fn an_unavailable_profile_says_unavailable_on_every_workspace_row() {
    use koru_agent_wire::CloudConnectionStage;
    let lang = crate::format::lang();
    let unavailable = Connection::Unavailable.sentence(lang);
    let (subtitle, rows, _) = subtitle_and_rows(
        Some(CloudConnectionStage::Retrying),
        vec![
            row("Notes", unmeasured()),
            row("Photos", partial()),
            row("Archive", held("sync_unavailable")),
            row("Done", synced()),
        ],
    );
    assert_eq!(subtitle, unavailable);
    assert_eq!(rows[0], unavailable);
    // A row whose last pass completed says it too, never *Synced*.
    assert!(rows[3].ends_with(&unavailable), "{}", rows[3]);
    assert!(
        !rows[3].contains(t(lang, TextKey::CloudTabDocSynced)),
        "{}",
        rows[3]
    );
    assert_eq!(
        rows[1],
        format!(
            "{} · {unavailable}",
            fill(t(lang, TextKey::CloudProgressDocuments), &["20", "27"])
        )
    );
    assert!(rows[2].ends_with(&unavailable), "{}", rows[2]);
    assert!(
        rows.iter()
            .all(|row| !row.contains(&Connection::Connecting.sentence(lang)))
    );
}

/// The Agent's refusal on the Profile's Cloud sync row is the same truth.
#[test]
fn a_refused_cloud_sync_row_makes_the_workspace_rows_unavailable_too() {
    let lang = crate::format::lang();
    let mut c = controller_with(true);
    let work = c.profiles[0].clone();
    c.cloud.sync.as_mut().unwrap()[0].error = Some("cloud_sync_unavailable".into());
    c.maintain_cloud_progress(Instant::now());
    c.progress.show(work.id, vec![row("Notes", unmeasured())]);
    let unavailable = Connection::Unavailable.sentence(lang);
    assert_eq!(c.cloud_connection(work.id), Connection::Unavailable);
    assert_eq!(c.cloud_sync_subtitle(&work), Some(unavailable.clone()));
    assert_eq!(
        c.cloud_progress_lines().unwrap().rows[0].detail,
        unavailable
    );
}

/// A Workspace held on a code that means Koru Cloud refuses service says the
/// unavailable wording, not *Waiting: sync_unavailable*.
#[test]
fn a_refuses_service_hold_reads_as_unavailable() {
    for code in ["sync_unavailable", "cloud_unavailable"] {
        assert_eq!(
            row_detail(EN, Some(&held(code)), CONNECTING),
            format!(
                "0 of 4 documents · {}",
                Connection::Unavailable.sentence(EN)
            )
        );
    }
}

#[test]
fn a_connecting_profile_says_connecting_on_unmeasured_rows() {
    use koru_agent_wire::CloudConnectionStage;
    let lang = crate::format::lang();
    let connecting = Connection::Connecting.sentence(lang);
    for stage in [Some(CloudConnectionStage::Connecting), None] {
        let (subtitle, rows, _) = subtitle_and_rows(
            stage,
            vec![row("Notes", unmeasured()), row("Photos", partial())],
        );
        assert_eq!(subtitle, connecting);
        assert_eq!(rows[0], connecting);
        // Measured progress is said as progress, with nothing appended.
        assert_eq!(
            rows[1],
            fill(t(lang, TextKey::CloudProgressDocuments), &["20", "27"])
        );
    }
}

#[test]
fn a_syncing_profile_shows_synced_counts() {
    use koru_agent_wire::CloudConnectionStage;
    let lang = crate::format::lang();
    let (subtitle, rows, summary) = subtitle_and_rows(
        Some(CloudConnectionStage::Syncing),
        vec![
            row("Notes", synced()),
            row("Photos", partial()),
            row("New", unmeasured()),
        ],
    );
    assert_eq!(subtitle, Connection::Syncing.sentence(lang));
    assert_eq!(rows[0], t(lang, TextKey::CloudTabDocSynced));
    assert_eq!(
        rows[1],
        fill(t(lang, TextKey::CloudProgressDocuments), &["20", "27"])
    );
    // Never *Connecting…* under a Profile that is syncing.
    assert_eq!(rows[2], Connection::Syncing.sentence(lang));
    assert_eq!(
        summary,
        fill(t(lang, TextKey::CloudProgressWorkspaces), &["1", "3"])
    );
}

#[test]
fn document_progress_precedes_blob_coverage_and_completion_ignores_file_holds() {
    let mut status = partial();
    let blobs = status.blobs.as_mut().unwrap();
    blobs.coverage = None;
    blobs.pending_reason = Some("mode_refuses_transport".into());
    assert_eq!(
        row_detail(EN, Some(&status), CONNECTING),
        "20 of 27 documents"
    );
    let pass = status.pass.as_mut().unwrap();
    pass.documents_synced = 27;
    // The last unit still has to drain and the whole pass must finish.
    assert_eq!(
        row_detail(EN, Some(&status), CONNECTING),
        "27 of 27 documents"
    );
    status.pass.as_mut().unwrap().finished_at = Some(100);
    assert_eq!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    status.pass.as_mut().unwrap().documents_changed = true;
    assert_ne!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    status.pass.as_mut().unwrap().documents_changed = false;
    status.pass.as_mut().unwrap().deferred = 1;
    assert_ne!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    status.pass.as_mut().unwrap().deferred = 0;
    status.pass.as_mut().unwrap().remote_hint = true;
    assert_ne!(row_detail(EN, Some(&status), CONNECTING), "Synced");
}

#[test]
fn an_active_pass_does_not_borrow_completion_from_an_old_coverage_sample() {
    let mut status = synced();
    status.pass = Some(koru_store_wire::internal::CloudPassActivity::default());
    assert_eq!(
        row_detail(EN, Some(&status), CONNECTING),
        t(EN, TextKey::CloudConnectionConnecting)
    );
}

#[test]
fn routine_rechecks_keep_confirmed_counts_and_workspace_completion() {
    use koru_store_wire::internal::CloudDocumentProgress;
    let mut status = partial();
    let pass = status.pass.as_mut().unwrap();
    pass.documents_required = Some(49);
    pass.documents_synced = 10;
    pass.confirmed_documents = Some(CloudDocumentProgress {
        required: 49,
        synced: 44,
        complete: false,
    });
    assert_eq!(
        row_detail(EN, Some(&status), CONNECTING),
        "44 of 49 documents"
    );
    let pass = status.pass.as_mut().unwrap();
    pass.documents_required = None;
    pass.documents_synced = 0;
    pass.confirmed_documents = Some(CloudDocumentProgress {
        required: 49,
        synced: 49,
        complete: true,
    });
    assert_eq!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    assert_eq!(
        summary(EN, &[row("Notes", status.clone())]),
        "1 of 1 workspaces synced"
    );
    status.pass.as_mut().unwrap().documents_changed = true;
    assert_ne!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    assert_eq!(
        summary(EN, &[row("Notes", status)]),
        "0 of 1 workspaces synced"
    );
}

#[test]
fn a_blank_workspace_name_has_a_visible_fallback() {
    let mut c = controller_with(true);
    let work = c.profiles[0].id;
    c.maintain_cloud_progress(Instant::now());
    c.progress.show(work, vec![row("  ", synced())]);
    assert_eq!(c.cloud_progress_lines().unwrap().rows[0].name, "Untitled");
}

/// `(name, detail)` for each row, the two strings every row draws.
fn texts(lines: &Lines) -> Vec<(String, String)> {
    lines
        .rows
        .iter()
        .map(|row| (row.name.clone(), row.detail.clone()))
        .collect()
}

#[test]
fn the_section_is_absent_for_a_local_only_profile() {
    let mut c = controller_with(true);
    let local = c.profiles[2].id;
    c.selected = Some(local);
    assert_eq!(c.cloud_progress_profile(), None);
    assert_eq!(c.cloud_progress_lines(), None);
    // And it asks nothing: no target, so no clock and no worker.
    c.maintain_cloud_progress(Instant::now());
    assert_eq!(c.progress.deadline(), None);
    assert!(!c.progress.in_flight);
}

#[test]
fn the_section_is_absent_while_the_profiles_cloud_sync_is_off() {
    let mut c = controller_with(false);
    assert_eq!(c.cloud_progress_profile(), None);
    assert_eq!(c.cloud_progress_lines(), None);
    // The account's other Profile has sync off too.
    c.selected = Some(c.profiles[1].id);
    assert_eq!(c.cloud_progress_lines(), None);
}

#[test]
fn a_profile_with_cloud_sync_on_reads_first_then_draws_its_rows() {
    let mut c = controller_with(true);
    let work = c.profiles[0].id;
    assert_eq!(c.cloud_progress_profile(), Some(work));
    // Before the poll has anything, the section says so rather than zeros.
    assert_eq!(
        c.cloud_progress_lines().map(|lines| lines.summary),
        Some("Reading Cloud sync…".to_owned())
    );

    let now = Instant::now();
    assert!(c.maintain_cloud_progress(now));
    // Visible and pointed at the Profile: the poll has a clock.
    assert!(c.progress.deadline().is_some());

    let generation = c.progress.generation;
    c.progress.answer(
        work,
        generation,
        vec![row("Notes", synced()), row("Photos", partial())],
    );
    assert!(c.maintain_cloud_progress(now));
    let lines = c.cloud_progress_lines().expect("the section shows");
    assert_eq!(lines.summary, "1 of 2 workspaces synced");
    assert_eq!(
        texts(&lines),
        vec![
            ("Notes".to_owned(), "Synced".to_owned()),
            ("Photos".to_owned(), "20 of 27 documents".to_owned()),
        ]
    );
}

#[test]
fn a_stale_answer_for_another_profile_is_dropped() {
    let mut c = controller_with(true);
    let work = c.profiles[0].id;
    let now = Instant::now();
    c.maintain_cloud_progress(now);
    let stale = c.progress.generation.wrapping_sub(1);
    c.progress.answer(work, stale, vec![row("Notes", synced())]);
    c.maintain_cloud_progress(now);
    assert_eq!(c.progress.rows, None);
}

#[test]
fn hiding_the_tab_stops_the_poll() {
    let mut c = controller_with(true);
    let now = Instant::now();
    c.maintain_cloud_progress(now);
    assert!(c.progress.deadline().is_some());
    c.set_cloud_visible(false);
    c.maintain_cloud_progress(now);
    assert_eq!(c.progress.deadline(), None);
}

#[test]
fn a_fully_synced_workspace_reads_as_synced() {
    assert_eq!(row_detail(EN, Some(&synced()), CONNECTING), "Synced");
    assert_eq!(
        summary(EN, &[row("Notes", synced())]),
        "1 of 1 workspaces synced"
    );
}

#[test]
fn a_partially_synced_workspace_shows_document_progress() {
    let status = partial();
    assert_eq!(
        row_detail(EN, Some(&status), CONNECTING),
        "20 of 27 documents"
    );
}

#[test]
fn files_missing_from_cloud_can_be_expanded_without_a_download_attempt() {
    let mut status = synced();
    let coverage = status.blobs.as_mut().unwrap().coverage.as_mut().unwrap();
    coverage.blobs_missing = 1;
    assert!(openable(Some(&status)));
    assert_eq!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    status
        .blobs
        .as_mut()
        .unwrap()
        .coverage
        .as_mut()
        .unwrap()
        .state = "superseded".into();
    assert!(!openable(Some(&status)));
}

#[test]
fn a_held_workspace_gives_the_context_panes_reason() {
    assert_eq!(
        row_detail(EN, Some(&held("cloud_access_unavailable")), CONNECTING),
        "0 of 4 documents · Not syncing — Cloud sync is off for this Profile"
    );
    assert_eq!(
        row_detail(EN, Some(&held("mode_refuses_transport")), CONNECTING),
        "0 of 4 documents · Not syncing — this Workspace stays on this device"
    );
    // One map for both surfaces: whatever the context pane says, this says.
    for code in [
        "network_metered",
        "network_offline",
        "object_missing",
        "some_code_this_build_has_not_heard_of",
    ] {
        assert!(
            row_detail(EN, Some(&held(code)), CONNECTING)
                .ends_with(&crate::mainview::cloud_sync::hold_sentence(EN, code))
        );
    }
    let mut paused = synced();
    paused.blobs.as_mut().unwrap().paused = true;
    assert_eq!(row_detail(EN, Some(&paused), CONNECTING), "Synced");
    // A code the Cloud permit stopped on is a hold too.
    let mut refused = synced();
    refused.sync.as_mut().unwrap().error_code = Some("quota_exceeded".into());
    assert!(row_detail(EN, Some(&refused), CONNECTING).ends_with("Waiting: quota_exceeded"));
}

#[test]
fn the_summary_counts_what_is_held_and_formats_its_numbers() {
    let rows = [
        row("Notes", synced()),
        row("Photos", partial()),
        row("Archive", held("network_metered")),
        row("Scans", held("paused_by_person")),
        row("Big", cloud_on(blobs(3, 1_000))),
    ];
    assert_eq!(summary(EN, &rows), "1 of 5 workspaces synced · 2 held");
}

#[test]
fn a_workspace_not_on_cloud_is_counted_out_but_not_as_held() {
    let off = CloudSyncStatus {
        sync: Some(koru_store_wire::internal::CloudSyncState {
            enabled: false,
            stage: None,
            error_code: None,
        }),
        blobs: Some(blobs(0, 2)),
        ..Default::default()
    };
    assert_eq!(
        row_detail(EN, Some(&off), CONNECTING),
        t(EN, TextKey::CloudTabSyncOff)
    );
    assert_eq!(
        summary(EN, &[row("Notes", synced()), row("Local", off)]),
        "1 of 2 workspaces synced"
    );
}

/// A completed pass survives a missing permit while the Profile is
/// connecting — and only then: under an unavailable Profile the same row
/// says unavailable, not *Synced* (maintainers, 2026-10-10).
#[test]
fn missing_permit_preserves_completion_only_while_connecting() {
    let mut status = partial();
    let pass = status.pass.as_mut().unwrap();
    pass.documents_synced = pass.documents_required.unwrap();
    pass.finished_at = Some(100);
    status.sync.as_mut().unwrap().stage = None;
    assert_eq!(row_detail(EN, Some(&status), CONNECTING), "Synced");
    let unavailable = row_detail(EN, Some(&status), Connection::Unavailable);
    assert!(
        unavailable.ends_with(&Connection::Unavailable.sentence(EN)),
        "{unavailable}"
    );
    assert!(!unavailable.contains("Synced"), "{unavailable}");
    status.sync = None;
    assert_eq!(
        row_detail(EN, Some(&status), CONNECTING),
        t(EN, TextKey::CloudTabUnavailable)
    );
}

#[test]
fn documents_are_said_only_when_a_claim_may_be_read() {
    let mut unmeasured = synced();
    unmeasured.blobs.as_mut().unwrap().coverage = None;
    assert_eq!(
        row_detail(EN, Some(&unmeasured), CONNECTING),
        t(EN, TextKey::CloudConnectionConnecting)
    );
    let mut superseded = synced();
    superseded.blobs.as_mut().unwrap().coverage = Some(coverage("superseded", 27, 0));
    assert_eq!(
        row_detail(EN, Some(&superseded), CONNECTING),
        t(EN, TextKey::CloudConnectionConnecting)
    );
    // Nothing measured, nothing referenced, nothing moving: the Sync word.
    let bare = cloud_on(blobs(0, 0));
    assert_eq!(
        row_detail(EN, Some(&bare), CONNECTING),
        t(EN, TextKey::CloudConnectionConnecting)
    );
    // A Workspace the Store would not answer about says so, alone.
    assert_eq!(
        row_detail(EN, None, CONNECTING),
        "The Store is not answering."
    );
}

/// **One line per document**, with how many files and for whom — or, when
/// the Store knows of no holder, that no device online has them.
#[test]
fn a_document_is_one_line_with_its_file_count_and_whom_it_waits_for() {
    assert_eq!(
        document_line(EN, &waiting("Trip notes", 6, &["Pixel 8"])),
        "Trip notes · 6 files · waiting for Pixel 8"
    );
    assert_eq!(
        document_line(EN, &waiting("Recipes", 1, &[])),
        "Recipes · 1 file · no device online has it"
    );
    assert_eq!(
        document_line(EN, &waiting("  ", 2, &["Pixel 8", "Work laptop"])),
        "Untitled · 2 files · waiting for Pixel 8, Work laptop"
    );
    assert_eq!(
        document_line(EN, &waiting("Scans", 1_200, &[])),
        "Scans · 1,200 files · no device online has it"
    );
    // Nothing waiting on another device is said, not left blank.
    assert_eq!(
        waiting_lines(EN, &Waiting::Documents(Vec::new()), WAITING_CAP),
        vec!["No document is waiting for another device.".to_owned()]
    );
    assert_eq!(
        waiting_lines(EN, &Waiting::Unavailable, WAITING_CAP),
        vec!["The Store is not answering.".to_owned()]
    );
}

/// Waiting-document details remain bounded at twenty lines.
#[test]
fn an_open_row_is_capped_at_twenty_lines() {
    let many: Vec<CloudWaitingDocument> = (0..25)
        .map(|index| waiting(&format!("Page {index}"), 1, &[]))
        .collect();
    let lines = waiting_lines(EN, &Waiting::Documents(many.clone()), WAITING_CAP);
    assert_eq!(lines.len(), 20);
    assert_eq!(lines[18], "Page 18 · 1 file · no device online has it");
    assert_eq!(lines[19], "and 6 more");
    let exactly = waiting_lines(EN, &Waiting::Documents(many[..20].to_vec()), WAITING_CAP);
    assert_eq!(exactly.len(), 20);
    assert!(exactly[19].starts_with("Page 19"));
    // A short pane: the row keeps what fits, ending on its own *more*.
    let short = waiting_lines(EN, &Waiting::Documents(many), 3);
    assert_eq!(short.last().map(String::as_str), Some("and 23 more"));
}

/// **Only a held row opens**: files parked with no hold on the line open too;
/// a row the Store would not answer about does not.
#[test]
fn only_a_held_row_can_open() {
    assert!(!openable(Some(&synced())));
    assert!(openable(Some(&held("object_missing"))));
    let mut parked = synced();
    parked.blobs.as_mut().unwrap().missing = 2;
    assert!(openable(Some(&parked)));
    assert!(!openable(None));

    let mut c = controller_with(true);
    let work = c.profiles[0].id;
    c.maintain_cloud_progress(Instant::now());
    let notes = row("Notes", synced());
    let photos = row("Photos", held("object_missing"));
    let (notes_id, photos_id) = (notes.id, photos.id);
    c.progress.show(work, vec![notes, photos]);
    assert!(!c.progress.toggle(notes_id, Instant::now()));
    c.perform(Intent::ToggleProgress(photos_id));
    let lines = c.cloud_progress_lines().unwrap();
    assert_eq!(lines.rows[0].waiting, None);
    // Open, and asking at once rather than at the next tick.
    assert_eq!(lines.rows[1].waiting, Some(Waiting::Reading));
    assert!(
        c.progress
            .next_refresh
            .is_some_and(|due| due <= Instant::now())
    );
    c.perform(Intent::ToggleProgress(photos_id));
    assert_eq!(c.cloud_progress_lines().unwrap().rows[1].waiting, None);
}

/// A row that stops being held closes itself: there is nothing left to list.
#[test]
fn a_row_that_is_no_longer_held_closes() {
    let mut c = controller_with(true);
    let work = c.profiles[0].id;
    c.maintain_cloud_progress(Instant::now());
    let photos = row("Photos", held("object_missing"));
    let photos_id = photos.id;
    c.progress.show(work, vec![photos.clone()]);
    c.perform(Intent::ToggleProgress(photos_id));
    let generation = c.progress.generation;
    c.progress.answer(
        work,
        generation,
        vec![WorkspaceProgress {
            status: Some(synced()),
            ..photos
        }],
    );
    c.maintain_cloud_progress(Instant::now());
    assert!(c.progress.open.is_empty());
    assert_eq!(c.cloud_progress_lines().unwrap().rows[0].waiting, None);
}
