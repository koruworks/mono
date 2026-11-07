// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;

use super::*;

pub(crate) const KORU: &str = if cfg!(windows) { "koru.exe" } else { "koru" };
const AGENT: &str = if cfg!(windows) {
    "koru-agent.exe"
} else {
    "koru-agent"
};

/// The pid [`Fake::koru_window`] says is running the Koru app.
pub(crate) const WINDOW: u32 = 4321;

pub(crate) fn at(dir: &str, file: &str) -> PathBuf {
    Path::new(dir).join(file)
}

/// Every OS answer, written down. `canonical` resolves through `links` and
/// otherwise answers only for paths it was told exist — which is what
/// `fs::canonicalize` does, without a disk.
pub(crate) struct Fake {
    pub(crate) own_exe: Option<PathBuf>,
    pub(crate) images: HashMap<u32, PathBuf>,
    pub(crate) links: HashMap<PathBuf, PathBuf>,
    pub(crate) exists: Vec<PathBuf>,
}

impl Fake {
    /// An Agent at `/app/koru-agent`, and pid [`WINDOW`] running `/app/koru`.
    pub(crate) fn koru_window() -> Self {
        Self {
            own_exe: Some(at("/app", AGENT)),
            images: HashMap::from([(WINDOW, at("/app", KORU))]),
            links: HashMap::new(),
            exists: vec![at("/app", AGENT), at("/app", KORU)],
        }
    }

    /// The same machine, with `pid` running `image` (which exists).
    pub(crate) fn with_process(mut self, pid: u32, image: PathBuf) -> Self {
        self.exists.push(image.clone());
        self.images.insert(pid, image);
        self
    }
}

impl ProcessProbe for Fake {
    fn own_exe(&self) -> Option<PathBuf> {
        self.own_exe.clone()
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

/// **The decision, stated as a test.** A peer is the Koru app when the OS says
/// it is running the `koru` beside the Agent — and nothing about how it was
/// started is asked, because nothing about it is known here.
#[test]
fn a_peer_running_the_koru_beside_the_agent_is_the_koru_app() {
    assert!(verify(Some(WINDOW), &Fake::koru_window()).is_ok());
}

/// A peer running anything else — another binary in the same folder, or a
/// `koru` from another installation — is a stranger, however it is named.
#[test]
fn a_peer_running_another_binary_is_not_the_koru_app() {
    let sibling = Fake::koru_window().with_process(77, at("/app", "koru-mcp"));
    assert_eq!(
        verify(Some(77), &sibling).unwrap_err(),
        Refusal::NotThisKoru,
        "a sibling that is not koru"
    );

    let elsewhere = Fake::koru_window().with_process(78, at("/nightly", KORU));
    assert_eq!(
        verify(Some(78), &elsewhere).unwrap_err(),
        Refusal::NotThisKoru,
        "a koru from another installation"
    );

    // The Agent's own binary is not a window either.
    let agent = Fake::koru_window().with_process(79, at("/app", AGENT));
    assert_eq!(
        verify(Some(79), &agent).unwrap_err(),
        Refusal::NotThisKoru,
        "the agent binary"
    );
}

/// Every lookup that fails is a refusal: no peer pid, no own path, no image,
/// an image that no longer resolves, a `koru` that is not beside us at all.
#[test]
fn any_failed_lookup_is_a_refusal() {
    assert_eq!(
        verify(None, &Fake::koru_window()).unwrap_err(),
        Refusal::Unverified,
        "peer pid unknown"
    );

    let mut no_exe = Fake::koru_window();
    no_exe.own_exe = None;
    assert_eq!(
        verify(Some(WINDOW), &no_exe).unwrap_err(),
        Refusal::Unverified,
        "own executable unknown"
    );

    assert_eq!(
        verify(Some(9999), &Fake::koru_window()).unwrap_err(),
        Refusal::Unverified,
        "peer image unknown"
    );

    // Linux reports a replaced binary as `… (deleted)`, which does not resolve.
    let mut replaced = Fake::koru_window();
    replaced
        .images
        .insert(WINDOW, at("/app", &format!("{KORU} (deleted)")));
    assert_eq!(
        verify(Some(WINDOW), &replaced).unwrap_err(),
        Refusal::Unverified,
        "image does not resolve"
    );

    let mut no_sibling = Fake::koru_window();
    no_sibling.exists.retain(|path| *path != at("/app", KORU));
    assert_eq!(
        verify(Some(WINDOW), &no_sibling).unwrap_err(),
        Refusal::Unverified,
        "no koru beside us"
    );
}

/// Both sides are compared after canonicalising: a symlinked install, a
/// relative `current_exe` and a peer image reached through a link all name
/// the same file and are trusted; a link that leads somewhere else is not.
#[test]
fn paths_are_compared_after_canonicalisation() {
    // The Agent runs through a symlinked folder; the peer's image is the real
    // file.
    let mut linked_agent = Fake::koru_window();
    linked_agent.own_exe = Some(at("/bin-link", AGENT));
    linked_agent
        .links
        .insert(at("/bin-link", KORU), at("/app", KORU));
    assert!(verify(Some(WINDOW), &linked_agent).is_ok());

    // A relative `current_exe`, and a peer image reported through a link.
    let mut relative = Fake::koru_window();
    relative.own_exe = Some(at("./target/debug", AGENT));
    relative
        .links
        .insert(at("./target/debug", KORU), at("/app", KORU));
    relative
        .images
        .insert(WINDOW, at("/Applications/Koru", KORU));
    relative
        .links
        .insert(at("/Applications/Koru", KORU), at("/app", KORU));
    assert!(verify(Some(WINDOW), &relative).is_ok());

    // A link named koru that resolves to a different file is that file.
    let mut impostor = Fake::koru_window().with_process(66, at("/tmp", "evil"));
    impostor.images.insert(66, at("/tmp", KORU));
    impostor.links.insert(at("/tmp", KORU), at("/tmp", "evil"));
    assert_eq!(
        verify(Some(66), &impostor).unwrap_err(),
        Refusal::NotThisKoru
    );
}
