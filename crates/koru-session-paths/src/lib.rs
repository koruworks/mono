// SPDX-License-Identifier: MPL-2.0

//! Resolved, device-local storage owned by the supervised session host.
//!
//! This module derives the layout once at the Agent boundary. Runtime code is
//! given [`SessionPaths`] or a final `PathBuf`; it must not read a process-global
//! root or repeat `<root>/agent/sessions` arithmetic.

use std::io;
use std::path::{Path, PathBuf};

use koru_session_protocol::{AttachmentId, SessionId};

#[cfg(test)]
mod tests;

/// The device-local filesystem layout for terminal sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPaths {
    sessions_dir: PathBuf,
}

impl SessionPaths {
    /// Construct from the exact sessions directory resolved by the Agent.
    ///
    /// This deliberately does not accept a product root: the host consumes the
    /// already-resolved value from its authenticated startup bootstrap rather
    /// than repeating `<root>/agent/sessions` arithmetic.
    pub fn from_resolved_directory(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// `<root>/agent/sessions`.
    pub fn directory(&self) -> &Path {
        &self.sessions_dir
    }

    /// The only persisted launch-profile file.
    pub fn profiles_file(&self) -> PathBuf {
        self.sessions_dir.join("profiles.conf")
    }

    /// A sibling staging path for an atomic profile replacement.
    ///
    /// Readers ignore `.part`; the one session host writes this complete file
    /// and replaces [`profiles_file`](Self::profiles_file) in one filesystem
    /// operation. It never edits the destination in place.
    pub fn profiles_atomic_path(&self) -> AtomicPath {
        AtomicPath::for_target(self.profiles_file())
    }

    /// The only persisted project file.
    ///
    /// A rooted run resolves its own sessions directory, so a `--root` host
    /// keeps its own projects and can never read or replace the ordinary
    /// channel's.
    pub fn projects_file(&self) -> PathBuf {
        self.sessions_dir.join("projects.conf")
    }

    /// A sibling staging path for an atomic project-registry replacement.
    ///
    /// The same mechanism as [`profiles_atomic_path`](Self::profiles_atomic_path):
    /// the one session host writes the complete file beside its destination
    /// and replaces it in a single filesystem operation, never editing the
    /// destination in place. A reader that finds only `.part` finds the
    /// previous registry intact.
    pub fn projects_atomic_path(&self) -> AtomicPath {
        AtomicPath::for_target(self.projects_file())
    }

    /// Where a registry that could not be parsed is copied before it is
    /// replaced. Copied, never moved: recovery must not be able to destroy
    /// the thing it was called to preserve.
    pub fn projects_quarantine_path(&self) -> PathBuf {
        self.sessions_dir.join("projects.conf.unreadable")
    }

    /// The host's model catalog: each agent's model list, kept across
    /// restarts. A cache, never an authority — a rooted host keeps its own,
    /// and one that cannot be read is simply rebuilt.
    pub fn provider_models_file(&self) -> PathBuf {
        self.sessions_dir.join("provider-models.json")
    }

    /// The same mechanism as [`profiles_atomic_path`](Self::profiles_atomic_path):
    /// the one session host writes the whole catalog beside its destination
    /// and replaces it in one filesystem operation.
    pub fn provider_models_atomic_path(&self) -> AtomicPath {
        AtomicPath::for_target(self.provider_models_file())
    }

    /// The sole subtree startup cleanup is allowed to traverse.
    pub fn attachments_directory(&self) -> PathBuf {
        self.sessions_dir.join("attachments")
    }

    /// One session's attachment directory. The typed UUID cannot contain a
    /// separator, traversal component, alternate stream, or device name.
    pub fn attachment_directory(&self, session: SessionId) -> PathBuf {
        self.attachments_directory().join(session.to_string())
    }

    /// One verified attachment destination.
    pub fn attachment_file(
        &self,
        session: SessionId,
        attachment: AttachmentId,
        extension: VerifiedAttachmentExtension,
    ) -> PathBuf {
        self.attachment_directory(session)
            .join(format!("{attachment}.{}", extension.as_str()))
    }

    /// A sibling staging path for an atomic attachment commit.
    pub fn attachment_atomic_path(
        &self,
        session: SessionId,
        attachment: AttachmentId,
        extension: VerifiedAttachmentExtension,
    ) -> AtomicPath {
        AtomicPath::for_target(self.attachment_file(session, attachment, extension))
    }

    /// Prepare and validate one session-owned attachment directory.
    ///
    /// Neither the shared attachment root nor the session directory may be a
    /// symlink/reparse point. Callers perform file I/O only after this check and
    /// use typed UUID path components supplied by this crate.
    pub fn prepare_attachment_directory(&self, session: SessionId) -> io::Result<PathBuf> {
        let root = self.attachments_directory();
        ensure_plain_directory(&root)?;
        let directory = self.attachment_directory(session);
        ensure_plain_directory(&directory)?;
        Ok(directory)
    }

    /// Remove exactly one session directory without following a link/reparse
    /// point placed at that path.
    pub fn remove_session_attachments(&self, session: SessionId) -> io::Result<bool> {
        let root = self.attachments_directory();
        let root_metadata = match std::fs::symlink_metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if link_or_reparse_point(&root_metadata) || !root_metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the session attachments root is not a plain directory",
            ));
        }
        let directory = self.attachment_directory(session);
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => {
                remove_without_following(&directory)?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Remove attachment state abandoned by a previous host.
    ///
    /// The authority can only be obtained after both startup claims have been
    /// proved. The attachments root itself may not be a symlink/reparse point,
    /// and directory removal uses the standard library's no-symlink-following
    /// implementation. Nothing above or beside the subtree is touched.
    pub fn cleanup_abandoned_attachments(
        &self,
        _authority: CleanupAuthority,
    ) -> io::Result<CleanupReport> {
        let root = self.attachments_directory();
        let metadata = match std::fs::symlink_metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CleanupReport { removed_entries: 0 });
            }
            Err(error) => return Err(error),
        };
        if link_or_reparse_point(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the session attachments root is a symlink or reparse point",
            ));
        }
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the session attachments root is not a directory",
            ));
        }

        let mut removed_entries = 0;
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            remove_without_following(&entry.path())?;
            removed_entries += 1;
        }
        Ok(CleanupReport { removed_entries })
    }
}

/// Extensions admitted only after content validation and normalization.
///
/// There is deliberately no string constructor. A filename, MIME parameter,
/// provider value, or public request can never become a path component merely
/// by claiming an extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifiedAttachmentExtension {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl VerifiedAttachmentExtension {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }
}

/// Destination plus the sibling path used to prepare a complete replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomicPath {
    destination: PathBuf,
    staging: PathBuf,
}

impl AtomicPath {
    fn for_target(destination: PathBuf) -> Self {
        let staging = destination.with_extension(match destination.extension() {
            Some(extension) => format!("{}.part", extension.to_string_lossy()),
            None => "part".to_owned(),
        });
        Self {
            destination,
            staging,
        }
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    pub fn staging(&self) -> &Path {
        &self.staging
    }
}

/// Startup evidence required before abandoned attachments may be removed.
///
/// With the supervised-host architecture these facts are proved by different
/// processes: Agent grants exclusive authority for this exact root, and the
/// host binds the matching private session endpoint. Neither fact substitutes
/// for the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupPredicate {
    exclusive_root: bool,
    session_endpoint_bound: bool,
}

impl CleanupPredicate {
    pub const fn new(exclusive_root: bool, session_endpoint_bound: bool) -> Self {
        Self {
            exclusive_root,
            session_endpoint_bound,
        }
    }

    pub const fn satisfied(self) -> bool {
        self.exclusive_root && self.session_endpoint_bound
    }

    pub fn authority(self) -> Option<CleanupAuthority> {
        self.satisfied()
            .then_some(CleanupAuthority { _private: () })
    }
}

/// Proof token consumed by startup cleanup.
#[derive(Debug)]
pub struct CleanupAuthority {
    _private: (),
}

/// Content-free result suitable for a low-volume lifecycle diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed_entries: usize,
}

fn remove_without_following(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if link_or_reparse_point(&metadata) || !metadata.is_dir() {
        remove_link_or_file(path, &metadata)
    } else {
        // `remove_dir_all` does not follow symlinks it encounters. Calling it
        // once also uses its platform hardening against traversal races rather
        // than reproducing a check-then-recurse walker here.
        std::fs::remove_dir_all(path)
    }
}

fn ensure_plain_directory(path: &Path) -> io::Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if link_or_reparse_point(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "attachment storage component is not a plain directory",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn remove_link_or_file(path: &Path, _metadata: &std::fs::Metadata) -> io::Result<()> {
    std::fs::remove_file(path)
}

#[cfg(windows)]
fn remove_link_or_file(path: &Path, metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}
