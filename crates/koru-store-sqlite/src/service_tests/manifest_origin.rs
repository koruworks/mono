// SPDX-License-Identifier: MPL-2.0

//! **Exactly one device ever creates a given manifest. Every other device
//! receives it.**
//!
//! Loro converges one document's history; it cannot tell you that two devices
//! created the same document. Two independently created identity manifests for
//! one Profile merge without complaint — last-writer-wins simply picks one
//! `created_at`, a field the contract calls immutable, and from then on neither
//! device can reconcile with the other in either direction. Document identity
//! is therefore established by *who created it*, which is outside the CRDT and
//! cannot be delegated to it.
//!
//! Until this file the invariant was held by reading `service.rs`, which is
//! exactly how it was broken before: `apply_profile_unit_updates(LegacyPrivate
//! { .. })` writes a peer's document into `profile_crdts`, and both legacy
//! upgrades read a row in that table as proof of local origin. These tests walk
//! the three real paths — `Store::open`'s upgrade, `restore_profile_local`, and
//! provisioning followed by a restart — and fail if any of them mints a
//! manifest for an id this installation was only told about.
//!
//! The restarts are real: [`SqliteCatalog::shared_in_memory`] keeps one named
//! database alive across `Store`s, so a second open reads the bytes the first
//! one actually wrote rather than a hand-built imitation of them. Nothing here
//! touches a disk or a Workspace.

use super::*;
use koru_store_core::errors::{Code, Result};
use koru_store_core::ports::{Catalog, LocalProfileRemoval, LocalProfileRestoration};
use koru_store_core::profile_manifest::ProfileManifest;
use koru_store_core::profile_units::ProfileUnitRef;
use koru_store_core::service::Backend;
use koru_store_core::{Profile, ProfileAccountBinding, ProfileId};

/// A Profile's origin is decided without any Workspace being opened, so this
/// backend turns "it opened one" into a failure rather than a silent cost.
struct NoWorkspaceIo;
impl Backend for NoWorkspaceIo {
    type Workspace = crate::SqliteWorkspace;
    type Blobs = crate::blobs::SqliteBlobs;
    fn open_workspace(&self, _: WorkspaceId) -> Result<Self::Workspace> {
        panic!("Profile origin never opens a Workspace")
    }
    fn delete_workspace(&self, _: WorkspaceId) -> Result<()> {
        panic!("Profile origin never deletes a Workspace")
    }
    fn objects(&self) -> Result<Self::Blobs> {
        panic!("Profile origin never opens blobs")
    }
}

fn binding() -> ProfileAccountBinding {
    ProfileAccountBinding::parse_identity_id("10000000-0000-4000-8000-000000000001").unwrap()
}

/// The origin device's clock, hours before this device ever heard of the
/// Profile. A lineage minted here would carry [`at`] instead, and that single
/// difference is what makes the two documents unmergeable forever.
fn originated_at() -> Timestamp {
    Timestamp::from_millis(at().millis() - 6 * 60 * 60 * 1000)
}

/// A legacy `ProfileManifest` as some *other* device authored it, carrying that
/// device's creation time — the shape that reaches this installation through
/// `apply_profile_unit_updates(LegacyPrivate { .. })`.
fn peers_legacy_document(id: ProfileId) -> Vec<u8> {
    let mut profile = Profile::new("Work", Locale::default(), originated_at()).unwrap();
    profile.id = id;
    ProfileManifest::create(&profile)
        .unwrap()
        .export_snapshot()
        .unwrap()
}

fn open(name: &str) -> Store<SqliteCatalog, NoWorkspaceIo> {
    Store::open(
        SqliteCatalog::shared_in_memory(name).unwrap(),
        NoWorkspaceIo,
    )
    .unwrap()
}

/// Asked of the database directly rather than through the `Store`, because a
/// locally removed Profile refuses every service-level read and the question
/// "were bytes minted" still has to be answerable about one.
fn identity_bytes_exist(name: &str, id: ProfileId) -> bool {
    SqliteCatalog::shared_in_memory(name)
        .unwrap()
        .profile_unit(ProfileUnitRef::Identity { profile: id })
        .unwrap()
        .is_some()
}

/// The maintainers' account, reproduced. A device is told a Profile exists, a peer's
/// legacy document arrives, the device restarts — and it must still be waiting
/// to receive an identity unit rather than holding one of its own.
#[test]
fn a_profile_this_device_was_only_told_about_never_mints_an_identity_across_a_restart() {
    const DB: &str = "koru-origin-told-about";
    let keeper = SqliteCatalog::shared_in_memory(DB).unwrap();
    let id = ProfileId::new();

    let mut store = open(DB);
    let told = store
        .provision_account_profile(id, binding(), "Work", Locale::default(), at())
        .unwrap();
    assert_eq!(told.id, id);
    // The placeholder stamps this device's clock, which is a fact about this
    // device. Nothing may promote it into a manifest.
    assert_eq!(told.created_at, at());
    assert!(!identity_bytes_exist(DB, id));

    store
        .apply_profile_unit_updates(
            ProfileUnitRef::LegacyPrivate { profile: id },
            &[peers_legacy_document(id)],
        )
        .unwrap();
    assert!(!identity_bytes_exist(DB, id));
    drop(store);

    for _restart in 0..2 {
        let mut store = open(DB);
        assert!(
            !identity_bytes_exist(DB, id),
            "a legacy document received from a peer is not authority to author one",
        );
        assert_eq!(
            store.profile_version(id).unwrap_err().code,
            Code::VersionUnavailable,
            "the device waits to receive identity bytes; that is what receipt means",
        );
        drop(store);
    }
    drop(keeper);
}

/// The other half of the same rule: a Profile genuinely created here is not
/// held up by the guard, and stays readable across a restart.
#[test]
fn a_profile_created_here_records_its_origin_and_keeps_it_across_a_restart() {
    const DB: &str = "koru-origin-created-here";
    let keeper = SqliteCatalog::shared_in_memory(DB).unwrap();

    let mut store = open(DB);
    let profile = store
        .create_profile("Mine", None, None, Locale::default(), at())
        .unwrap();
    assert!(identity_bytes_exist(DB, profile.id));
    drop(store);

    let mut store = open(DB);
    assert!(identity_bytes_exist(DB, profile.id));
    assert_eq!(store.profile(profile.id).unwrap().name, "Mine");
    assert!(store.profile_version(profile.id).is_ok());
    drop(store);
    drop(keeper);
}

/// `restore_profile_local` holds the same guard as `open`, and refuses rather
/// than skipping, because a person asked for this one and is owed an answer.
#[test]
fn restoring_a_profile_this_device_was_told_about_refuses_to_mint_from_legacy_bytes() {
    const DB: &str = "koru-origin-restore";
    let keeper = SqliteCatalog::shared_in_memory(DB).unwrap();
    let id = ProfileId::new();

    let mut store = open(DB);
    store
        .provision_account_profile(id, binding(), "Work", Locale::default(), at())
        .unwrap();
    store
        .apply_profile_unit_updates(
            ProfileUnitRef::LegacyPrivate { profile: id },
            &[peers_legacy_document(id)],
        )
        .unwrap();
    let mark = store
        .begin_profile_removal(
            id,
            koru_store_core::profile_removal::RemovalKind::Local,
            at(),
        )
        .unwrap();
    assert_eq!(
        store
            .remove_profile_local(id, Some(mark.attempt), at())
            .unwrap(),
        LocalProfileRemoval::Removed,
    );

    assert_eq!(
        store.restore_profile_local(id).unwrap_err().code,
        Code::VersionUnavailable,
    );
    assert!(!identity_bytes_exist(DB, id));
    drop(store);

    // And the refusal is not a one-time in-memory state either.
    let mut store = open(DB);
    assert_eq!(
        store.restore_profile_local(id).unwrap_err().code,
        Code::VersionUnavailable,
    );
    assert!(!identity_bytes_exist(DB, id));
    drop(store);
    drop(keeper);
}

/// The one-time grandfather, and its boundary.
///
/// A legacy row already on the disk when markers begin has no recorded
/// provenance, and refusing it would strand a device that genuinely originated
/// its Profile offline — so it is upgraded once, carrying the creation time the
/// legacy document already held rather than inventing one. A legacy row that
/// arrives *after* markers begin is a receipt, and is never upgraded.
#[test]
fn legacy_rows_are_grandfathered_once_and_never_again() {
    const DB: &str = "koru-origin-grandfather";
    let keeper = SqliteCatalog::shared_in_memory(DB).unwrap();
    let older = ProfileId::new();
    let later = ProfileId::new();

    // Written by a build that kept no markers, before this one ever ran.
    {
        let mut catalog = SqliteCatalog::shared_in_memory(DB).unwrap();
        let mut profile = Profile::new("Work", Locale::default(), originated_at()).unwrap();
        profile.id = older;
        let legacy = ProfileManifest::create(&profile).unwrap();
        catalog
            .commit_profile_crdt(&profile, &legacy.export_snapshot().unwrap())
            .unwrap();
    }

    let mut store = open(DB);
    assert!(
        identity_bytes_exist(DB, older),
        "an offline originator must not be stranded by the guard",
    );
    assert_eq!(
        store.profile(older).unwrap().created_at,
        originated_at(),
        "the upgrade carries the legacy document's own creation time",
    );

    // Markers have begun. The next legacy document is a receipt.
    store
        .provision_account_profile(later, binding(), "Work", Locale::default(), at())
        .unwrap();
    store
        .apply_profile_unit_updates(
            ProfileUnitRef::LegacyPrivate { profile: later },
            &[peers_legacy_document(later)],
        )
        .unwrap();
    drop(store);

    let store = open(DB);
    assert!(identity_bytes_exist(DB, older));
    assert!(
        !identity_bytes_exist(DB, later),
        "grandfathering is one pass wide; nothing arriving afterwards is covered",
    );
    drop(store);
    drop(keeper);
}

/// The grandfather reaches Profiles the visible listing does not.
///
/// A locally removed Profile is hidden from `profiles()` and so is never
/// upgraded by `open` — `restore_profile_local` is the only path that upgrades
/// it, and it runs long after markers began. Enumerating only visible rows
/// would therefore make a Profile this installation originated offline and then
/// hid unrestorable for good, which is the one failure this whole change exists
/// to avoid.
#[test]
fn a_locally_removed_legacy_profile_is_grandfathered_and_still_restores() {
    const DB: &str = "koru-origin-removed";
    let keeper = SqliteCatalog::shared_in_memory(DB).unwrap();
    let id = ProfileId::new();

    // Originated here by a build that kept no markers, then hidden.
    {
        let mut catalog = SqliteCatalog::shared_in_memory(DB).unwrap();
        let mut profile = Profile::new("Hidden", Locale::default(), originated_at()).unwrap();
        profile.id = id;
        let legacy = ProfileManifest::create(&profile).unwrap();
        catalog
            .commit_profile_crdt(&profile, &legacy.export_snapshot().unwrap())
            .unwrap();
        let mark = catalog
            .update_profile_removal(
                id,
                koru_store_core::profile_removal::RemovalUpdate::Begin {
                    kind: koru_store_core::profile_removal::RemovalKind::Local,
                    at: at(),
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog
                .remove_profile_local(id, Some(mark.attempt), at())
                .unwrap(),
            LocalProfileRemoval::Removed,
        );
    }

    let mut store = open(DB);
    // `open` leaves it alone — a hidden Profile is not in the visible listing.
    assert!(!identity_bytes_exist(DB, id));
    assert_eq!(
        store.restore_profile_local(id).unwrap(),
        LocalProfileRestoration::Restored,
    );
    assert!(identity_bytes_exist(DB, id));
    assert_eq!(store.profile(id).unwrap().created_at, originated_at());
    drop(store);
    drop(keeper);
}
