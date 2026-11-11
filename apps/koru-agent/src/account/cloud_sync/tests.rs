// SPDX-License-Identifier: MPL-2.0

use super::*;

use koru_account_runtime::account::cloud_sync::{
    AccountCloudSync, DeviceCloudSync, ProfileCloudSync, STATE_VERSION, WorkspaceCloudSync,
};

fn host(directory: &Path) -> AgentCloudSync {
    AgentCloudSync {
        root: None,
        state_path: directory.join("agent").join("cloud-sync.json"),
        write: Mutex::new(()),
    }
}

fn remembered() -> DeviceCloudSync {
    let profile = koru_account_core::ProfileId::new();
    let workspace = koru_account_core::WorkspaceId::new();
    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        profile,
        ProfileCloudSync {
            enabled: true,
            // Kept through a write and a read, like the rest.
            starts_on: true,
            workspaces: std::collections::BTreeMap::from([(
                workspace,
                WorkspaceCloudSync {
                    enabled: true,
                    authorization_version: 3,
                    incarnation_id: Some(uuid::Uuid::from_u128(9)),
                    observed: true,
                    error: Some("workspace_version_mismatch".into()),
                },
            )]),
        },
    );
    DeviceCloudSync {
        version: STATE_VERSION,
        accounts: std::collections::BTreeMap::from([(
            koru_account_core::KoruIdentityId::mint().to_string(),
            AccountCloudSync { profiles },
        )]),
    }
}

#[test]
fn the_remembered_choice_round_trips_through_the_state_file() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path());
    // Nothing written yet is a starting point, not a failure: the switch has
    // simply never been touched on this device.
    assert_eq!(host.load().unwrap(), DeviceCloudSync::default());

    let state = remembered();
    host.store(&state).unwrap();
    assert_eq!(host.load().unwrap(), state);

    // A second write replaces rather than appends, and leaves no temporary
    // sibling behind.
    let mut changed = state.clone();
    changed.accounts.values_mut().next().unwrap();
    for account in changed.accounts.values_mut() {
        for profile in account.profiles.values_mut() {
            profile.enabled = false;
        }
    }
    host.store(&changed).unwrap();
    assert_eq!(host.load().unwrap(), changed);
    let entries: Vec<_> = std::fs::read_dir(host.state_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("cloud-sync.json")]);
}

#[test]
fn an_unreadable_or_newer_state_file_starts_over_rather_than_refusing() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path());
    std::fs::create_dir_all(host.state_path.parent().unwrap()).unwrap();

    // Truncated by a crash before this change existed.
    std::fs::write(&host.state_path, b"{\"version\":1,\"accou").unwrap();
    assert_eq!(host.load().unwrap(), DeviceCloudSync::default());

    // Written by a build that understands more than this one does. Reading
    // half of it would risk disabling a Workspace on a version it never saw.
    let future = serde_json::to_vec(&serde_json::json!({
        "version": STATE_VERSION + 1,
        "accounts": {}
    }))
    .unwrap();
    std::fs::write(&host.state_path, future).unwrap();
    assert_eq!(host.load().unwrap(), DeviceCloudSync::default());

    // And the device can still write its own answer over either of them.
    let state = remembered();
    host.store(&state).unwrap();
    assert_eq!(host.load().unwrap(), state);
}

#[test]
fn a_failed_replacement_leaves_the_previous_answer_intact() {
    let directory = tempfile::tempdir().unwrap();
    let state_path = directory.path().join("cloud-sync.json");
    write_replacing(&state_path, b"first").unwrap();
    // A directory cannot be renamed over by a file write, so this replacement
    // fails; the point is that the earlier bytes survive it.
    let blocked = directory.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    assert!(write_replacing(&blocked, b"second").is_err());
    assert_eq!(std::fs::read(&state_path).unwrap(), b"first");
    let strays: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .collect();
    assert!(strays.is_empty(), "a failed write cleans up after itself");
}

#[test]
fn the_state_file_sits_beside_the_agents_other_durable_state() {
    let directory = tempfile::tempdir().unwrap();
    let paths = crate::Paths::for_root(directory.path().to_path_buf());
    assert_eq!(
        paths.cloud_sync_state(),
        paths.agent_dir().join("cloud-sync.json")
    );
    assert_eq!(
        paths.cloud_sync_state().parent(),
        paths.identity_db().parent()
    );
}
