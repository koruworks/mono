// SPDX-License-Identifier: MPL-2.0

//! The interleaving model of §6.5 (page-document plan, P0b).
//!
//! An in-memory filesystem, one writer, two cleaners and a store whose
//! confirmation can be delayed or lost, explored exhaustively over every
//! interleaving of their steps with any actor dying at any step. The claims
//! asserted on every reachable state are the plan's: every published
//! record's revision has its updates and every listed blob present; content
//! the store has not confirmed is never deleted; and once the store holds
//! everything and a cleaner lives, nothing is left behind. Repeated clean-up
//! is a no-op, and a dead writer's `.part` is ignored.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::*;

// ----------------------------------------------------------- a filesystem

/// A filesystem as a map, with the semantics the disk machines rely on: a
/// parent must exist to write into, a directory must be empty to remove, a
/// rename moves a whole subtree atomically.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
struct MemFs {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

fn not_found(what: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, what.display().to_string())
}

impl MemFs {
    fn has_dir(&self, dir: &Path) -> bool {
        self.dirs.contains(dir)
    }

    fn dir_is_empty(&self, dir: &Path) -> bool {
        !self.files.keys().any(|f| f.starts_with(dir) && f != dir)
            && !self.dirs.iter().any(|d| d.starts_with(dir) && d != dir)
    }
}

impl RevisionFs for MemFs {
    fn create_dir(&mut self, dir: &Path) -> io::Result<()> {
        for ancestor in dir.ancestors() {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            self.dirs.insert(ancestor.to_path_buf());
        }
        Ok(())
    }

    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let parent = path.parent().unwrap_or(Path::new(""));
        if !parent.as_os_str().is_empty() && !self.has_dir(parent) {
            return Err(not_found(parent));
        }
        self.files.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(bytes) = self.files.remove(from) {
            self.files.insert(to.to_path_buf(), bytes);
            return Ok(());
        }
        if !self.dirs.remove(from) {
            return Err(not_found(from));
        }
        self.dirs.insert(to.to_path_buf());
        let moved: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter(|d| d.starts_with(from))
            .cloned()
            .collect();
        for dir in moved {
            self.dirs.remove(&dir);
            self.dirs.insert(to.join(dir.strip_prefix(from).unwrap()));
        }
        let files: Vec<(PathBuf, Vec<u8>)> = self
            .files
            .iter()
            .filter(|(f, _)| f.starts_with(from))
            .map(|(f, b)| (f.clone(), b.clone()))
            .collect();
        for (file, bytes) in files {
            self.files.remove(&file);
            self.files
                .insert(to.join(file.strip_prefix(from).unwrap()), bytes);
        }
        Ok(())
    }

    fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        self.files
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| not_found(path))
    }

    fn remove_dir(&mut self, dir: &Path) -> io::Result<()> {
        if !self.has_dir(dir) {
            return Err(not_found(dir));
        }
        assert!(self.dir_is_empty(dir), "remove_dir on a non-empty {dir:?}");
        self.dirs.remove(dir);
        Ok(())
    }

    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files.get(path).cloned().ok_or_else(|| not_found(path))
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for d in &self.dirs {
            if d.parent() == Some(dir) {
                out.push(Entry {
                    name: d.file_name().unwrap().to_string_lossy().into_owned(),
                    is_dir: true,
                });
            }
        }
        for f in self.files.keys() {
            if f.parent() == Some(dir) {
                out.push(Entry {
                    name: f.file_name().unwrap().to_string_lossy().into_owned(),
                    is_dir: false,
                });
            }
        }
        Ok(out)
    }
}

// ------------------------------------------------------------- fixtures

const DOC: &str = "outbox/ws/doc";
const REPLICA: u64 = 0xA1;

fn doc_dir() -> PathBuf {
    PathBuf::from(DOC)
}

fn name(rev: u64) -> RevisionName {
    RevisionName {
        replica: REPLICA,
        rev,
    }
}

/// In the model a revision's bytes *are* its name, so the store can say
/// which revision a version or an updates file belongs to.
fn record(rev: u64, blobs: &[String]) -> Record {
    Record {
        workspace: "ws".into(),
        document: "doc".into(),
        page: "list:x".into(),
        generation: 0,
        state: "registering".into(),
        seen_listed: false,
        blobs: blobs.to_vec(),
        version: name(rev).dir_name().into_bytes(),
    }
}

fn blobs_for(rev: u64, count: usize) -> Vec<(String, Vec<u8>)> {
    (0..count)
        .map(|j| {
            (
                format!("h{rev}{j}"),
                format!("bytes {rev} {j}").into_bytes(),
            )
        })
        .collect()
}

fn writer(rev: u64, blob_count: usize) -> WriteRevision {
    let blobs = blobs_for(rev, blob_count);
    let hashes: Vec<String> = blobs.iter().map(|(h, _)| h.clone()).collect();
    WriteRevision::new(
        &doc_dir(),
        name(rev),
        &format!("p{rev}"),
        name(rev).dir_name().into_bytes(),
        blobs,
        &record(rev, &hashes),
    )
}

/// The store, as a set of what it has durably confirmed.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
struct Store {
    updates: BTreeSet<RevisionName>,
    blobs: BTreeSet<String>,
}

impl Confirmation for Store {
    fn covers(&self, version: &[u8]) -> bool {
        RevisionName::parse(std::str::from_utf8(version).unwrap())
            .is_some_and(|n| self.updates.contains(&n))
    }

    fn covers_updates(&self, updates: &[u8]) -> bool {
        self.covers(updates)
    }

    fn blob_confirmed(&self, hash: &str) -> bool {
        self.blobs.contains(hash)
    }
}

struct Writers(Option<u64>);

impl WriterStatus for Writers {
    fn current_revision(&self, replica: u64) -> Option<u64> {
        assert_eq!(replica, REPLICA);
        self.0
    }
}

// ------------------------------------------------------- the plain cases

#[test]
fn a_revision_name_round_trips_as_a_directory_and_as_a_record() {
    let n = name(7);
    assert_eq!(n.dir_name(), "00000000000000a1.7");
    assert_eq!(n.record_name(), "00000000000000a1.7.json");
    assert_eq!(RevisionName::parse(&n.dir_name()), Some(n));
    assert_eq!(RevisionName::parse(&n.record_name()), Some(n));
    assert_eq!(RevisionName::parse("00000000000000a1.7.json.part"), None);
    assert_eq!(RevisionName::parse(".p1.part"), None);
    assert_eq!(
        RevisionName::parse("a1.7"),
        None,
        "a replica is sixteen hex digits"
    );
}

#[test]
fn a_record_round_trips_through_its_encoding() {
    let r = record(3, &["h30".into()]);
    assert_eq!(Record::decode(&r.encode()), Some(r));
    assert_eq!(Record::decode(b"not json"), None);
}

#[test]
fn a_written_revision_is_discovered_complete_and_nothing_else() {
    let mut fs = MemFs::default();
    writer(1, 2).run(&mut fs).unwrap();
    let found = discover(&fs, &doc_dir()).unwrap();
    assert_eq!(found.complete.len(), 1);
    assert_eq!(found.complete[0].name, name(1));
    assert_eq!(found.complete[0].record.blobs, vec!["h10", "h11"]);
    assert!(found.incomplete.is_empty() && found.damaged.is_empty() && found.parts.is_empty());
    assert!(!found.is_empty());
}

#[test]
fn a_writer_that_dies_before_the_rename_leaves_only_a_part() {
    let mut fs = MemFs::default();
    let mut w = writer(1, 1);
    for _ in 0..3 {
        w.step(&mut fs).unwrap();
    }
    let found = discover(&fs, &doc_dir()).unwrap();
    assert_eq!(found.parts, vec![doc_dir().join(".p1.part")]);
    assert!(found.complete.is_empty() && found.incomplete.is_empty() && found.damaged.is_empty());
    assert!(found.is_empty(), "a part is nobody's to count");
}

#[test]
fn a_writer_that_dies_before_the_record_leaves_an_incomplete_directory() {
    let mut fs = MemFs::default();
    let mut w = writer(1, 1);
    // Create, updates, the blob, the rename — and then nothing.
    for _ in 0..4 {
        w.step(&mut fs).unwrap();
    }
    assert!(!w.is_done(), "the record has not been written");
    let found = discover(&fs, &doc_dir()).unwrap();
    assert_eq!(found.incomplete.len(), 1);
    assert_eq!(found.incomplete[0].name, name(1));
    assert!(found.complete.is_empty() && found.damaged.is_empty());

    // Kept while its writer may still be about to write the record.
    let store = Store {
        updates: [name(1)].into(),
        blobs: ["h10".to_string()].into(),
    };
    assert!(!may_adopt(&found.incomplete[0], &fs, &Writers(Some(1)), &store).unwrap());
    // Adoptable once the writer is gone or has moved on — and only if the
    // store holds the content.
    assert!(may_adopt(&found.incomplete[0], &fs, &Writers(None), &store).unwrap());
    assert!(may_adopt(&found.incomplete[0], &fs, &Writers(Some(2)), &store).unwrap());
    assert!(!may_adopt(&found.incomplete[0], &fs, &Writers(None), &Store::default()).unwrap());
}

#[test]
fn a_record_whose_blob_is_missing_is_damaged_not_consumable() {
    let mut fs = MemFs::default();
    writer(1, 1).run(&mut fs).unwrap();
    fs.remove_file(&doc_dir().join(name(1).dir_name()).join("h10.blob"))
        .unwrap();
    let found = discover(&fs, &doc_dir()).unwrap();
    assert!(found.complete.is_empty());
    assert_eq!(found.damaged.len(), 1);
    assert_eq!(found.damaged[0].name, Some(name(1)));
    assert!(
        found.damaged[0].why.contains("h10"),
        "{}",
        found.damaged[0].why
    );
    assert!(!found.is_empty(), "damage is counted");

    let mut fs = MemFs::default();
    writer(2, 0).run(&mut fs).unwrap();
    fs.write_file(&doc_dir().join(name(2).record_name()), b"{}")
        .unwrap();
    let found = discover(&fs, &doc_dir()).unwrap();
    assert_eq!(found.damaged.len(), 1, "an undecodable record is damaged");
    assert_eq!(
        found.incomplete.len(),
        0,
        "and its directory is not reported twice"
    );
}

#[test]
fn the_release_rule_needs_the_version_and_every_blob() {
    let mut fs = MemFs::default();
    writer(1, 2).run(&mut fs).unwrap();
    let found = discover(&fs, &doc_dir()).unwrap();
    let revision = &found.complete[0];
    let mut store = Store::default();
    assert!(!may_release(revision, &store));
    store.updates.insert(name(1));
    assert!(
        !may_release(revision, &store),
        "an unconfirmed image keeps the revision"
    );
    store.blobs.insert("h10".into());
    assert!(!may_release(revision, &store));
    store.blobs.insert("h11".into());
    assert!(may_release(revision, &store));
}

#[test]
fn a_release_takes_the_record_first_and_repeating_it_is_a_no_op() {
    let mut fs = MemFs::default();
    writer(1, 1).run(&mut fs).unwrap();
    let found = discover(&fs, &doc_dir()).unwrap();
    let mut release = ReleaseRevision::of_complete(&fs, &found.complete[0]).unwrap();
    release.step(&mut fs).unwrap();
    let after_one = discover(&fs, &doc_dir()).unwrap();
    assert!(after_one.complete.is_empty(), "the record went first");
    assert_eq!(
        after_one.incomplete.len(),
        1,
        "the directory is still there, record-less"
    );
    release.run(&mut fs).unwrap();
    assert_eq!(
        fs,
        MemFs {
            dirs: [
                PathBuf::from("outbox"),
                PathBuf::from("outbox/ws"),
                doc_dir()
            ]
            .into(),
            files: Default::default()
        }
    );

    // A second cleaner that built its job from the same revision finishes
    // against nothing, and nothing errors.
    let mut again = ReleaseRevision::of_incomplete(
        &fs,
        &Incomplete {
            name: name(1),
            dir: doc_dir().join(name(1).dir_name()),
        },
    )
    .unwrap();
    let before = fs.clone();
    again.run(&mut fs).unwrap();
    assert_eq!(fs, before, "repeated clean-up is a no-op");
}

// --------------------------------------------------------------- the model

/// One cleaner: alive or not, and the job it is in the middle of.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Cleaner {
    alive: bool,
    job: Option<ReleaseRevision>,
}

/// The whole world, hashed so every interleaving is explored once.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct World {
    fs: MemFs,
    store: Store,
    writer_alive: bool,
    /// The revision being written, or the next one to start.
    writing: Option<WriteRevision>,
    /// What the writer had written of its in-flight revision when it died:
    /// `(rev, updates written, blobs written)`.
    abandoned: Option<(u64, bool, Vec<bool>)>,
    next_rev: u64,
    cleaners: [Cleaner; 2],
}

#[derive(Clone, Copy, Debug)]
struct Config {
    revisions: u64,
    blobs: usize,
}

impl World {
    fn start() -> Self {
        Self {
            fs: MemFs::default(),
            store: Store::default(),
            writer_alive: true,
            writing: None,
            abandoned: None,
            next_rev: 1,
            cleaners: [
                Cleaner {
                    alive: true,
                    job: None,
                },
                Cleaner {
                    alive: true,
                    job: None,
                },
            ],
        }
    }

    /// What `WriterStatus` answers: the revision whose record is not yet
    /// written. A finished writer has moved past its last revision.
    fn writers(&self) -> Writers {
        if !self.writer_alive {
            return Writers(None);
        }
        Writers(Some(match &self.writing {
            Some(w) => w.name().rev,
            None => self.next_rev,
        }))
    }

    /// Every revision the writer has started, with what it has written so
    /// far — what the store may confirm and what must not be lost.
    fn started(&self, config: Config) -> Vec<(u64, bool, Vec<bool>)> {
        let mut out = Vec::new();
        for rev in 1..self.next_rev {
            if let Some(w) = self.writing.as_ref().filter(|w| w.name().rev == rev) {
                out.push((
                    rev,
                    w.updates_written(),
                    (0..config.blobs).map(|j| w.blob_written(j)).collect(),
                ));
            } else if let Some(dead) = self.abandoned.as_ref().filter(|a| a.0 == rev) {
                out.push(dead.clone());
            } else {
                out.push((rev, true, vec![true; config.blobs]));
            }
        }
        out
    }

    fn successors(&self, config: Config) -> Vec<World> {
        let mut next = Vec::new();

        // The writer: start or advance the current revision.
        if self.writer_alive && (self.writing.is_some() || self.next_rev <= config.revisions) {
            let mut w = self.clone();
            let mut job = w
                .writing
                .take()
                .unwrap_or_else(|| writer(w.next_rev, config.blobs));
            if job.name().rev == w.next_rev {
                w.next_rev += 1;
            }
            let done = job.step(&mut w.fs).unwrap();
            if !done {
                w.writing = Some(job);
            }
            next.push(w);
        }

        // Each cleaner: advance a job, or scan and take one.
        for i in 0..2 {
            if !self.cleaners[i].alive {
                continue;
            }
            let mut w = self.clone();
            match w.cleaners[i].job.take() {
                Some(mut job) => {
                    if !job.step(&mut w.fs).unwrap() {
                        w.cleaners[i].job = Some(job);
                    }
                    next.push(w);
                }
                None => {
                    if let Some(job) = pick_job(&w) {
                        w.cleaners[i].job = Some(job);
                        next.push(w);
                    }
                }
            }
        }

        // The store: confirm something it could have received. Not
        // confirming is the "lost" branch, and every state already has it.
        for (rev, updates, blobs) in self.started(config) {
            if updates && !self.store.updates.contains(&name(rev)) {
                let mut w = self.clone();
                w.store.updates.insert(name(rev));
                next.push(w);
            }
            for (j, written) in blobs.iter().enumerate() {
                let hash = format!("h{rev}{j}");
                if *written && !self.store.blobs.contains(&hash) {
                    let mut w = self.clone();
                    w.store.blobs.insert(hash);
                    next.push(w);
                }
            }
        }

        // Deaths.
        if self.writer_alive {
            let mut w = self.clone();
            w.writer_alive = false;
            w.abandoned = w.writing.take().map(|job| {
                (
                    job.name().rev,
                    job.updates_written(),
                    (0..config.blobs).map(|j| job.blob_written(j)).collect(),
                )
            });
            next.push(w);
        }
        for i in 0..2 {
            if self.cleaners[i].alive {
                let mut w = self.clone();
                w.cleaners[i] = Cleaner {
                    alive: false,
                    job: None,
                };
                next.push(w);
            }
        }
        next
    }

    /// Whether anything but a death could still happen.
    fn quiescent(&self, config: Config) -> bool {
        let writer_can =
            self.writer_alive && (self.writing.is_some() || self.next_rev <= config.revisions);
        let cleaner_can = (0..2).any(|i| {
            self.cleaners[i].alive && (self.cleaners[i].job.is_some() || pick_job(self).is_some())
        });
        let store_can = self.started(config).into_iter().any(|(rev, u, blobs)| {
            (u && !self.store.updates.contains(&name(rev)))
                || blobs
                    .iter()
                    .enumerate()
                    .any(|(j, w)| *w && !self.store.blobs.contains(&format!("h{rev}{j}")))
        });
        !(writer_can || cleaner_can || store_can)
    }
}

/// What a cleaner does on a scan: the first complete revision the store
/// holds, else the first abandoned, covered directory. Nothing else is ever
/// a job.
fn pick_job(w: &World) -> Option<ReleaseRevision> {
    let found = discover(&w.fs, &doc_dir()).unwrap();
    for revision in &found.complete {
        if may_release(revision, &w.store) {
            return Some(ReleaseRevision::of_complete(&w.fs, revision).unwrap());
        }
    }
    let writers = w.writers();
    for incomplete in &found.incomplete {
        if may_adopt(incomplete, &w.fs, &writers, &w.store).unwrap() {
            return Some(ReleaseRevision::of_incomplete(&w.fs, incomplete).unwrap());
        }
    }
    None
}

/// The plan's assertions, on one reachable state.
fn check(w: &World, config: Config) {
    let dir = |rev: u64| doc_dir().join(name(rev).dir_name());
    let part = |rev: u64| doc_dir().join(format!(".p{rev}{PART_SUFFIX}"));
    let present = |rev: u64, file: &str| {
        w.fs.files.contains_key(&dir(rev).join(file))
            || w.fs.files.contains_key(&part(rev).join(file))
    };

    // Content the store has not confirmed is never deleted.
    for (rev, updates, blobs) in w.started(config) {
        if updates && !w.store.updates.contains(&name(rev)) {
            assert!(
                present(rev, UPDATES),
                "rev {rev}: unconfirmed updates were lost\n{w:#?}"
            );
        }
        for (j, written) in blobs.iter().enumerate() {
            let hash = format!("h{rev}{j}");
            if *written && !w.store.blobs.contains(&hash) {
                assert!(
                    present(rev, &blob_file(&hash)),
                    "rev {rev}: unconfirmed {hash} was lost\n{w:#?}"
                );
            }
        }
    }

    // Every published record's revision has its updates and every listed
    // blob present — a record is never left pointing at a deleted or
    // half-deleted directory.
    let found = discover(&w.fs, &doc_dir()).unwrap();
    assert!(found.damaged.is_empty(), "a record was damaged\n{w:#?}");
    for revision in &found.complete {
        assert!(w.fs.files.contains_key(&revision.dir.join(UPDATES)));
        for hash in &revision.record.blobs {
            assert!(w.fs.files.contains_key(&revision.dir.join(blob_file(hash))));
        }
    }

    if w.quiescent(config) {
        let writers = w.writers();
        for incomplete in &found.incomplete {
            // An incomplete directory is kept only because it has to be:
            // its writer may still name it, or the store does not hold it.
            assert!(
                !may_adopt(incomplete, &w.fs, &writers, &w.store).unwrap()
                    || !w.cleaners.iter().any(|c| c.alive),
                "a covered, abandoned directory was left behind\n{w:#?}"
            );
        }
        // Once the store holds everything and a cleaner lives, nothing is
        // left but a dead writer's part — and nothing is owed.
        let all_confirmed = w.started(config).into_iter().all(|(rev, u, blobs)| {
            (!u || w.store.updates.contains(&name(rev)))
                && blobs
                    .iter()
                    .enumerate()
                    .all(|(j, written)| !written || w.store.blobs.contains(&format!("h{rev}{j}")))
        });
        let writer_done = !w.writer_alive || (w.writing.is_none() && w.next_rev > config.revisions);
        if all_confirmed && writer_done && w.cleaners.iter().any(|c| c.alive) {
            assert!(
                found.complete.is_empty() && found.incomplete.is_empty(),
                "{w:#?}"
            );
            assert!(
                found.parts.is_empty() || !w.writer_alive,
                "only a dead writer leaves a part\n{w:#?}"
            );
        }
    }
}

fn explore(config: Config) -> usize {
    let mut seen: HashSet<World> = HashSet::new();
    let mut stack = vec![World::start()];
    while let Some(world) = stack.pop() {
        if !seen.insert(world.clone()) {
            continue;
        }
        check(&world, config);
        stack.extend(world.successors(config));
    }
    seen.len()
}

/// One revision with two images, one writer, two cleaners, a store that may
/// confirm anything at any time or never, and any actor dying at any step.
#[test]
fn every_interleaving_of_one_revision_keeps_the_plans_promises() {
    let states = explore(Config {
        revisions: 1,
        blobs: 2,
    });
    assert!(states > 1_000, "the exploration is not trivial: {states}");
}

/// Two revisions in sequence, so a cleaner can meet the writer's older
/// directory while the newer is in flight, and an abandoned first revision
/// is older than the writer's current one.
#[test]
fn every_interleaving_of_two_revisions_keeps_the_plans_promises() {
    let states = explore(Config {
        revisions: 2,
        blobs: 1,
    });
    assert!(states > 1_000, "the exploration is not trivial: {states}");
}

// ------------------------------------------------------------- the disk

/// The same machines over `std::fs`: written, found, released — and a
/// record-less directory found as incomplete.
#[test]
fn the_disk_runs_the_same_machines() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("doc");
    let blobs = blobs_for(1, 1);
    let hashes: Vec<String> = blobs.iter().map(|(h, _)| h.clone()).collect();
    WriteRevision::new(
        &dir,
        name(1),
        "p1",
        b"updates".to_vec(),
        blobs,
        &record(1, &hashes),
    )
    .run(&mut DiskFs)
    .unwrap();
    let found = discover(&DiskFs, &dir).unwrap();
    assert_eq!(found.complete.len(), 1);
    assert_eq!(found.complete[0].name, name(1));
    assert_eq!(
        std::fs::read(found.complete[0].dir.join(UPDATES)).unwrap(),
        b"updates"
    );

    let mut release = ReleaseRevision::of_name(&DiskFs, &dir, name(1)).unwrap();
    release.run(&mut DiskFs).unwrap();
    assert!(discover(&DiskFs, &dir).unwrap().is_empty());
    assert!(!dir.join(name(1).dir_name()).exists());

    std::fs::create_dir_all(dir.join(name(2).dir_name())).unwrap();
    let found = discover(&DiskFs, &dir).unwrap();
    assert_eq!(found.incomplete.len(), 1);
    assert!(found.complete.is_empty());
}
