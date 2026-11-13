// SPDX-License-Identifier: MPL-2.0

//! Cloud progress fixtures, shared by the controller's tests and the view's.
use super::*;
use koru_agent_wire::CloudSyncProfile;
use koru_store_wire::internal::{BlobCoverageReport, BlobTransferReport, CloudSyncState};
use koru_workspace_host::account_ui::CommandAdmission;

pub(crate) const ACCOUNT: &str = "10000000-0000-4000-8000-000000000001";

pub(crate) fn profile(name: &str, account: Option<&str>) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: account.map(|id| format!("koru:{id}")),
        folder: String::new(),
    }
}

/// A signed-in, subscribed controller with one account Profile whose Cloud
/// sync is `work_on`, one whose sync is off, and one local-only Profile —
/// driven through the same commands and answers the Agent sends, so the gate
/// under test is the real one.
pub(crate) fn controller_with(work_on: bool) -> ProfilesController {
    let mut c = ProfilesController::new();
    // A new controller asks for its status first; start from an idle queue.
    c.take_request();
    c.account = AccountState {
        active_scope: 1,
        active_account_id: Some(ACCOUNT.into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    };
    c.set_cloud_visible(true);
    c.profiles = vec![
        profile("Work", Some(ACCOUNT)),
        profile("Home", Some(ACCOUNT)),
        profile("Scratch", None),
    ];
    c.selected = Some(c.profiles[0].id);

    let (AccountCommand::InspectCloudSubscription { request_id }, Some(submission)) =
        c.take_request_with_submission().unwrap()
    else {
        panic!("expected cloud inspection");
    };
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 0
    }));
    assert!(c.complete_scoped(
        1,
        0,
        AccountOperationResult::CloudSubscription {
            request_id,
            current: Some(true),
            access: Some(true),
        }
    ));
    let (AccountCommand::InspectCloudSync { request_id }, Some(submission)) =
        c.take_request_with_submission().unwrap()
    else {
        panic!("expected a per-Profile cloud read");
    };
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 0
    }));
    let rows = c.profiles[..2]
        .iter()
        .zip([work_on, false])
        .map(|(profile, enabled)| CloudSyncProfile {
            profile_id: profile.id.to_storage(),
            enabled,
            workspaces: 3,
            synced_workspaces: 3,
            error: None,
        })
        .collect();
    assert!(c.complete_scoped(
        1,
        0,
        AccountOperationResult::CloudSync {
            request_id,
            profiles: rows
        }
    ));
    c
}

pub(crate) fn blobs(durable: u32, referenced: u32) -> BlobTransferReport {
    BlobTransferReport {
        referenced,
        present: referenced,
        durable,
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
        coverage: None,
        paused: false,
        preflight: None,
    }
}

pub(crate) fn coverage(state: &str, required: u32, missing: u32) -> BlobCoverageReport {
    BlobCoverageReport {
        state: state.into(),
        current: state == "complete",
        documents_required: required,
        documents_missing: missing,
        blobs_referenced: 0,
        blobs_durable: 0,
        blobs_uploading: 0,
        blobs_missing: 0,
        unreadable_references: 0,
        sample: String::new(),
    }
}

pub(crate) fn cloud_on(blobs: BlobTransferReport) -> CloudSyncStatus {
    CloudSyncStatus {
        sync: Some(CloudSyncState {
            enabled: true,
            stage: Some("settled".into()),
            error_code: None,
        }),
        blobs: Some(blobs),
        ..Default::default()
    }
}

/// 27 of 27 documents, 270 of 270 files, nothing moving.
pub(crate) fn synced() -> CloudSyncStatus {
    cloud_on(BlobTransferReport {
        coverage: Some(coverage("complete", 27, 0)),
        ..blobs(270, 270)
    })
}

/// 20 of 27 documents, 169 of 270 files, one upload running through Cloud.
pub(crate) fn partial() -> CloudSyncStatus {
    let mut status = cloud_on(BlobTransferReport {
        coverage: Some(coverage("incomplete", 27, 7)),
        uploading: 1,
        direction: Some("upload".into()),
        path: Some("cloud".into()),
        ..blobs(169, 270)
    });
    status.pass = Some(koru_store_wire::internal::CloudPassActivity {
        documents_required: Some(27),
        documents_synced: 20,
        ..Default::default()
    });
    status
}

/// Held on a code, with the same counts shape as the others.
pub(crate) fn held(code: &str) -> CloudSyncStatus {
    let mut status = cloud_on(BlobTransferReport {
        coverage: Some(coverage("incomplete", 4, 4)),
        pending_reason: Some(code.into()),
        ..blobs(0, 12)
    });
    status.sync.as_mut().unwrap().error_code = Some(code.into());
    status.pass = Some(koru_store_wire::internal::CloudPassActivity {
        documents_required: Some(4),
        documents_synced: 0,
        ..Default::default()
    });
    status
}

pub(crate) fn row(name: &str, status: CloudSyncStatus) -> WorkspaceProgress {
    WorkspaceProgress {
        id: WorkspaceId::new(),
        name: name.into(),
        status: Some(status),
        waiting: None,
    }
}

pub(crate) fn waiting(title: &str, files: u32, holders: &[&str]) -> CloudWaitingDocument {
    CloudWaitingDocument {
        document: DocumentId::new(),
        title: title.into(),
        missing_files: files,
        holders: holders.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// Test access to the poll: what a worker would have answered, and what the
/// poll holds.
impl Poll {
    /// Point the poll at `target` with `rows` already read.
    pub(crate) fn show(&mut self, target: ProfileId, rows: Vec<WorkspaceProgress>) {
        self.target = Some(target);
        self.rows = Some(rows);
    }
    /// Queue a worker's answer for `generation`, read on the next maintain.
    pub(crate) fn answer(
        &mut self,
        target: ProfileId,
        generation: u64,
        rows: Vec<WorkspaceProgress>,
    ) {
        let (send, receive) = mpsc::channel();
        self.receiver = Some(receive);
        self.in_flight = true;
        send.send(Stamped {
            target,
            generation,
            answer: Answer::Read {
                rows,
                manifests: HashMap::new(),
            },
        })
        .unwrap();
    }
}
