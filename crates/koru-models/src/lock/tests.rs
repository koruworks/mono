// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::mpsc;
use std::time::Duration;

/// The exclusive side excludes, and the shared side does not exclude itself.
/// Both halves matter: an install must be alone, and any number of workers must
/// be able to take a lease at once.
#[test]
fn exclusive_excludes_and_shared_shares() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locks").join("digest.install");

    let held = InstallLock::exclusive(&path).unwrap();
    let (send, receive) = mpsc::channel();
    let racing = path.clone();
    let waiting = std::thread::spawn(move || {
        let lock = InstallLock::exclusive(&racing).unwrap();
        let _ = send.send(());
        drop(lock);
    });
    assert!(
        receive.recv_timeout(Duration::from_millis(200)).is_err(),
        "a second exclusive lock was granted while the first was held"
    );
    drop(held);
    receive
        .recv_timeout(Duration::from_secs(5))
        .expect("the waiter never woke");
    waiting.join().unwrap();

    let one = InstallLock::shared(&path).unwrap();
    let two = InstallLock::shared(&path).unwrap();
    drop((one, two));
}

/// **The whole of crash recovery, and it needs no clock.** A lease file whose
/// holder died is one nothing holds the lock on, which is exactly what a file
/// written by hand looks like — so this is the same test as *a killed worker's
/// lease is reclaimed*, without killing a worker.
#[test]
fn a_lease_whose_holder_died_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let leases = dir.path().join("leases");
    fs::create_dir_all(&leases).unwrap();
    let orphan = leases.join("4242-0.lease");
    fs::write(&orphan, b"").unwrap();

    assert_eq!(live_leases(&leases), 0, "a dead holder is not a live one");
    assert!(!orphan.exists(), "the debris was not reclaimed");
}

/// A live lease is counted and is not reclaimed, which is the half that keeps
/// somebody's dictation alive through another channel's `prune`.
#[test]
fn a_live_lease_is_counted_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let leases = dir.path().join("leases");
    let held = Lease::take(&leases).unwrap();
    assert_eq!(live_leases(&leases), 1);

    // Two holders of one model is an ordinary state — two dictations in one
    // worker — so a second lease takes a second name rather than failing.
    let second = Lease::take(&leases).unwrap();
    assert_eq!(live_leases(&leases), 2);

    drop(second);
    assert_eq!(live_leases(&leases), 1);
    drop(held);
    assert_eq!(live_leases(&leases), 0);
    assert_eq!(
        fs::read_dir(&leases).unwrap().count(),
        0,
        "dropping a lease leaves its file behind"
    );
}

/// Staging debris from a process that died mid-install is swept; a live
/// installer's staging is not. The difference is the `.busy` lock and nothing
/// else — no timeout, no pid.
#[test]
fn staging_debris_is_swept_and_a_live_installer_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");

    let live = Staging::create(&staging, "abc").unwrap();
    fs::create_dir_all(live.dir()).unwrap();
    fs::write(live.dir().join("model.bin"), b"in progress").unwrap();

    // A dead installer: the directory and its `.busy` are there, and nothing
    // holds the lock.
    let dead = staging.join("dead-1-1");
    fs::create_dir_all(&dead).unwrap();
    fs::write(dead.join("model.bin"), b"abandoned").unwrap();
    fs::write(staging.join("dead-1-1.busy"), b"").unwrap();

    // And one that died between creating the directory and claiming it.
    let unclaimed = staging.join("unclaimed-1-1");
    fs::create_dir_all(&unclaimed).unwrap();

    sweep_staging(&staging);

    assert!(live.dir().is_dir(), "a live installer's staging was swept");
    assert!(!dead.exists(), "debris survived the sweep");
    assert!(!staging.join("dead-1-1.busy").exists());
    assert!(!unclaimed.exists(), "an unclaimed directory has no owner");
}

/// Releasing hands the directory on; dropping deletes it. `publish` relies on
/// the first and every failure path relies on the second.
#[test]
fn releasing_keeps_the_directory_and_dropping_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");

    let kept = Staging::create(&staging, "kept").unwrap();
    let kept_dir = kept.dir().to_path_buf();
    fs::create_dir_all(&kept_dir).unwrap();
    kept.release();
    assert!(kept_dir.is_dir());

    let dropped = Staging::create(&staging, "dropped").unwrap();
    let dropped_dir = dropped.dir().to_path_buf();
    fs::create_dir_all(&dropped_dir).unwrap();
    fs::write(dropped_dir.join("half"), b"written").unwrap();
    drop(dropped);
    assert!(!dropped_dir.exists());
}
