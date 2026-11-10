// SPDX-License-Identifier: MPL-2.0

//! The Store's half of A1 against a real catalog: the immediate stop, what a
//! failed write is allowed to claim, what survives a restart, and the one thing
//! a device-local mode must never do — leave the device.

use super::*;
use koru_store_core::errors::Code;
use koru_store_core::ids::ProfileId;
use koru_store_core::model::{Profile, Workspace};
use koru_store_core::transport::{
    InitializeTransportMode, SetTransportMode, SharingEvidence, TransportMode, TransportModeCommit,
    TransportPath, TransportSubject,
};

type RealStore = Store<SqliteCatalog, SqliteBackend>;

/// A store whose catalog is a file, so the same bytes can be reopened.
fn durable_store(directory: &std::path::Path) -> RealStore {
    Store::open(
        SqliteCatalog::open(&directory.join("catalog.db")).unwrap(),
        SqliteBackend::new(directory),
    )
    .unwrap()
}

fn profile_and_workspace(store: &mut RealStore) -> (ProfileId, WorkspaceId) {
    let profile: Profile = store
        .create_profile("Work", None, None, Locale::default(), at())
        .unwrap();
    let workspace = store.create_workspace(profile.id, "ws", at()).unwrap().id;
    (profile.id, workspace)
}

fn set(subject: TransportSubject, expected: i64, mode: TransportMode) -> SetTransportMode {
    SetTransportMode {
        subject,
        expected_revision: expected,
        mode,
    }
}

/// Raise every Workspace and Profile to Cloud, so a later restriction has
/// somewhere to fall from.
fn raise_to_cloud(store: &mut RealStore, profile: ProfileId, workspace: WorkspaceId) {
    store
        .set_transport_modes(&[
            set(TransportSubject::Profile(profile), 1, TransportMode::Cloud),
            set(
                TransportSubject::Workspace(workspace),
                1,
                TransportMode::Cloud,
            ),
        ])
        .unwrap();
}

// -------------------------------------------------------- the immediate stop

#[test]
fn a_restrictive_choice_closes_admission_before_the_durable_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);
    assert!(store.admit_transport(subject, TransportPath::Cloud).is_ok());
    let before = store.policy_generation();

    // The write cannot land. A real disk failure looks the same from here:
    // the operation returns an error and nothing reached the row.
    refuse_transport_writes(dir.path());
    let error = store
        .set_transport_mode(set(subject, 2, TransportMode::LocalOnly))
        .unwrap_err();

    assert_eq!(error.code(), Code::StorageFailed);
    // The guard is closed even though nothing was saved, and it says so.
    let admission = store.transport_admission(subject).unwrap();
    assert!(admission.fenced);
    assert_eq!(admission.mode, TransportMode::LocalOnly);
    assert_ne!(
        admission.generation, before,
        "old answers must not be reused"
    );
    assert_eq!(
        store
            .admit_transport(subject, TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
    // And it claims nothing about durability: the committed row is untouched.
    assert_eq!(admission.stored_mode, TransportMode::Cloud);
    assert_eq!(admission.revision, 2);
}

#[test]
fn a_failed_write_promises_nothing_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);

    refuse_transport_writes(dir.path());
    store
        .set_transport_mode(set(subject, 2, TransportMode::LocalOnly))
        .unwrap_err();
    assert!(store.transport_admission(subject).unwrap().fenced);
    drop(store);

    // Restart reloads the **last actually committed** mode. The fence is a
    // session guard, and a write that failed must not look like one that
    // succeeded — which is the honest, and the unsafe-sounding, direction.
    let store = durable_store(dir.path());
    let admission = store.transport_admission(subject).unwrap();
    assert!(!admission.fenced);
    assert_eq!(admission.mode, TransportMode::Cloud);
}

#[test]
fn a_committed_local_only_choice_is_still_local_only_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);

    store
        .set_transport_mode(set(subject, 2, TransportMode::LocalOnly))
        .unwrap();
    // The fence reopens once the row is the authority again.
    assert!(!store.transport_admission(subject).unwrap().fenced);
    drop(store);

    let mut store = durable_store(dir.path());
    let admission = store.transport_admission(subject).unwrap();
    assert_eq!(admission.mode, TransportMode::LocalOnly);
    assert!(admission.explicit_choice);
    // And a verified promotion arriving afterwards cannot raise it again: only
    // an explicit local choice can.
    store
        .initialize_transport_mode(InitializeTransportMode {
            subject,
            expected_revision: admission.revision,
            mode: TransportMode::Cloud,
        })
        .unwrap();
    assert_eq!(
        store.transport_admission(subject).unwrap().mode,
        TransportMode::LocalOnly
    );
}

#[test]
fn lowering_a_profile_closes_every_child_it_covers() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, one) = profile_and_workspace(&mut store);
    let two = store.create_workspace(profile, "second", at()).unwrap().id;
    store
        .set_transport_modes(&[
            set(TransportSubject::Profile(profile), 1, TransportMode::Cloud),
            set(TransportSubject::Workspace(one), 1, TransportMode::Cloud),
            set(TransportSubject::Workspace(two), 1, TransportMode::Cloud),
        ])
        .unwrap();

    refuse_transport_writes(dir.path());
    store
        .set_transport_modes(&[
            set(
                TransportSubject::Profile(profile),
                2,
                TransportMode::LocalOnly,
            ),
            set(
                TransportSubject::Workspace(one),
                2,
                TransportMode::LocalOnly,
            ),
            set(
                TransportSubject::Workspace(two),
                2,
                TransportMode::LocalOnly,
            ),
        ])
        .unwrap_err();

    // Nothing under the Profile may move, including the children the caller
    // happened to name and the ones it did not: the fence is about what may
    // move now, not about which rows were going to be written.
    for subject in [
        TransportSubject::Profile(profile),
        TransportSubject::Workspace(one),
        TransportSubject::Workspace(two),
    ] {
        let admission = store.transport_admission(subject).unwrap();
        assert!(admission.fenced, "{subject:?}");
        assert_eq!(admission.mode, TransportMode::LocalOnly, "{subject:?}");
    }
}

// ----------------------------------------------------- export and admission

#[test]
fn the_local_stop_closes_both_transport_paths_and_leaves_local_work_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);

    // Cloud: both directions open.
    assert!(store.admit_transport(subject, TransportPath::Cloud).is_ok());
    assert!(store.admit_transport(subject, TransportPath::Peer).is_ok());

    // P2P: the peer path stays, Cloud closes.
    store
        .set_transport_mode(set(subject, 2, TransportMode::P2p))
        .unwrap();
    assert!(store.admit_transport(subject, TransportPath::Peer).is_ok());
    assert_eq!(
        store
            .admit_transport(subject, TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );

    // Local-only: nothing leaves and nothing is admitted, for text and bytes
    // alike (D245).
    store
        .set_transport_mode(set(subject, 3, TransportMode::LocalOnly))
        .unwrap();
    for path in [TransportPath::Peer, TransportPath::Cloud] {
        assert_eq!(
            store.admit_transport(subject, path).unwrap_err().code(),
            Code::Forbidden,
            "{path:?}"
        );
    }
    // Local editing is untouched — the Store is healthy and the content is
    // still there.
    assert_eq!(store.documents(workspace).unwrap().len(), 0);
    assert!(store.export_workspace(workspace, at()).is_ok());
}

#[test]
fn the_profile_ceiling_applies_to_every_check_and_a_mismatch_stops_the_transport() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);

    // Move the Workspace under a Local-only Profile. The move is supported and
    // the child's choice is not rewritten, so the ladder is now inconsistent.
    let elsewhere = store
        .create_profile("Elsewhere", None, None, Locale::default(), at())
        .unwrap();
    store.move_workspace(workspace, elsewhere.id).unwrap();

    let admission = store.transport_admission(subject).unwrap();
    assert!(admission.ceiling_mismatch);
    assert_eq!(admission.stored_mode, TransportMode::Cloud, "repaired");
    assert_eq!(admission.mode, TransportMode::LocalOnly);
    // Reported as an inconsistency rather than as an ordinary refusal, so the
    // caller can say something true about it.
    assert_eq!(
        store
            .admit_transport(subject, TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
}

#[test]
fn a_missing_row_stops_the_transport_instead_of_defaulting() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (_, workspace) = profile_and_workspace(&mut store);
    let subject = TransportSubject::Workspace(workspace);
    delete_transport_row(dir.path(), workspace);

    assert_eq!(
        store.transport_admission(subject).unwrap_err().code(),
        Code::CorruptState
    );
    assert_eq!(
        store
            .admit_transport(subject, TransportPath::Peer)
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    // The content is untouched and still openable locally.
    assert!(store.documents(workspace).is_ok());
}

// ------------------------------------------------ the mode stays on the device

#[test]
fn a_mode_never_reaches_content_discovery_or_peer_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    let (document, _) = store
        .create_document(workspace, "A page", "document", None, None, at())
        .unwrap();
    store.flush(workspace, document, at()).unwrap();
    raise_to_cloud(&mut store, profile, workspace);

    // Everything that leaves this device, or could: the exportable bundle, the
    // manifest snapshot a peer or the Cloud receives, the anti-entropy digest
    // that drives discovery, and the local record of a remote state change.
    let bundle = store.export_workspace(workspace, at()).unwrap();
    let mut leaving: Vec<Vec<u8>> = vec![
        serde_json::to_vec(&bundle.manifest).unwrap(),
        store.snapshot(workspace, document).unwrap(),
        store.workspace_sync_digest(workspace).unwrap(),
    ];
    for (_, snapshot) in &bundle.snapshots {
        leaving.push(snapshot.clone());
    }
    let manifest_id = store.manifest_id(workspace).unwrap();
    leaving.push(store.snapshot(workspace, manifest_id).unwrap());
    for unit in store.profile_unit_states(profile).unwrap() {
        leaving.push(unit.snapshot);
    }

    // Byte scan rather than a field check, because the failure being guarded
    // against is a mode riding along inside something nobody thought to look
    // at — a serde derive added later, a debug field, an attribute map.
    for needle in [
        "local_transport_modes",
        "transport_mode",
        "explicit_choice",
        "local_only",
    ] {
        for payload in &leaving {
            assert!(
                !contains(payload, needle.as_bytes()),
                "{needle} left the device"
            );
        }
    }
    // "cloud" is a word other things use, so it is asserted only where it
    // would mean this: the mode of the Workspace that was just set to Cloud.
    assert_eq!(
        store
            .transport_mode(TransportSubject::Workspace(workspace))
            .unwrap()
            .mode,
        TransportMode::Cloud
    );
    assert!(!contains(&leaving[0], b"cloud"));

    // And the mode has no home outside catalog.db: the Workspace's own
    // database, which is what a peer or the Cloud replicates, has no table
    // that could hold one.
    let path = SqliteBackend::new(dir.path()).workspace_db(workspace);
    let workspace_db = rusqlite::Connection::open(&path).unwrap();
    let mut names = workspace_db
        .prepare("SELECT name FROM sqlite_master WHERE name LIKE '%transport%'")
        .unwrap();
    assert_eq!(
        names
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .count(),
        0
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ------------------------------------------------------------------ fixtures

/// Make every write to the mode table fail, the way a failing disk would.
///
/// A trigger installed over a second connection to the same file, rather than a
/// fake catalog: the failure then happens at the exact statement the real
/// operation runs, inside the real transaction. Arming the state to be tested
/// rather than asserting that some connection fails.
fn refuse_transport_writes(directory: &std::path::Path) {
    beside(directory)
        .execute_batch(
            "CREATE TRIGGER refuse_transport_writes
             BEFORE UPDATE ON local_transport_modes
             BEGIN SELECT RAISE(ABORT, 'simulated storage failure'); END;",
        )
        .unwrap();
}

fn delete_transport_row(directory: &std::path::Path, workspace: WorkspaceId) {
    beside(directory)
        .execute(
            "DELETE FROM local_transport_modes
              WHERE subject_kind = 'workspace' AND subject_id = ?1",
            rusqlite::params![workspace.to_storage()],
        )
        .unwrap();
}

/// A second connection to the catalog the live Store is holding open. WAL and
/// the shared busy timeout make this ordinary concurrent access, not a trick.
fn beside(directory: &std::path::Path) -> rusqlite::Connection {
    crate::db::open(&directory.join("catalog.db")).unwrap()
}

#[test]
fn an_exhausted_revision_refuses_and_holds_that_transport_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    let subject = TransportSubject::Workspace(workspace);
    store
        .set_transport_modes(&[
            set(TransportSubject::Profile(profile), 1, TransportMode::Cloud),
            set(subject, 1, TransportMode::P2p),
        ])
        .unwrap();
    exhaust_revision(dir.path(), workspace);

    // The request is *permissive* — P2P up to Cloud, which the Profile already
    // allows. It still cannot be written, and that is the point: a row that can
    // never change again is one this device could never turn off later, so
    // contract §2 holds its transport closed until somebody repairs it.
    let error = store
        .set_transport_mode(set(subject, i64::MAX, TransportMode::Cloud))
        .unwrap_err();

    assert_eq!(error.code(), Code::CorruptState);
    let admission = store.transport_admission(subject).unwrap();
    assert!(admission.fenced);
    assert_eq!(admission.mode, TransportMode::LocalOnly);
    assert_eq!(
        store
            .admit_transport(subject, TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
}

fn exhaust_revision(directory: &std::path::Path, workspace: WorkspaceId) {
    beside(directory)
        .execute(
            "UPDATE local_transport_modes SET revision = ?2
              WHERE subject_kind = 'workspace' AND subject_id = ?1",
            rusqlite::params![workspace.to_storage(), i64::MAX],
        )
        .unwrap();
}

// ------------------------------------------------------ tombstone and recovery

#[test]
fn a_tombstone_keeps_its_mode_and_gets_it_back_on_restore() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);

    store.delete_workspace(workspace, at()).unwrap();
    // A Workspace in the Trash keeps its choice: a tombstone is retired state,
    // not erased state, and restoring it must not silently re-enable or
    // silently disable a transport the person chose.
    let tombstoned = store.transport_mode(subject).unwrap();
    assert_eq!(tombstoned.mode, TransportMode::Cloud);
    assert!(tombstoned.explicit_choice);
    assert_eq!(tombstoned.revision, 2);

    store.restore_workspace(workspace).unwrap();
    assert_eq!(store.transport_mode(subject).unwrap(), tombstoned);

    // Same-device recovery — reopening the same bytes — preserves it too.
    drop(store);
    let store = durable_store(dir.path());
    assert_eq!(store.transport_mode(subject).unwrap(), tombstoned);
}

#[test]
fn materialization_on_this_device_initializes_this_devices_own_choice() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let profile = store
        .create_profile("Work", None, None, Locale::default(), at())
        .unwrap();
    store
        .set_transport_mode(set(
            TransportSubject::Profile(profile.id),
            1,
            TransportMode::Cloud,
        ))
        .unwrap();

    // A Workspace arriving from somewhere else. **Whatever the other device
    // chose is that device's business and does not travel** — what this one
    // starts at is *this* device's own Profile choice, read downward.
    //
    // Before D258 the sentence here was "starts Local-only and is promoted by
    // this device's evidence". Evidence promotion is withdrawn, so a row that
    // started Local-only would stay there until somebody answered the Profile
    // prompt again — and every Workspace that arrived from a phone would
    // silently withhold its pictures while its documents synced.
    let arriving = Workspace::new(profile.id, "From the phone", at()).unwrap();
    store
        .provision_replica(
            &profile,
            &arriving,
            koru_store_core::ids::DocumentId::new(),
            at(),
        )
        .unwrap();

    let state = store
        .transport_mode(TransportSubject::Workspace(arriving.id))
        .unwrap();
    assert_eq!(state.mode, TransportMode::Cloud);
    assert!(
        !state.explicit_choice,
        "inheriting a Profile's choice is a default, not a second choice",
    );
    assert_eq!(state.revision, 1);
    assert_eq!(state.owner, profile.id);
}

/// **A Profile nobody has spoken about still seeds its children Local-only.**
///
/// The inheritance above copies a *person's* explicit Profile choice. A
/// Profile still sitting at migration 12's seeded default has no choice to
/// copy, and a child that read Cloud out of one would be the promotion D258
/// withdrew wearing a different hat.
#[test]
fn a_workspace_under_an_unspoken_profile_still_starts_local_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);

    let state = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap();
    assert_eq!(state.mode, TransportMode::LocalOnly);
    assert!(!state.explicit_choice);

    // …and a Workspace made *after* the prompt is answered inherits it, which
    // is what keeps "new Workspace, pictures never sync" from being a thing a
    // person has to discover.
    store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();
    let later = store
        .create_workspace(profile, "made later", at())
        .unwrap()
        .id;
    let state = store
        .transport_mode(TransportSubject::Workspace(later))
        .unwrap();
    assert_eq!(state.mode, TransportMode::Cloud);
    assert!(!state.explicit_choice);
    assert!(
        store
            .admit_transport(TransportSubject::Workspace(later), TransportPath::Cloud)
            .is_ok(),
        "a Workspace made under a Cloud Profile can actually move bytes",
    );
}

#[test]
fn a_late_initializer_against_an_explicit_row_does_not_interrupt_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);
    raise_to_cloud(&mut store, profile, workspace);
    let subject = TransportSubject::Workspace(workspace);
    let generation = store.policy_generation();

    // Evidence arrives saying "this should default to Local-only". It cannot
    // touch an explicit row, so it must not close the fence either: the
    // evidence path runs on every pairing and subscription event, and a
    // transfer interrupted by an operation that was always a no-op is a bug
    // nobody would ever find.
    let outcome = store
        .initialize_transport_mode(InitializeTransportMode {
            subject,
            expected_revision: 2,
            mode: TransportMode::LocalOnly,
        })
        .unwrap();

    assert!(!outcome.wrote());
    let admission = store.transport_admission(subject).unwrap();
    assert!(!admission.fenced);
    assert_eq!(admission.mode, TransportMode::Cloud);
    assert_eq!(admission.generation, generation);
}

// --------------------------------------- B8 half one: initialization (D254)

/// A Profile row as a catalog stamped at schema 11 holds it: the projection
/// columns, and no mode row, because that table does not exist yet.
fn legacy_profile_row(connection: &rusqlite::Connection, profile: &Profile) {
    connection
        .execute(
            "INSERT INTO profiles
               (id, name, purpose, avatar, account, locale, created_at, icon, color, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                profile.id.to_storage(),
                profile.name,
                profile.purpose,
                profile.avatar,
                profile.account,
                profile.locale.as_str(),
                profile.created_at.millis(),
                profile.icon,
                profile.color,
                profile.kind.as_str(),
            ],
        )
        .unwrap();
}

fn legacy_workspace_row(connection: &rusqlite::Connection, workspace: &Workspace) {
    connection
        .execute(
            "INSERT INTO workspaces (id, owner, name, created_at, position, deleted_at)
             VALUES (?1, ?2, ?3, ?4, 0, NULL)",
            rusqlite::params![
                workspace.id.to_storage(),
                workspace.owner.to_storage(),
                workspace.name,
                workspace.created_at.millis(),
            ],
        )
        .unwrap();
}

/// A populated catalog stamped at schema **11** on disk, before anything in
/// this build has opened it.
fn populated_schema_eleven(
    directory: &std::path::Path,
) -> (Profile, Workspace, Workspace, Workspace) {
    let mut connection = crate::db::open(&directory.join("catalog.db")).unwrap();
    crate::db::migrate(
        &mut connection,
        "catalog.db",
        &crate::catalog::MIGRATIONS[..11],
        11,
    )
    .unwrap();
    let owner = Profile::new("Existing", Locale::default(), at()).unwrap();
    legacy_profile_row(&connection, &owner);
    let hosted = Workspace::new(owner.id, "Cloud enabled", at()).unwrap();
    let paired = Workspace::new(owner.id, "Paired only", at()).unwrap();
    let alone = Workspace::new(owner.id, "Nobody else", at()).unwrap();
    for workspace in [&hosted, &paired, &alone] {
        legacy_workspace_row(&connection, workspace);
    }
    (owner, hosted, paired, alone)
}

fn evidence(hosted: bool, peer: bool) -> SharingEvidence {
    SharingEvidence {
        hosted_enabled_with_account: hosted,
        peer_selected: peer,
    }
}

/// **The proof D254 asks for, end to end.** A catalog that existed before
/// migration 12, migrated by the ordinary open, and an existing Cloud-enabled
/// Workspace arriving at `cloud` rather than sitting at the seeded
/// `local_only` — which is the outage the gate would otherwise be turned on
/// into.
#[test]
fn an_existing_cloud_workspace_is_promoted_out_of_the_seeded_local_only() {
    let dir = tempfile::tempdir().unwrap();
    let (owner, hosted, paired, alone) = populated_schema_eleven(dir.path());

    let mut store = durable_store(dir.path());

    // Where every installation stands the moment migration 12 runs, and why
    // the gate cannot be switched on before the rest of this test.
    for subject in [
        TransportSubject::Profile(owner.id),
        TransportSubject::Workspace(hosted.id),
        TransportSubject::Workspace(paired.id),
        TransportSubject::Workspace(alone.id),
    ] {
        let state = store.transport_mode(subject).unwrap();
        assert_eq!(state.mode, TransportMode::LocalOnly, "{subject:?}");
        assert!(!state.explicit_choice);
        assert_eq!(state.revision, 1);
    }
    assert_eq!(
        store
            .admit_transport(TransportSubject::Workspace(hosted.id), TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::Forbidden,
        "the seeded default refuses Cloud, which is the outage half one prevents"
    );

    // The Agent's verified effects arrive.
    let committed = store
        .initialize_transport_modes_from_evidence(
            owner.id,
            evidence(true, true),
            &std::collections::BTreeMap::from([
                (hosted.id, evidence(true, false)),
                (paired.id, evidence(false, true)),
                (alone.id, evidence(false, false)),
            ]),
        )
        .unwrap();
    assert_eq!(
        committed.len(),
        3,
        "the Profile and the two Workspaces evidence moves: {committed:?}"
    );

    let cloud = store
        .transport_mode(TransportSubject::Workspace(hosted.id))
        .unwrap();
    assert_eq!(cloud.mode, TransportMode::Cloud);
    assert!(
        !cloud.explicit_choice,
        "a verified default is still a default: a person can still choose"
    );
    assert_eq!(cloud.revision, 2);
    assert_eq!(
        store
            .transport_mode(TransportSubject::Workspace(paired.id))
            .unwrap()
            .mode,
        TransportMode::P2p
    );
    assert_eq!(
        store
            .transport_mode(TransportSubject::Workspace(alone.id))
            .unwrap()
            .mode,
        TransportMode::LocalOnly,
        "no evidence promotes nothing"
    );
    // And the thing the gate is about: this Workspace may now reach Cloud.
    assert!(
        store
            .admit_transport(TransportSubject::Workspace(hosted.id), TransportPath::Cloud)
            .is_ok()
    );
    assert!(
        store
            .admit_transport(TransportSubject::Workspace(paired.id), TransportPath::Peer)
            .is_ok()
    );
    assert_eq!(
        store
            .admit_transport(TransportSubject::Workspace(paired.id), TransportPath::Cloud)
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
}

/// The same evidence twice writes once. The Agent's effects fire on every
/// pairing, subscription and selection event, so this is the ordinary path
/// rather than an edge case — and a revision that advanced on each one would
/// spend the compare-and-set token on nothing.
#[test]
fn a_second_pass_of_the_same_evidence_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (owner, hosted, ..) = populated_schema_eleven(dir.path());
    let mut store = durable_store(dir.path());
    let workspaces = std::collections::BTreeMap::from([(hosted.id, evidence(true, false))]);

    store
        .initialize_transport_modes_from_evidence(owner.id, evidence(true, false), &workspaces)
        .unwrap();
    let generation = store.policy_generation();
    let again = store
        .initialize_transport_modes_from_evidence(owner.id, evidence(true, false), &workspaces)
        .unwrap();

    assert!(again.is_empty(), "{again:?}");
    assert_eq!(
        store
            .transport_mode(TransportSubject::Workspace(hosted.id))
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        store.policy_generation(),
        generation,
        "a pass that writes nothing invalidates nothing"
    );
}

/// A choice made on this device before the evidence arrived stands, and the
/// evidence still promotes the sibling it is allowed to.
#[test]
fn an_explicit_choice_survives_the_initializer_beside_a_promoted_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let (owner, hosted, paired, _) = populated_schema_eleven(dir.path());
    let mut store = durable_store(dir.path());
    store
        .set_transport_mode(set(
            TransportSubject::Workspace(hosted.id),
            1,
            TransportMode::LocalOnly,
        ))
        .unwrap();

    store
        .initialize_transport_modes_from_evidence(
            owner.id,
            evidence(true, true),
            &std::collections::BTreeMap::from([
                (hosted.id, evidence(true, true)),
                (paired.id, evidence(true, false)),
            ]),
        )
        .unwrap();

    let chosen = store
        .transport_mode(TransportSubject::Workspace(hosted.id))
        .unwrap();
    assert_eq!(chosen.mode, TransportMode::LocalOnly);
    assert!(chosen.explicit_choice);
    assert_eq!(
        store
            .transport_mode(TransportSubject::Workspace(paired.id))
            .unwrap()
            .mode,
        TransportMode::Cloud
    );
}

/// A Workspace initializer **alone** cannot promote anything, because the
/// Profile row migration 12 seeded is the ceiling. This is the shape that
/// reports a successful write and leaves the installation Local-only, and it
/// is why the evidence entry point submits the Profile with its children.
#[test]
fn a_workspace_initializer_without_its_profile_is_capped_at_the_seeded_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let (_, hosted, ..) = populated_schema_eleven(dir.path());
    let mut store = durable_store(dir.path());

    let outcome = store
        .initialize_transport_mode(InitializeTransportMode {
            subject: TransportSubject::Workspace(hosted.id),
            expected_revision: 1,
            mode: TransportMode::Cloud,
        })
        .unwrap();

    // Not an error — the caller is told the row is already what it asked for,
    // because `min(Cloud, LocalOnly)` *is* what it asked for once the ceiling
    // has been applied. A caller that checks only `is_ok()` reads this as a
    // Workspace now on Cloud.
    assert!(matches!(outcome, TransportModeCommit::AlreadyCurrent(_)));
    assert_eq!(
        outcome.state().mode,
        TransportMode::LocalOnly,
        "capped at the seeded Profile, which is exactly why the group entry point exists"
    );
}

// ------------------------------------------------- the Profile prompt (D258)

/// Three Workspaces under one Profile, every row as migration 12 seeds it.
fn seeded_profile(store: &mut RealStore) -> (ProfileId, [WorkspaceId; 3]) {
    let profile: Profile = store
        .create_profile("Work", None, None, Locale::default(), at())
        .unwrap();
    let children = [
        store.create_workspace(profile.id, "one", at()).unwrap().id,
        store.create_workspace(profile.id, "two", at()).unwrap().id,
        store
            .create_workspace(profile.id, "three", at())
            .unwrap()
            .id,
    ];
    for subject in [
        TransportSubject::Profile(profile.id),
        TransportSubject::Workspace(children[0]),
    ] {
        let row = store.transport_mode(subject).unwrap();
        assert_eq!(row.mode, TransportMode::LocalOnly);
        assert!(!row.explicit_choice, "a seeded row is nobody's choice");
    }
    (profile.id, children)
}

/// **The prompt's "yes".** A Profile and every Workspace beneath it arrive at
/// Cloud together, as explicit choices, in one group — which is what "then
/// move" means and is the only thing that could make a Workspace's own Cloud
/// row mean anything, because `plan` caps a child at its Profile.
#[test]
fn answering_yes_moves_the_profile_and_its_workspaces_in_one_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, children) = seeded_profile(&mut store);

    let choice = store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();

    assert!(choice.profile.wrote());
    assert_eq!(choice.workspaces_submitted, 3);
    assert_eq!(choice.workspaces_moved, 3);
    assert_eq!(choice.workspaces_left_alone, 0);

    // **Through the Store, not through what the caller was told.** The rows
    // are the durable fact; the outcome is a report about them.
    let owner = store
        .transport_mode(TransportSubject::Profile(profile))
        .unwrap();
    assert_eq!(owner.mode, TransportMode::Cloud);
    assert!(owner.explicit_choice);
    for workspace in children {
        let subject = TransportSubject::Workspace(workspace);
        let row = store.transport_mode(subject).unwrap();
        assert_eq!(row.mode, TransportMode::Cloud);
        assert!(row.explicit_choice, "the prompt is a choice, not a default");
        // The effective answer, which is the only value a transport may
        // branch on: nothing is capped, so bytes may actually move.
        let admission = store.transport_admission(subject).unwrap();
        assert_eq!(admission.mode, TransportMode::Cloud);
        assert!(!admission.ceiling_mismatch);
        assert!(
            store.admit_transport(subject, TransportPath::Cloud).is_ok(),
            "answering yes is what lets a picture leave the device",
        );
    }
}

/// **The prompt's "no".** The Profile moves alone and its Workspaces stay
/// where they are, capped beneath it.
///
/// That is a legitimate state and not an error: the ceiling is what makes it
/// safe, and the person gets exactly what they said — documents under this
/// Profile may use Cloud, its pictures wait.
#[test]
fn answering_no_moves_the_profile_alone_and_leaves_its_children_capped() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, children) = seeded_profile(&mut store);

    let choice = store
        .choose_transport_mode(profile, TransportMode::Cloud, false)
        .unwrap();

    assert!(choice.profile.wrote());
    assert_eq!(choice.workspaces_submitted, 0);
    assert_eq!(choice.workspaces_moved, 0);
    assert_eq!(choice.workspaces_left_alone, 0);

    assert_eq!(
        store
            .transport_mode(TransportSubject::Profile(profile))
            .unwrap()
            .mode,
        TransportMode::Cloud,
    );
    for workspace in children {
        let subject = TransportSubject::Workspace(workspace);
        let row = store.transport_mode(subject).unwrap();
        assert_eq!(row.mode, TransportMode::LocalOnly);
        assert!(!row.explicit_choice, "nobody chose for the children");
        let admission = store.transport_admission(subject).unwrap();
        assert_eq!(admission.mode, TransportMode::LocalOnly);
        assert_eq!(admission.profile_mode, TransportMode::Cloud);
        assert!(
            !admission.ceiling_mismatch,
            "a child below its Profile is the ordinary shape, not damage",
        );
        assert_eq!(
            store
                .admit_transport(subject, TransportPath::Cloud)
                .unwrap_err()
                .code(),
            Code::Forbidden,
            "the pictures wait, and the refusal has a name",
        );
    }
}

/// **An explicit prior choice survives the prompt.**
///
/// One Workspace was deliberately set Local-only before anybody was asked
/// about the Profile. Answering *move everything* raises its siblings and
/// leaves it exactly where its owner put it — and says so, because a
/// Workspace left alone is not a Workspace that failed.
#[test]
fn an_explicit_workspace_choice_survives_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, children) = seeded_profile(&mut store);
    let refused = children[1];

    // Deliberate, and at the mode the seed already reads — which is still a
    // choice, and still pins the row.
    let before = store
        .transport_mode(TransportSubject::Workspace(refused))
        .unwrap();
    store
        .set_transport_mode(set(
            TransportSubject::Workspace(refused),
            before.revision,
            TransportMode::LocalOnly,
        ))
        .unwrap();
    let pinned = store
        .transport_mode(TransportSubject::Workspace(refused))
        .unwrap();
    assert!(pinned.explicit_choice);

    let choice = store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();
    assert_eq!(choice.workspaces_submitted, 2);
    assert_eq!(choice.workspaces_moved, 2);
    assert_eq!(
        choice.workspaces_left_alone, 1,
        "the one somebody had already decided about",
    );

    let after = store
        .transport_mode(TransportSubject::Workspace(refused))
        .unwrap();
    assert_eq!(after.mode, TransportMode::LocalOnly);
    assert!(after.explicit_choice);
    assert_eq!(
        after.revision, pinned.revision,
        "left out of the group entirely, not written with the same value",
    );
    for sibling in [children[0], children[2]] {
        assert_eq!(
            store
                .transport_mode(TransportSubject::Workspace(sibling))
                .unwrap()
                .mode,
            TransportMode::Cloud,
        );
    }
}

/// Answering the same thing twice writes nothing and is still a success — a
/// retry costs bytes, never correctness.
#[test]
fn answering_the_same_thing_twice_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, children) = seeded_profile(&mut store);

    store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();
    let revisions: Vec<i64> = children
        .iter()
        .map(|workspace| {
            store
                .transport_mode(TransportSubject::Workspace(*workspace))
                .unwrap()
                .revision
        })
        .collect();

    let again = store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();
    assert!(!again.profile.wrote());
    assert_eq!(again.workspaces_moved, 0);
    assert_eq!(
        again.workspaces_left_alone, 3,
        "they are explicit now, so the group leaves them alone",
    );
    for (workspace, revision) in children.iter().zip(revisions) {
        assert_eq!(
            store
                .transport_mode(TransportSubject::Workspace(*workspace))
                .unwrap()
                .revision,
            revision,
        );
    }
}

/// **A1 §5, through the prompt.** A write that cannot land claims nothing,
/// leaves the guard closed, and does not report a choice that is not on the
/// disk — and a restart reads the last mode that actually committed.
#[test]
fn a_failed_prompt_write_claims_nothing_and_holds_the_guard_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, children) = seeded_profile(&mut store);
    store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();

    refuse_transport_writes(dir.path());
    let error = store
        .choose_transport_mode(profile, TransportMode::LocalOnly, true)
        .unwrap_err();
    assert_eq!(error.code(), Code::StorageFailed);

    // Closed now, on the strength of the intent rather than the write.
    for subject in [
        TransportSubject::Profile(profile),
        TransportSubject::Workspace(children[0]),
    ] {
        let admission = store.transport_admission(subject).unwrap();
        assert!(admission.fenced);
        assert_eq!(admission.mode, TransportMode::LocalOnly);
    }

    // …and nothing durable was claimed: a restart reads what committed.
    drop(store);
    let store = durable_store(dir.path());
    assert_eq!(
        store
            .transport_mode(TransportSubject::Profile(profile))
            .unwrap()
            .mode,
        TransportMode::Cloud,
        "a failed write must not look like a saved one",
    );
}

// ------------------------------------- linking is the choice (D258 amended)

use koru_store_core::root_catalog::CatalogUnitRef;
use koru_store_core::service::RootGrant;

/// An `all_data` peer's grant: the same person's other device, over the peer
/// path. It is what a linked phone and a linked desktop ask each other with.
fn peer_grant() -> RootGrant<'static> {
    RootGrant {
        path: TransportPath::Peer,
        ..RootGrant::account()
    }
}

/// Join this device to an account, exactly as sign-in does. This is the act
/// the seeded default now reads — not the platform, and not evidence.
fn link_to_account(store: &mut RealStore) {
    store
        .bind_account_scope(
            koru_store_core::ids::UserScopeId::new().as_uuid(),
            koru_store_core::ids::UserScopeId::new().as_uuid(),
            1,
        )
        .unwrap();
}

fn unit_listed(store: &RealStore, unit: CatalogUnitRef) -> bool {
    store
        .root_export_inventory(&peer_grant())
        .unwrap()
        .units
        .iter()
        .any(|(listed, _)| *listed == unit)
}

/// The seed is read when the row is written, so a restart must find the
/// committed value and not re-decide it.
fn still_p2p_after_restart(directory: &std::path::Path, profile: ProfileId, ws: WorkspaceId) {
    let reopened = durable_store(directory);
    for subject in [
        TransportSubject::Profile(profile),
        TransportSubject::Workspace(ws),
    ] {
        assert_eq!(
            reopened.transport_mode(subject).unwrap().mode,
            TransportMode::P2p,
            "{subject:?}",
        );
    }
}

/// **The phone case.** Nothing on a phone can answer the Profile prompt, so
/// before this a Workspace created there was withheld for ever and its root
/// unit could never reach the person's other devices. Linking is the choice:
/// a subject created afterwards admits the peer path with nobody answering
/// anything.
#[test]
fn a_subject_created_on_a_linked_device_admits_peers_with_no_prompt_answered() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    link_to_account(&mut store);

    let (profile, workspace) = profile_and_workspace(&mut store);

    for subject in [
        TransportSubject::Profile(profile),
        TransportSubject::Workspace(workspace),
    ] {
        let admission = store.transport_admission(subject).unwrap();
        assert_eq!(admission.mode, TransportMode::P2p, "{subject:?}");
        assert!(
            !admission.explicit_choice,
            "{subject:?} was seeded, not chosen",
        );
        assert!(!admission.ceiling_mismatch, "{subject:?}");
        assert!(
            store.admit_transport(subject, TransportPath::Peer).is_ok(),
            "{subject:?} refuses the peer path",
        );
        // Linking two of the person's own devices is not consent to Cloud.
        assert_eq!(
            store
                .admit_transport(subject, TransportPath::Cloud)
                .unwrap_err()
                .code(),
            Code::Forbidden,
            "{subject:?}",
        );
    }

    // And the root unit — the foundation this was about — actually leaves.
    let unit = CatalogUnitRef::Workspace { workspace };
    assert!(
        store
            .eligible_export_source(unit, TransportPath::Peer)
            .unwrap()
            .is_some(),
    );
    assert!(unit_listed(&store, unit));
    assert_eq!(
        store.root_export_inventory(&peer_grant()).unwrap().held,
        0,
        "a linked device withheld a unit anyway"
    );
    assert!(
        store
            .exportable_profiles(&peer_grant())
            .unwrap()
            .contains(&profile)
    );
    still_p2p_after_restart(dir.path(), profile, workspace);
}

/// **Nothing the person already has is promoted.** Linking says what to do
/// with subjects made from now on; reaching back over the existing rows would
/// reverse D258 for data that was created under it.
#[test]
fn a_subject_created_before_linking_keeps_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    let (profile, workspace) = profile_and_workspace(&mut store);

    link_to_account(&mut store);

    for subject in [
        TransportSubject::Profile(profile),
        TransportSubject::Workspace(workspace),
    ] {
        let state = store.transport_mode(subject).unwrap();
        assert_eq!(state.mode, TransportMode::LocalOnly, "{subject:?}");
        assert_eq!(state.revision, 1, "{subject:?} was rewritten");
        assert_eq!(
            store
                .admit_transport(subject, TransportPath::Peer)
                .unwrap_err()
                .code(),
            Code::Forbidden,
            "{subject:?}",
        );
    }
    let unit = CatalogUnitRef::Workspace { workspace };
    assert_eq!(
        store
            .eligible_export_source(unit, TransportPath::Peer)
            .unwrap(),
        None
    );

    // A Workspace created after linking under that same unpromoted Profile is
    // **capped at it** rather than seeded above it: a child over its ceiling
    // is a `ceiling_mismatch`, which stops the transport rather than opening
    // it. The Profile prompt is still the way out for rows that predate the
    // linking, and on a phone that surface does not exist yet.
    let later = store.create_workspace(profile, "after", at()).unwrap().id;
    let state = store
        .transport_mode(TransportSubject::Workspace(later))
        .unwrap();
    assert_eq!(state.mode, TransportMode::LocalOnly);
    assert!(
        !store
            .transport_admission(TransportSubject::Workspace(later))
            .unwrap()
            .ceiling_mismatch,
    );
}

/// **Turning one subject off still turns it all the way off**, which is the
/// fix that landed before this and must not be weakened: a Local-only subject
/// is not described to a peer at all — not its unit, not its name, not its
/// placement.
#[test]
fn a_local_only_choice_on_a_linked_device_still_hides_the_subject_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    link_to_account(&mut store);
    let (profile, workspace) = profile_and_workspace(&mut store);
    let unit = CatalogUnitRef::Workspace { workspace };
    let profile_unit = CatalogUnitRef::Profile { profile };
    assert!(unit_listed(&store, unit));

    // The person turns this Workspace off.
    store
        .set_transport_mode(set(
            TransportSubject::Workspace(workspace),
            1,
            TransportMode::LocalOnly,
        ))
        .unwrap();

    let state = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap();
    assert_eq!(state.mode, TransportMode::LocalOnly);
    assert!(state.explicit_choice, "a choice, outranking the seed");
    for path in [TransportPath::Peer, TransportPath::Cloud] {
        assert_eq!(
            store.eligible_export_source(unit, path).unwrap(),
            None,
            "{path:?}"
        );
    }
    assert!(!unit_listed(&store, unit), "the withheld unit was listed");

    // Turning the **Profile** off hides its own unit and its name with it: it
    // is in no exportable set, so a unit naming it elsewhere is redacted.
    store
        .set_transport_mode(set(
            TransportSubject::Profile(profile),
            1,
            TransportMode::LocalOnly,
        ))
        .unwrap();
    assert!(
        !store
            .exportable_profiles(&peer_grant())
            .unwrap()
            .contains(&profile),
    );
    assert!(!unit_listed(&store, profile_unit));
    assert_eq!(
        store
            .eligible_export_source(profile_unit, TransportPath::Peer)
            .unwrap(),
        None
    );
}

/// The desktop's prompt is untouched: it still commits the Profile and its
/// children as one group, over the linkage seed as readily as over Local-only.
#[test]
fn the_profile_prompt_still_raises_a_linked_device_to_cloud_in_one_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = durable_store(dir.path());
    link_to_account(&mut store);
    let (profile, workspace) = profile_and_workspace(&mut store);
    let chosen = store.create_workspace(profile, "mine", at()).unwrap().id;
    store
        .set_transport_mode(set(
            TransportSubject::Workspace(chosen),
            1,
            TransportMode::LocalOnly,
        ))
        .unwrap();

    let answer = store
        .choose_transport_mode(profile, TransportMode::Cloud, true)
        .unwrap();

    assert!(answer.profile.wrote());
    assert_eq!(answer.workspaces_moved, 1, "the seeded default came along");
    assert_eq!(
        answer.workspaces_left_alone, 1,
        "the Workspace the person chose for was left alone"
    );
    for subject in [
        TransportSubject::Profile(profile),
        TransportSubject::Workspace(workspace),
    ] {
        let admission = store.transport_admission(subject).unwrap();
        assert_eq!(admission.mode, TransportMode::Cloud, "{subject:?}");
        assert!(admission.explicit_choice, "{subject:?}");
    }
    assert_eq!(
        store
            .transport_admission(TransportSubject::Workspace(chosen))
            .unwrap()
            .mode,
        TransportMode::LocalOnly,
    );
}
