// SPDX-License-Identifier: MPL-2.0

//! **Ownership of a cache that has more than one writer.**
//!
//! Channel-independence is what creates this problem. A released build and a
//! dev build share one model directory, so a process-local *currently loaded*
//! flag cannot see the other one's `prune`, and a process-local *currently
//! installing* flag cannot see the other one's download. Every guarantee in
//! Plan 1.2 that is not about bytes is about this file.
//!
//! ## The idiom is `koru_update::lease`'s, deliberately
//!
//! `InstanceLease` already answers *is another process using this* with an
//! advisory file lock and a directory of per-process files, and
//! `other_instances` already reclaims a dead holder's by taking the lock its
//! owner would still be holding. The mechanism is copied rather than invented,
//! because a second answer to a question the repository has already answered is
//! the shape [PRINCIPLES](../../../guide/contributing/PRINCIPLES.md) argues against. What is
//! different is the key: an update lock is per *installation*, and every lock
//! here is per **digest**, because two people installing two different models
//! have nothing to say to each other.
//!
//! ## Three locks, and what each one excludes
//!
//! | | |
//! | --- | --- |
//! | [`InstallLock`] | exclusive, per digest. Held for a whole install, and by `remove`/`prune` while they delete. |
//! | [`InstallLock::shared`] | the same file, shared. Held for the instant a lease is taken, which is what closes the window between *is it installed* and *nothing may delete it*. |
//! | [`Lease`] | one file per live holder, exclusively locked for as long as the model is loaded. |
//!
//! **A lock that would block is taken blocking**, not refused. An install is
//! already minutes of work on a thread that answers nobody, and two workers
//! reaching for the same 650 MB should produce one download and one wait —
//! which is exactly what a blocking exclusive lock produces, because the second
//! one finds the model installed the moment it wakes.
//!
//! ## Reclaiming a dead holder, and why it needs no clock
//!
//! An OS releases every advisory lock a process held when the process ends,
//! however it ended. So *the holder is dead* is answerable by trying to take
//! the lock: success means nobody holds it, and the file is debris. There is no
//! timeout, no heartbeat and no stored pid to compare against a process table —
//! all three of which are wrong on a machine whose clock moved, and the last of
//! which is wrong on a machine that reused a pid.
//!
//! The same trick sweeps staging: a staging directory has a `.busy` file beside
//! it that its installer holds locked, so a directory whose `.busy` unlocks is
//! debris from a process that died mid-install.

use crate::error::{ModelError, ModelErrorCode};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

/// Open (creating it and its parent) a file to lock. Never truncating: the
/// content is irrelevant and truncating one would be a write a reader could
/// race.
fn open_lock_file(path: &Path) -> Result<File, ModelError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            ModelErrorCode::Io.with(format!("could not create {}: {e}", parent.display()))
        })?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| ModelErrorCode::Io.with(format!("could not open {}: {e}", path.display())))
}

/// **The per-digest install lock.** Exclusive while an install runs and while a
/// `remove` or a `prune` deletes; shared for the instant a lease is taken.
pub(crate) struct InstallLock {
    file: File,
}

impl InstallLock {
    /// Blocking. See the module note: the second caller is meant to wait and
    /// then find the work already done, not to be told to come back.
    pub(crate) fn exclusive(path: &Path) -> Result<InstallLock, ModelError> {
        let file = open_lock_file(path)?;
        FileExt::lock_exclusive(&file).map_err(|e| {
            ModelErrorCode::Io.with(format!("could not lock {}: {e}", path.display()))
        })?;
        Ok(InstallLock { file })
    }

    /// Held only across *take a lease*, which is why it may be shared: any
    /// number of workers may load one model at once, and none of them may be
    /// deleting it.
    pub(crate) fn shared(path: &Path) -> Result<InstallLock, ModelError> {
        let file = open_lock_file(path)?;
        FileExt::lock_shared(&file).map_err(|e| {
            ModelErrorCode::Io.with(format!("could not lock {}: {e}", path.display()))
        })?;
        Ok(InstallLock { file })
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// **One live holder of one model.**
///
/// The file is created, locked exclusively, and deleted on drop. Its *name* is
/// diagnostic only — nothing reads a pid out of it — because the lock is the
/// fact and the name is decoration.
pub(crate) struct Lease {
    file: File,
    path: PathBuf,
}

impl Lease {
    pub(crate) fn take(dir: &Path) -> Result<Lease, ModelError> {
        fs::create_dir_all(dir).map_err(|e| {
            ModelErrorCode::Io.with(format!("could not create {}: {e}", dir.display()))
        })?;
        // A pid alone is not unique: one process may hold several leases on one
        // model, and two dictations in one worker is an ordinary state.
        for attempt in 0..64u32 {
            let path = dir.join(format!("{}-{attempt}.lease", std::process::id()));
            let file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) => {
                    return Err(
                        ModelErrorCode::Io.with(format!("could not open {}: {e}", path.display()))
                    );
                }
            };
            // A file we can lock is either ours to take or a dead holder's;
            // either way this handle is now the holder. One we cannot lock is
            // live, and the next name is tried.
            if FileExt::try_lock_exclusive(&file).is_ok() {
                return Ok(Lease { file, path });
            }
        }
        Err(ModelErrorCode::Io.with(format!("{} already holds 64 live leases", dir.display())))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        let _ = fs::remove_file(&self.path);
    }
}

/// **How many processes are holding this model open**, reclaiming the files of
/// any that died.
///
/// The reclamation is not tidying: a lease file left by a killed worker would
/// otherwise refuse every `remove` of that model forever, and the only repair a
/// person would have is to delete a file nothing told them about. Taking the
/// lock is what proves the holder is gone — see the module note.
pub(crate) fn live_leases(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut live = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lease") {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            // Opening it failed, so something else has it in a way we cannot
            // see past. Counting it live is the safe direction: the cost is a
            // refused delete, and the alternative cost is deleting a model
            // somebody is dictating with.
            live += 1;
            continue;
        };
        if FileExt::try_lock_exclusive(&file).is_ok() {
            let _ = FileExt::unlock(&file);
            drop(file);
            let _ = fs::remove_file(&path);
        } else {
            live += 1;
        }
    }
    live
}

/// A staging directory, and the `.busy` lock beside it that says its installer
/// is still alive.
///
/// The lock is *beside* rather than inside so that sweeping is one
/// `remove_dir_all` and one `remove_file`, with no handle open inside the tree
/// being deleted — which Windows refuses.
pub(crate) struct Staging {
    dir: PathBuf,
    busy: PathBuf,
    file: Option<File>,
}

impl Staging {
    pub(crate) fn create(staging_root: &Path, label: &str) -> Result<Staging, ModelError> {
        fs::create_dir_all(staging_root).map_err(|e| {
            ModelErrorCode::Io.with(format!("could not create {}: {e}", staging_root.display()))
        })?;
        let unique = format!(
            "{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let dir = staging_root.join(&unique);
        let busy = staging_root.join(format!("{unique}.busy"));
        let file = open_lock_file(&busy)?;
        FileExt::try_lock_exclusive(&file).map_err(|e| {
            ModelErrorCode::InstallFailed.with(format!("could not claim {}: {e}", busy.display()))
        })?;
        Ok(Staging {
            dir,
            busy,
            file: Some(file),
        })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Give up ownership without deleting — what `publish` calls once the
    /// rename has succeeded and the directory is no longer staging.
    pub(crate) fn release(mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
        let _ = fs::remove_file(&self.busy);
        // `dir` is gone (renamed) or is the caller's problem now.
        self.dir = PathBuf::new();
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
        let _ = fs::remove_file(&self.busy);
    }
}

/// **Sweep staging debris**: a directory whose `.busy` file unlocks belonged to
/// a process that died mid-install.
///
/// Best effort throughout, exactly as `prune_verified` is: failing to tidy is
/// never a reason to fail the install that called this.
pub(crate) fn sweep_staging(staging_root: &Path) {
    let Ok(entries) = fs::read_dir(staging_root) else {
        return;
    };
    for entry in entries.flatten() {
        let busy = entry.path();
        if busy.extension().and_then(|e| e.to_str()) != Some("busy") {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&busy) else {
            continue;
        };
        if FileExt::try_lock_exclusive(&file).is_err() {
            continue; // a live installer
        }
        let _ = FileExt::unlock(&file);
        drop(file);
        let dir = busy.with_extension("");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&busy);
    }
    // A staging directory with no `.busy` at all is from a process that died
    // between creating one and creating the other, or from an older build.
    // It has no owner by construction.
    let Ok(entries) = fs::read_dir(staging_root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let mut busy = dir.clone();
        busy.as_mut_os_string().push(".busy");
        if !busy.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests;
