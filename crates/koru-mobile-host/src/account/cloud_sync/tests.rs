// SPDX-License-Identifier: MPL-2.0

use super::*;

use koru_account_runtime::account::cloud_sync::{
    AccountCloudSync, DeviceCloudSync, ProfileCloudSync, STATE_VERSION, WorkspaceCloudSync,
};

fn remembered() -> DeviceCloudSync {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        profile,
        ProfileCloudSync {
            enabled: true,
            // The phone never writes it (mobile/CATCHUP.md).
            starts_on: false,
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
    let state = CloudSyncState::new(directory.path().join(STATE_FILE));
    // Nothing written yet is a starting point, not a failure: the switch has
    // simply never been touched on this phone.
    assert_eq!(state.load().unwrap(), DeviceCloudSync::default());

    let remembered = remembered();
    state.store(&remembered).unwrap();
    assert_eq!(state.load().unwrap(), remembered);

    // A second write replaces rather than appends, and leaves no temporary
    // sibling behind for the next load to trip over.
    let mut changed = remembered.clone();
    for account in changed.accounts.values_mut() {
        for profile in account.profiles.values_mut() {
            profile.enabled = false;
        }
    }
    state.store(&changed).unwrap();
    assert_eq!(state.load().unwrap(), changed);
    let entries: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from(STATE_FILE)]);
}

#[test]
fn an_unreadable_or_newer_state_file_starts_over_rather_than_refusing() {
    let directory = tempfile::tempdir().unwrap();
    let state = CloudSyncState::new(directory.path().join(STATE_FILE));

    // Truncated by a crash, or by a process death mid-write on a phone.
    std::fs::write(&state.path, b"{\"version\":1,\"accou").unwrap();
    assert_eq!(state.load().unwrap(), DeviceCloudSync::default());

    // Written by a build that understands more than this one does. Reading
    // half of it would risk disabling a Workspace on a version it never saw.
    let future = serde_json::to_vec(&serde_json::json!({
        "version": STATE_VERSION + 1,
        "accounts": {}
    }))
    .unwrap();
    std::fs::write(&state.path, future).unwrap();
    assert_eq!(state.load().unwrap(), DeviceCloudSync::default());

    // And the phone can still write its own answer over either of them.
    let remembered = remembered();
    state.store(&remembered).unwrap();
    assert_eq!(state.load().unwrap(), remembered);
}

#[test]
fn a_failed_replacement_leaves_the_previous_answer_intact() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(STATE_FILE);
    write_replacing(&path, b"first").unwrap();
    // A directory cannot be renamed over by a file write, so this replacement
    // fails; the point is that the earlier bytes survive it.
    let blocked = directory.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    assert!(write_replacing(&blocked, b"second").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
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
fn the_state_file_sits_beside_the_hosts_other_durable_state() {
    let root = std::path::Path::new("/phone/root");
    let state = CloudSyncState::new(root.join(STATE_FILE));
    // `device.json` and `trust.db` are written straight into the host root by
    // `MobileHost::open`; the remembered choice belongs with them.
    assert_eq!(state.path.parent(), Some(root));
    assert_eq!(state.path, root.join("cloud-sync.json"));
}

#[test]
fn the_catalog_is_reported_as_one_pair_per_workspace_across_profiles() {
    let first = ProfileId::new();
    let second = ProfileId::new();
    let a = WorkspaceId::new();
    let b = WorkspaceId::new();
    let c = WorkspaceId::new();
    // Two Profiles' Workspaces arrive interleaved, which is what the handler
    // groups; the port must not drop, merge or reorder a row on the way.
    let pairs = owners(vec![
        workspace(first, a),
        workspace(second, b),
        workspace(first, c),
    ]);
    assert_eq!(pairs, vec![(first, a), (second, b), (first, c)]);

    let mut grouped: std::collections::BTreeMap<ProfileId, Vec<WorkspaceId>> =
        std::collections::BTreeMap::new();
    for (owner, workspace) in pairs {
        grouped.entry(owner).or_default().push(workspace);
    }
    assert_eq!(grouped.get(&first).unwrap().len(), 2);
    assert_eq!(grouped.get(&second).unwrap(), &vec![b]);
}

#[test]
fn an_empty_catalog_is_an_answer_rather_than_a_refusal() {
    assert!(owners(Vec::new()).is_empty());
}

fn workspace(owner: ProfileId, id: WorkspaceId) -> koru_store_core::Workspace {
    // The name is exactly what the port must not carry into the runtime, so
    // the row is built with one rather than an empty string.
    let mut workspace = koru_store_core::Workspace::new(
        owner,
        "Personal notes",
        koru_store_core::Timestamp::from_millis(0),
    )
    .unwrap();
    workspace.id = id;
    workspace
}
