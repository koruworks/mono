// SPDX-License-Identifier: MPL-2.0

//! What a working scope *is* on this machine's filesystem.
//!
//! A [`WorkspaceScope`] names a folder by a project id and a relative folder
//! a person typed. Two of those can name one directory — through case, a
//! trailing separator, a `\\?\` prefix, a junction, a symlink, a short name
//! — and a string comparison would call them different. A [`ScopeIdentity`]
//! is the directory itself: the path `canonicalize` answers, broken into
//! segments folded the way the filesystem folds them, and compared
//! structurally. Its `Debug` is a hash, because a path is private.

use std::{
    collections::hash_map::RandomState,
    ffi::OsString,
    fmt,
    hash::BuildHasher,
    io,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use koru_delegate_core::{ProjectId, RunId, ScopeOverlap, WorkspaceScope};

use crate::platform;

/// Where a scope's folders are on this machine: the port the session
/// domain's project registry and the run's temporary-directory owner fill.
///
/// A project's root is the host's business — it validates the folder at
/// launch and may supply a default for a project with none — so this crate
/// asks rather than reads a registry file. Resolution happens on the
/// orchestrator's thread; nothing here runs on a thread that answers a
/// person.
pub trait ScopeRoots {
    /// Trusted host seam. Production always checks the existing canonical
    /// directory; tests may inject a pure identity without touching disk.
    fn bot_folder(
        &self,
        folder: &koru_delegate_core::BotWorkingFolder,
    ) -> Result<ResolvedScope, ScopeRefusal> {
        ResolvedScope::of_directory(Path::new(folder.as_str()))
    }

    /// The absolute directory a session project launches in.
    fn project_root(&self, project: ProjectId) -> RootLookup;

    /// The directory Koru created for this run's `Temporary` scope (D37 owns
    /// it as long as the run lives), or `None` if none was created.
    fn temporary_directory(&self, run: RunId) -> Option<PathBuf>;

    /// The exact temporary scope identity without creating the final run
    /// directory. Hosts use this while a durable claimed run reserves its
    /// scope; the default preserves older in-memory fixtures.
    fn planned_temporary(&self, run: RunId) -> Result<ResolvedScope, ScopeRefusal> {
        let directory = self
            .temporary_directory(run)
            .ok_or(ScopeRefusal::NoTemporaryDirectory)?;
        resolved(&directory)
    }
}

/// What a project-root lookup found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RootLookup {
    Directory(PathBuf),
    /// The project exists and has no folder to launch in.
    NoRoot,
    /// No project by that id on this device — a request that named one
    /// from another device (D15) lands here.
    UnknownProject,
}

/// Why a scope could not be resolved to a directory. A code, never a path:
/// P1.14 renders it to a person, P1.8 may carry it as an attribute.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScopeRefusal {
    /// `WorkspaceScope::None`. A request may be built with it; a run never
    /// starts with it.
    NotAdmissible,
    UnknownProject,
    ProjectHasNoRoot,
    NoTemporaryDirectory,
    /// Bot execution never takes the ordinary manual warn-and-proceed path.
    Busy,
    /// The relative folder resolved somewhere the project's root does not
    /// contain — a `..` chain, an absolute spelling, a junction out.
    FolderOutsideProject,
    NotFound,
    NotADirectory,
    /// The filesystem refused to answer; the kind is `std`'s closed
    /// vocabulary.
    Unreadable(io::ErrorKind),
}

impl fmt::Display for ScopeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAdmissible => f.write_str("no working scope was chosen"),
            Self::UnknownProject => f.write_str("the project is not on this device"),
            Self::ProjectHasNoRoot => f.write_str("the project has no folder"),
            Self::NoTemporaryDirectory => f.write_str("no temporary directory was created"),
            Self::Busy => f.write_str("the working folder is busy"),
            Self::FolderOutsideProject => f.write_str("the folder is outside the project"),
            Self::NotFound => f.write_str("the folder does not exist"),
            Self::NotADirectory => f.write_str("the path is not a directory"),
            Self::Unreadable(kind) => write!(f, "the folder could not be read ({kind:?})"),
        }
    }
}

impl std::error::Error for ScopeRefusal {}

/// How two scopes relate. A folder and its parent overlap in either
/// direction; siblings do not.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Overlap {
    /// One directory, however it was spelled.
    Same,
    /// One lies inside the other. `inner` and `outer` are always the two
    /// different parties; both are named so a reader of a warning need not
    /// invert one to learn the other.
    Nested {
        inner: Party,
        outer: Party,
    },
    Disjoint,
}

/// The two sides of an overlap question: the scope asking to start, and the
/// scope a claim already holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Party {
    Candidate,
    Holder,
}

impl Overlap {
    /// How the scope asking to start relates to a held one.
    pub fn between(candidate: &ScopeIdentity, holder: &ScopeIdentity) -> Self {
        let (c, h) = (&candidate.segments, &holder.segments);
        if c == h {
            Self::Same
        } else if c.starts_with(h) {
            Self::Nested {
                inner: Party::Candidate,
                outer: Party::Holder,
            }
        } else if h.starts_with(c) {
            Self::Nested {
                inner: Party::Holder,
                outer: Party::Candidate,
            }
        } else {
            Self::Disjoint
        }
    }

    /// Whether the two scopes share any directory.
    pub fn is_overlap(self) -> bool {
        !matches!(self, Self::Disjoint)
    }

    /// The same answer with the candidate as the point of view — the shape
    /// a run's history and its event row carry (D34). `None` for
    /// [`Self::Disjoint`], which is not an overlap and has no word.
    pub fn for_candidate(self) -> Option<ScopeOverlap> {
        match self {
            Self::Same => Some(ScopeOverlap::Same),
            Self::Nested {
                inner: Party::Candidate,
                ..
            } => Some(ScopeOverlap::Inside),
            Self::Nested { .. } => Some(ScopeOverlap::Around),
            Self::Disjoint => None,
        }
    }
}

/// The canonical identity of one directory on this machine.
///
/// Built from a resolved real directory, or from fixed child segments beneath
/// one for a not-yet-created Koru-owned run folder. Two identities that compare
/// equal are one directory and two that compare as nested are one inside the
/// other — through every alias the filesystem offers. It is not serialized:
/// an alias can change while a worker is down, so a restart resolves again
/// (see [`crate::ScopeClaims::reconcile`]) rather than trusting a stored
/// answer.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ScopeIdentity {
    /// The volume as one segment, then each folded directory name. Never a
    /// separator, never the original spelling.
    segments: Vec<OsString>,
}

/// One scope resolved once: the opaque overlap identity and the exact
/// canonical directory that launch must consume. It is intentionally
/// in-memory only; its `Debug` remains path-free.
#[derive(Clone)]
pub struct ResolvedScope {
    pub identity: ScopeIdentity,
    pub directory: PathBuf,
}

impl fmt::Debug for ResolvedScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedScope")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl ResolvedScope {
    /// Resolve once and return both the canonical directory launch must use
    /// and the identity derived from those same bytes.
    pub fn of_directory(path: &Path) -> Result<Self, ScopeRefusal> {
        resolved(path)
    }

    /// Plan a not-yet-created Koru-owned child below an existing directory.
    /// The parent is canonicalized once; fixed child segments are then part
    /// of the same opaque identity without creating a candidate folder that
    /// could be orphaned by a pre-claim process crash.
    pub fn planned_under(existing_parent: &Path, child: &Path) -> Result<Self, ScopeRefusal> {
        let parent = platform::resolve(existing_parent).map_err(refusal_for)?;
        if !parent.is_dir() {
            return Err(ScopeRefusal::NotADirectory);
        }
        let directory = parent.join(child);
        Ok(Self {
            identity: ScopeIdentity::from_resolved(&directory),
            directory,
        })
    }

    pub fn resolve(
        scope: &WorkspaceScope,
        run: RunId,
        roots: &dyn ScopeRoots,
    ) -> Result<Self, ScopeRefusal> {
        match scope {
            WorkspaceScope::None => Err(ScopeRefusal::NotAdmissible),
            // A lost mount must never become an empty replacement working folder.
            WorkspaceScope::BotFolder { folder } => roots.bot_folder(folder),
            WorkspaceScope::ProjectRoot { project } => resolved(&project_root(roots, *project)?),
            WorkspaceScope::ProjectFolder { project, folder } => {
                let root = resolved(&project_root(roots, *project)?)?;
                let folder = resolved(&root.directory.join(folder.as_str()))?;
                match Overlap::between(&folder.identity, &root.identity) {
                    Overlap::Same
                    | Overlap::Nested {
                        inner: Party::Candidate,
                        ..
                    } => Ok(folder),
                    Overlap::Nested { .. } | Overlap::Disjoint => {
                        Err(ScopeRefusal::FolderOutsideProject)
                    }
                }
            }
            WorkspaceScope::Temporary => {
                let directory = roots
                    .temporary_directory(run)
                    .ok_or(ScopeRefusal::NoTemporaryDirectory)?;
                resolved(&directory)
            }
        }
    }
}

impl ScopeIdentity {
    /// Pure test fixture only. Never a caller-provided production identity.
    #[cfg(feature = "fixtures")]
    pub fn fixture_canonical(path: &Path) -> Self {
        Self::from_resolved(path)
    }

    /// The identity of the directory at `path`, or why there is none. The
    /// one place this crate touches the disk.
    pub fn of_directory(path: &Path) -> Result<Self, ScopeRefusal> {
        Ok(ResolvedScope::of_directory(path)?.identity)
    }

    /// The identity a scope names for `run`, resolved through `roots`.
    ///
    /// `WorkspaceScope::None` is refused unconditionally. A project folder
    /// must resolve to the project's root or inside it; one that lands
    /// elsewhere is refused rather than admitted where the person did not
    /// point.
    pub fn resolve(
        scope: &WorkspaceScope,
        run: RunId,
        roots: &dyn ScopeRoots,
    ) -> Result<Self, ScopeRefusal> {
        Ok(ResolvedScope::resolve(scope, run, roots)?.identity)
    }

    /// How this scope, asking to start, relates to a held one.
    pub fn overlap(&self, holder: &Self) -> Overlap {
        Overlap::between(self, holder)
    }

    /// How many directories deep below the volume. A count is safe to
    /// report; the names are not.
    pub fn depth(&self) -> usize {
        self.segments.len().saturating_sub(1)
    }

    /// The identity of an already-canonical path: the segments alone. The
    /// tests exercise the fold and prefix rules through it without a disk.
    pub(crate) fn from_resolved(path: &Path) -> Self {
        let mut segments = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => {
                    segments.push(platform::prefix_segment(prefix.kind()));
                }
                Component::RootDir | Component::CurDir => {}
                // A canonical path carries no `..`; handled structurally so
                // the identity is well-formed even if one did.
                Component::ParentDir => {
                    if segments.len() > 1 {
                        segments.pop();
                    }
                }
                Component::Normal(name) => segments.push(platform::fold(name)),
            }
        }
        Self { segments }
    }
}

fn resolved(path: &Path) -> Result<ResolvedScope, ScopeRefusal> {
    let directory = platform::resolve(path).map_err(refusal_for)?;
    if !directory.is_dir() {
        return Err(ScopeRefusal::NotADirectory);
    }
    Ok(ResolvedScope {
        identity: ScopeIdentity::from_resolved(&directory),
        directory,
    })
}

fn project_root(roots: &dyn ScopeRoots, project: ProjectId) -> Result<PathBuf, ScopeRefusal> {
    match roots.project_root(project) {
        RootLookup::Directory(root) => Ok(root),
        RootLookup::NoRoot => Err(ScopeRefusal::ProjectHasNoRoot),
        RootLookup::UnknownProject => Err(ScopeRefusal::UnknownProject),
    }
}

fn refusal_for(error: io::Error) -> ScopeRefusal {
    match error.kind() {
        io::ErrorKind::NotFound => ScopeRefusal::NotFound,
        io::ErrorKind::NotADirectory => ScopeRefusal::NotADirectory,
        kind => ScopeRefusal::Unreadable(kind),
    }
}

/// One key per process, so two identities of one directory print the same
/// marker within a run of tests or a worker's lifetime, and no marker in a
/// captured log can be matched against a guessed path afterwards. FNV or
/// any fixed-key hash would be a dictionary attack away from the folder
/// name.
static SALT: OnceLock<RandomState> = OnceLock::new();

impl fmt::Debug for ScopeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let marker = SALT.get_or_init(RandomState::new).hash_one(&self.segments);
        write!(f, "ScopeIdentity(#{marker:016x})")
    }
}
