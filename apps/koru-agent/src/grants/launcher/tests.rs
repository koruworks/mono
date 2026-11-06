// SPDX-License-Identifier: MPL-2.0

use std::cell::Cell;
use std::collections::HashMap;

use super::*;
use crate::grants::{GrantAuthority, Refusal};

const KORU: &str = if cfg!(windows) { "koru.exe" } else { "koru" };
const AGENT: &str = if cfg!(windows) {
    "koru-agent.exe"
} else {
    "koru-agent"
};

fn at(dir: &str, file: &str) -> PathBuf {
    Path::new(dir).join(file)
}

/// Every OS answer, written down. `canonical` resolves through `links` and
/// otherwise answers only for paths it was told exist — which is what
/// `fs::canonicalize` does, without a disk.
struct Fake {
    own_exe: Option<PathBuf>,
    /// Successive `parent_pid` answers; the last repeats.
    parents: Vec<Option<u32>>,
    asked: Cell<usize>,
    images: HashMap<u32, PathBuf>,
    links: HashMap<PathBuf, PathBuf>,
    exists: Vec<PathBuf>,
}

impl Fake {
    /// An Agent at `/app/koru-agent` whose parent, pid 4321, runs `/app/koru`.
    fn started_by_koru() -> Self {
        Self {
            own_exe: Some(at("/app", AGENT)),
            parents: vec![Some(4321)],
            asked: Cell::new(0),
            images: HashMap::from([(4321, at("/app", KORU))]),
            links: HashMap::new(),
            exists: vec![at("/app", AGENT), at("/app", KORU)],
        }
    }
}

impl LauncherProbe for Fake {
    fn own_exe(&self) -> Option<PathBuf> {
        self.own_exe.clone()
    }

    fn parent_pid(&self) -> Option<u32> {
        let index = self.asked.get().min(self.parents.len() - 1);
        self.asked.set(self.asked.get() + 1);
        self.parents[index]
    }

    fn image_of(&self, pid: u32) -> Option<PathBuf> {
        self.images.get(&pid).cloned()
    }

    fn canonical(&self, path: &Path) -> Option<PathBuf> {
        let resolved = self
            .links
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_path_buf());
        self.exists.contains(&resolved).then_some(resolved)
    }
}

/// **The decision, stated as a test.** The Agent's parent is trusted when the
/// OS says it is running the `koru` beside the Agent, and in no other case.
#[test]
fn the_koru_beside_the_agent_is_trusted_as_its_launcher() {
    assert_eq!(launcher_to_trust(&Fake::started_by_koru()), Some(4321));
}

/// A parent running anything else — another binary in the same folder, or a
/// `koru` from another installation — is a stranger, however it is named.
#[test]
fn a_parent_running_another_binary_is_not_trusted() {
    let mut other = Fake::started_by_koru();
    other.images.insert(4321, at("/app", "koru-mcp"));
    other.exists.push(at("/app", "koru-mcp"));
    assert_eq!(
        launcher_to_trust(&other),
        None,
        "a sibling that is not koru"
    );

    let mut elsewhere = Fake::started_by_koru();
    elsewhere.images.insert(4321, at("/nightly", KORU));
    elsewhere.exists.push(at("/nightly", KORU));
    assert_eq!(
        launcher_to_trust(&elsewhere),
        None,
        "a koru from another installation"
    );
}

/// Every lookup that fails records nothing: no own path, no parent, no image,
/// an image that no longer resolves, a `koru` that is not beside us at all.
#[test]
fn any_failed_lookup_trusts_nobody() {
    let mut no_exe = Fake::started_by_koru();
    no_exe.own_exe = None;
    assert_eq!(launcher_to_trust(&no_exe), None, "own executable unknown");

    let mut no_parent = Fake::started_by_koru();
    no_parent.parents = vec![None];
    assert_eq!(launcher_to_trust(&no_parent), None, "parent pid unknown");

    let mut no_image = Fake::started_by_koru();
    no_image.images.clear();
    assert_eq!(launcher_to_trust(&no_image), None, "parent image unknown");

    // Linux reports a replaced binary as `… (deleted)`, which does not resolve.
    let mut replaced = Fake::started_by_koru();
    replaced
        .images
        .insert(4321, at("/app", &format!("{KORU} (deleted)")));
    assert_eq!(launcher_to_trust(&replaced), None, "image does not resolve");

    let mut no_sibling = Fake::started_by_koru();
    no_sibling.exists.retain(|path| *path != at("/app", KORU));
    assert_eq!(launcher_to_trust(&no_sibling), None, "no koru beside us");
}

/// An orphan's parent is init or launchd, and pid 0 is nobody; neither is a
/// window even if the probe would answer for it.
#[test]
fn init_is_never_a_launcher() {
    for pid in [0, 1] {
        let mut orphan = Fake::started_by_koru();
        orphan.parents = vec![Some(pid)];
        orphan.images.insert(pid, at("/app", KORU));
        assert_eq!(launcher_to_trust(&orphan), None, "pid {pid}");
    }
}

/// A parent that changed while its image was being read is refused: on Unix
/// that is the parent exiting and this Agent being reparented, and the image
/// read may then belong to whatever reused the pid.
#[test]
fn a_parent_that_changes_during_the_read_is_not_trusted() {
    let mut reparented = Fake::started_by_koru();
    reparented.parents = vec![Some(4321), Some(1)];
    assert_eq!(launcher_to_trust(&reparented), None);
}

/// Both sides are compared after canonicalising: a symlinked install, a
/// relative `current_exe` and a parent image reached through a link all name
/// the same file and are trusted; a link that leads somewhere else is not.
#[test]
fn paths_are_compared_after_canonicalisation() {
    // The Agent runs through a symlinked folder; the parent's image is the
    // real file.
    let mut linked_agent = Fake::started_by_koru();
    linked_agent.own_exe = Some(at("/bin-link", AGENT));
    linked_agent
        .links
        .insert(at("/bin-link", KORU), at("/app", KORU));
    assert_eq!(launcher_to_trust(&linked_agent), Some(4321));

    // A relative `current_exe`, and a parent image reported through a link.
    let mut relative = Fake::started_by_koru();
    relative.own_exe = Some(at("./target/debug", AGENT));
    relative
        .links
        .insert(at("./target/debug", KORU), at("/app", KORU));
    relative.images.insert(4321, at("/Applications/Koru", KORU));
    relative
        .links
        .insert(at("/Applications/Koru", KORU), at("/app", KORU));
    assert_eq!(launcher_to_trust(&relative), Some(4321));

    // A link named koru that resolves to a different file is that file.
    let mut impostor = Fake::started_by_koru();
    impostor.images.insert(4321, at("/tmp", KORU));
    impostor.links.insert(at("/tmp", KORU), at("/tmp", "evil"));
    impostor.exists.push(at("/tmp", "evil"));
    assert_eq!(launcher_to_trust(&impostor), None);
}

/// A launcher recorded this way is exactly as trusted as a spawned window: the
/// same table, so the same grant — and a refused launcher leaves the table as
/// it was.
#[test]
fn a_trusted_launcher_is_granted_like_a_spawned_window() {
    let mut authority = GrantAuthority::default();
    assert_eq!(
        trust_launcher_in(&mut authority, &Fake::started_by_koru(), 1_000),
        Some(4321)
    );
    assert!(authority.was_launched_here(4321));
    let grant = authority
        .mint(Some(4321), 1_000, "grant-launcher".into())
        .expect("the launcher was refused a grant");
    assert!(authority.validate(grant.expose(), 1_001));

    let mut stranger = Fake::started_by_koru();
    stranger.images.insert(4321, at("/elsewhere", KORU));
    stranger.exists.push(at("/elsewhere", KORU));
    let mut authority = GrantAuthority::default();
    assert_eq!(trust_launcher_in(&mut authority, &stranger, 1_000), None);
    assert_eq!(
        authority.mint(Some(4321), 1_000, "grant-stranger".into()),
        Err(Refusal::NotLaunchedHere)
    );
}
