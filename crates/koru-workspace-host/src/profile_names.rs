// SPDX-License-Identifier: MPL-2.0

//! The names of the Profiles this device has seen, remembered on this device
//! so a deletion that happened somewhere else can still be named here.
//!
//! **Why this exists at all.** A deletion carried from another device names
//! no Profile. It cannot: the sending device does not have the name, the
//! signed gossip record has no field for one, and three written rules say
//! lifecycle broadcasts carry no Profile names or account metadata. An
//! optional field would be absent in exactly the case it was added for. So
//! the name never moves — this device simply keeps the one it was already
//! displaying, from while the Profile still existed.
//!
//! **Written during ordinary operation, not in response to a deletion.**
//! Every Store catalog listing that changes goes through [`ProfileNames::observe`],
//! which is long before any deletion is heard of. That is the whole point: the
//! removal is the thing that takes the catalog row away, so a name read when
//! the notice is worded is missing, and for a deletion from another device —
//! where the row is gone before this window hears of it, and where the notice
//! may be replayed to a window on a *later launch* — always missing.
//!
//! **One device-local file, never a message.** `profile-names.conf` under the
//! profile directory, beside `settings-*.conf` and the pending binding
//! markers, written atomically by one worker thread through
//! [`koru_settings::storage::write_atomic`] so a crash leaves the previous
//! complete file rather than an empty one. Nothing serialises it onto a wire,
//! nothing publishes it, and no record here ever leaves the machine.
//!
//! **Why not beside `profile_mru` in the window's settings.** It was the
//! obvious home — settings already keep a most-recently-used list of Profile
//! ids — and it is the wrong one for three reasons. The MRU is *per window
//! slot*, so `settings-<slot>.conf` holds what that slot used while the
//! catalog holds every Profile on the device, and the notice has to name a
//! Profile this window never opened. The two lifetimes are opposites: an MRU
//! entry is worthless the moment its Profile is gone, while a remembered name
//! is worth something *only* then — and `cleanup_removed_profile_state`
//! rewrites every settings file to drop the id from the MRU before the notice
//! is worded, so the shared list would have to carry a special case for the
//! one field that must outlive it. And several Koru processes share the
//! profile directory, so one list read-modify-written by each of them loses
//! entries; a file whose only writer is the live catalog's names does not.
//!
//! **Bounded by a cap, with the live Profiles safe.** [`KEPT`] records,
//! most-recently-seen first. Everything in the current catalog is re-seen on
//! every listing and therefore sits in front of everything that is not, so a
//! Profile that vanished from the catalog is the *last* stale record to be
//! evicted and cannot be pushed out by a Profile that is merely still there.
//! Pruning never drops a name a pending notice needs, because the only
//! deliberate prune is [`ProfileNames::forget`], which the removal path calls
//! after the notice has been worded.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use koru_core::{ProfileId, ProfileSummary};

/// How many Profile names are kept. Generous against the number of Profiles a
/// device has, so eviction is the backstop and not the normal case.
const KEPT: usize = 32;

const PROFILE_NAMES_FILE: &str = "profile-names.conf";

/// How long [`ProfileNames::forget`] will wait for the writer. A removal is
/// usually followed immediately by a switch or a relaunch, so the window may
/// stop existing before an unflushed write lands — and a name the person
/// deleted must not outlive the Profile. The bound is a last resort for a
/// wedged disk, never the expected cost: the file is a few hundred bytes.
const FLUSH_LIMIT: Duration = Duration::from_secs(5);

pub(crate) fn path(profile_dir: &Path) -> PathBuf {
    profile_dir.join(PROFILE_NAMES_FILE)
}

/// The Profile names this device remembers, in memory, mirrored to disk off
/// the event-loop thread.
pub(crate) struct ProfileNames {
    /// Most-recently-seen first. The truth, because the window is the writer.
    known: Vec<(ProfileId, String)>,
    jobs: mpsc::Sender<Vec<(ProfileId, String)>>,
    done: mpsc::Receiver<()>,
    queued: usize,
}

impl ProfileNames {
    /// Read what previous runs left behind — once — and start the writer.
    pub(crate) fn open(profile_dir: PathBuf) -> Self {
        let known = load(&profile_dir);
        let (jobs, job_rx) = mpsc::channel::<Vec<(ProfileId, String)>>();
        let (done_tx, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("koru-profile-names".into())
            .spawn(move || {
                while let Ok(snapshot) = job_rx.recv() {
                    if let Err(error) = save(&profile_dir, &snapshot) {
                        // Not fatal, and never a reason to stop: the next
                        // catalog listing writes the same thing again, and a
                        // missing record only costs a notice its name.
                        eprintln!(
                            "[Koru][profiles] could not record the Profile names on this device: {error}"
                        );
                    }
                    if done_tx.send(()).is_err() {
                        return;
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("[Koru][profiles] could not start the Profile-name writer: {error}");
        }
        Self {
            known,
            jobs,
            done,
            queued: 0,
        }
    }

    /// Take the names from a Store catalog listing.
    ///
    /// Called when the listing *changed*, so this does not write on a timer;
    /// and it writes nothing at all when the names it holds are already the
    /// ones the listing carries.
    pub(crate) fn observe(&mut self, catalog: &[ProfileSummary]) {
        let merged = merged(&self.known, catalog);
        if merged == self.known {
            return;
        }
        self.known = merged;
        self.queue();
    }

    /// The name remembered for one Profile, or `None` when this device never
    /// had one for it.
    pub(crate) fn get(&self, profile: ProfileId) -> Option<String> {
        self.known
            .iter()
            .find(|(id, _)| *id == profile)
            .map(|(_, name)| name.clone())
    }

    /// The Profile is gone from this device: drop its name, and **wait for the
    /// disk**.
    ///
    /// This is the privacy half. A Profile the person removed here should not
    /// leave its name lying around afterwards, and the window frequently
    /// switches Profile or relaunches within the same pass — an unflushed
    /// write would never land. Callers run this only after the notice for that
    /// Profile has been worded, so nothing still needs the name.
    pub(crate) fn forget(&mut self, profile: ProfileId) {
        let before = self.known.len();
        self.known.retain(|(id, _)| *id != profile);
        if self.known.len() == before {
            return;
        }
        self.queue();
        self.flush();
    }

    fn queue(&mut self) {
        if self.jobs.send(self.known.clone()).is_ok() {
            self.queued += 1;
        }
    }

    fn flush(&mut self) {
        while self.queued > 0 {
            match self.done.recv_timeout(FLUSH_LIMIT) {
                Ok(()) => self.queued -= 1,
                // The writer is gone or wedged. Nothing waits forever for a
                // cache of names.
                Err(_) => break,
            }
        }
    }
}

/// A name worth remembering, or nothing. Blank is no name — the notice has a
/// sentence for an unknown name and must not be handed an empty one in quotes.
/// A line break would make a second record out of one, so it becomes a space.
fn remembered_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.replace(['\n', '\r'], " "))
}

/// What the file should hold once a catalog listing has been seen: every named
/// row of the listing, then the records for Profiles the listing no longer
/// carries, capped.
///
/// The order is the eviction order, and it is why a vanished Profile survives:
/// a live Profile is re-seen on every listing and goes in front, so the oldest
/// *absence* is always the first record to go.
fn merged(known: &[(ProfileId, String)], catalog: &[ProfileSummary]) -> Vec<(ProfileId, String)> {
    let mut out: Vec<(ProfileId, String)> =
        Vec::with_capacity(KEPT.min(catalog.len() + known.len()));
    for row in catalog {
        let Some(name) = remembered_name(&row.name) else {
            continue;
        };
        if !out.iter().any(|(id, _)| *id == row.id) {
            out.push((row.id, name));
        }
    }
    for (id, name) in known {
        if !out.iter().any(|(seen, _)| seen == id) {
            out.push((*id, name.clone()));
        }
    }
    out.truncate(KEPT);
    out
}

/// `<profile-id>|<name>` a line, in order. The id is a UUID, so the first
/// separator is the only one that matters and a name may contain `|`.
fn encode(names: &[(ProfileId, String)]) -> String {
    let mut out = String::new();
    for (id, name) in names {
        out.push_str(&id.to_storage());
        out.push('|');
        out.push_str(name);
        out.push('\n');
    }
    out
}

/// Every record that can be read, in file order. A line that cannot be is
/// skipped: one lost name costs a notice its name, and is never a reason for
/// the app not to start.
fn decode(body: &str) -> Vec<(ProfileId, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let Some((raw_id, name)) = line.split_once('|') else {
            continue;
        };
        let Some(id) = ProfileId::parse(raw_id.trim()) else {
            continue;
        };
        let Some(name) = remembered_name(name) else {
            continue;
        };
        if !out
            .iter()
            .any(|(seen, _): &(ProfileId, String)| *seen == id)
        {
            out.push((id, name));
        }
    }
    out.truncate(KEPT);
    out
}

fn load(profile_dir: &Path) -> Vec<(ProfileId, String)> {
    match std::fs::read_to_string(path(profile_dir)) {
        Ok(body) => decode(&body),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            eprintln!(
                "[Koru][profiles] could not read the remembered Profile names in {}: {error}",
                profile_dir.display()
            );
            Vec::new()
        }
    }
}

fn save(profile_dir: &Path, names: &[(ProfileId, String)]) -> Result<(), String> {
    koru_settings::storage::write_atomic(&path(profile_dir), encode(names).as_bytes())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
