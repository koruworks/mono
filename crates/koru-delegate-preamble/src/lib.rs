// SPDX-License-Identifier: MPL-2.0

//! The preamble a delegated agent reads before its first turn, and where it
//! comes from.
//!
//! [`session`] provides the separate common orientation with a root-scoped
//! user override. Both files share bounded reads and objective validation;
//! the artifact resolver and its existing editor keep their original scope.
//!
//! D20 makes the preamble a Markdown file under `docs/prompts/`, shipped as
//! a release asset **so that a person may edit it**. Editability is the whole
//! reason it is a file rather than a string in this crate; the copy compiled
//! in here ([`BUNDLED`]) exists so that a fresh install, and an install whose
//! edited copy is broken, still has one.
//!
//! ## The rule
//!
//! [`resolve`] looks in two places and then at the bundled copy, in this
//! order:
//!
//! 1. `<installation>/docs/prompts/delegated-agent.md` — the same layout
//!    `koru-mcp-server` reads `docs/mcp/README.md` from. The caller passes
//!    the installation's *data* directory: the executables' own on Windows,
//!    the bundle's `Resources/` on macOS (`koru_update::DATA_PREFIX`).
//! 2. **In a development build only**, the repository's own copy — the file
//!    this crate `include_str!`s, [`SOURCE_PATH`]
//!    (`crates/koru-delegate-preamble/delegated-agent.md`). A `cargo run` puts
//!    the executables in `target/debug/`, which has no `docs/` beside them, so
//!    without this an edit of the repository file changed nothing until the
//!    next build and a developer had no way to tell. `koru-mcp-server` reaches
//!    its own document the same way (`crates/koru-mcp-server/MCP.md`).
//!
//!    **The source and the installed copy are at different paths.** The
//!    source lives beside this crate so the repository builds without the
//!    `docs/` tree; the release packager copies it to [`RELATIVE_PATH`],
//!    which is what an installed Koru — and every updater already shipped —
//!    expects.
//! 3. [`BUNDLED`].
//!
//! **The file beside the exe is searched before anything else**, which is
//! the tree's standing order for anything shipped beside the executables
//! (AGENTS.md; `koru-draw`'s `asset_roots`): an edit takes effect, and a
//! package missing the file is never masked by a build machine's source
//! tree. That last clause is why step 2 is `#[cfg(debug_assertions)]`
//! rather than a runtime check — `CARGO_MANIFEST_DIR` is an absolute path
//! baked in at compile time, so on the machine that built it a *released*
//! package would happily read the source tree and look perfect right up
//! until somebody else ran it. A release does not compile the path at all,
//! and `resolve_within(dir, None)` is that build's behaviour, asserted
//! directly.
//!
//! A copy that exists and cannot be used stops the search: the notice is
//! raised and the bundled text is used, rather than the next place along
//! quietly supplying a different file. A person who edited the copy beside
//! the executables is told their edit did not take.
//!
//! ## Objective validity only (D35)
//!
//! An edited copy is rejected for exactly five faults, every one of them a
//! fact about the file and none about its meaning: missing, empty,
//! unreadable, bad encoding, oversized. **Readable text is used as written,
//! whatever it says.** There is no semantic validator — nothing checks that
//! the tool names are still there, that a folder is still named, or that the
//! instructions are any good — because a person's edited instructions are
//! never silently discarded, and the price D35 accepts is that a person can
//! make their own agents worse and Koru will comply. The contract test in
//! `src/tests/contract.rs` pins the *bundled* text to the tool names and
//! folders P1.11 builds; it does not run against a person's copy.
//!
//! **Missing is not a fault.** A fresh install has no edited copy — the
//! package carries the file, and until somebody edits it the bundled text
//! and the shipped file are the same bytes — so a missing file resolves to
//! the bundled copy with **no notice**. The other four produce a
//! [`Notice`], because each is an edit somebody made that will not take
//! effect, and that is news.
//!
//! ## Where the run's paths come from
//!
//! The text names `.artifacts/<RunId>/` and `.koru/context/<RunId>/` with a
//! placeholder a reader understands and this crate never substitutes. **The
//! worker appends one line after the text** naming the run id and the two
//! absolute paths. Chosen over a `{{run_id}}` token because it keeps this
//! crate ignorant of the text — substitution is a parse, and a parse is the
//! beginning of a validator — and because an edited copy that lost the token
//! would otherwise lose the path silently. An appended line cannot be edited
//! away.
//!
//! ## Threads
//!
//! [`resolve`] reads the disk. The worker calls it on its own thread at run
//! start; it is never called on a thread that answers a person.
//!
//! ## Privacy
//!
//! A [`Resolved`]'s `Debug` prints the source, the byte count and the
//! notice, never the text — the text is provider payload. A [`Notice`]
//! carries a reason and, for `Oversized`, two counts; never a path. The
//! surface may show the path beside the sentence; the sentence does not
//! contain it.

use std::{
    borrow::Cow,
    fs::File,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

mod notice;
pub mod session;

pub use notice::Notice;

/// Where the file lives, relative to the installation's data directory —
/// the executables' directory on Windows and the bundle's `Resources/` on
/// macOS, as `koru_update::DATA_PREFIX` says. The release packager copies
/// [`SOURCE_PATH`] to exactly this path; the two are one agreement written
/// twice, and `koru-release`'s document test is what notices drift.
pub const RELATIVE_PATH: &str = "docs/prompts/delegated-agent.md";

/// Where the file lives in the repository, relative to the workspace root:
/// beside this crate's manifest. What the packager copies to
/// [`RELATIVE_PATH`], what [`BUNDLED`] compiles in, and what a development
/// build reads.
pub const SOURCE_PATH: &str = "crates/koru-delegate-preamble/delegated-agent.md";

/// The copy compiled into the program: the repository's file at build time.
pub const BUNDLED: &str = include_str!("../delegated-agent.md");

/// The most an edited copy may be, in bytes.
///
/// A provider's context is finite and the preamble is paid for on every
/// turn. The bundled text is under 4 KiB; 64 KiB is roughly sixteen thousand
/// tokens, which is more than any instruction sheet needs and less than
/// would quietly crowd the objective and the context out of a small model's
/// window. A file over it is a mistake — a document saved to the wrong name,
/// a paste that went wrong — not an unusually thorough edit, and the notice
/// says how far over it was.
pub const SIZE_LIMIT_BYTES: usize = 64 * 1024;

/// Which copy the text came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A file on disk, read and used as written — a session override, the
    /// copy beside the executables, or a development source. **Two words,
    /// not three**: what this says is
    /// *your file is in force*, and which build a developer is running is
    /// not a distinction to put in front of a person.
    Edited,
    /// The copy compiled into the program.
    Bundled,
}

/// The preamble the worker hands to the provider, and how it was found.
#[derive(Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The text read, or the resolver's compiled default. The legacy artifact
    /// resolver removes an initial UTF-8 byte-order mark; session text is
    /// preserved verbatim.
    pub text: Cow<'static, str>,
    pub source: Source,
    /// Set only when an edited copy existed and could not be used; the
    /// surface shows it once. A missing file is not a notice.
    pub notice: Option<Notice>,
    /// **The file this text came from**, and `None` when [`BUNDLED`] is in
    /// force — which is the whole of *what may be written back*.
    ///
    /// A surface that lets a person edit these instructions saves them by
    /// writing this path through [`write`], and offers no save at all when
    /// it is `None`: there is no file behind the built-in copy, and a save
    /// that quietly created one would put a second file in a search whose
    /// order is the point.
    ///
    /// Never printed — it is a path (§13.7), out of `Debug` for the same
    /// reason the text is.
    pub path: Option<PathBuf>,
}

impl std::fmt::Debug for Resolved {
    /// The text is provider payload and never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("source", &self.source)
            .field("bytes", &self.text.len())
            .field("notice", &self.notice)
            .finish()
    }
}

/// Find the preamble for a run: the copy under `installation_dir` if there
/// is one and it is objectively sound, else — in a development build — the
/// repository's own copy, else the bundled copy.
///
/// Reads the disk; call it off any thread that answers a person.
pub fn resolve(installation_dir: &Path) -> Resolved {
    resolve_within(installation_dir, repository_file().as_deref())
}

/// The search itself, with the development file *given* rather than found —
/// which is what lets a test stand a temporary file in for it, and what makes
/// `None` exactly a released build.
fn resolve_within(installation_dir: &Path, repository_file: Option<&Path>) -> Resolved {
    resolve_candidates(
        [
            Some(installation_dir.join(RELATIVE_PATH)),
            repository_file.map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten(),
        BUNDLED,
        read_edited,
    )
}

/// The precedence and fallback rule shared by the two independent files.
/// A reader is injected so callers' precedence tests need no filesystem.
fn resolve_candidates(
    paths: impl IntoIterator<Item = PathBuf>,
    default: &'static str,
    mut read: impl FnMut(&Path) -> Result<String, Fault>,
) -> Resolved {
    for path in paths {
        match read(&path) {
            Ok(text) => {
                return Resolved {
                    text: Cow::Owned(text),
                    source: Source::Edited,
                    notice: None,
                    path: Some(path),
                };
            }
            // Nothing there. Not news, and not the end of the search.
            Err(Fault::Missing) => continue,
            // Something is there and cannot be used. The search stops: a
            // person who edited this copy hears that their edit did not
            // take, instead of silently being handed another one.
            Err(Fault::Rejected(notice)) => return bundled(default, Some(notice)),
        }
    }
    bundled(default, None)
}

/// The repository's own copy — [`SOURCE_PATH`], beside this crate's manifest
/// — **and only in a development build**.
///
/// Compiled out of a release entirely rather than skipped at runtime, so a
/// released package carries no path into anybody's source tree and cannot
/// be talked into reading one.
#[cfg(debug_assertions)]
fn repository_file() -> Option<PathBuf> {
    Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("delegated-agent.md"))
}

/// A release ships the file beside the executables; there is no second
/// place to look.
#[cfg(not(debug_assertions))]
fn repository_file() -> Option<PathBuf> {
    None
}

fn bundled(default: &'static str, notice: Option<Notice>) -> Resolved {
    Resolved {
        text: Cow::Borrowed(default),
        source: Source::Bundled,
        notice,
        path: None,
    }
}

enum Fault {
    Missing,
    Rejected(Notice),
}

/// Why an edit could not be saved. Closed, content-free, and rendered as a
/// sentence by [`WriteFault::message`] — the same shape [`Notice`] has,
/// because a person who could not save needs the same kind of answer as one
/// whose file could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteFault {
    /// The text is nothing but whitespace. `resolve` would reject it on the
    /// next run and fall back, so writing it would be writing a file that
    /// is already known not to work.
    Empty,
    /// The text is larger than [`SIZE_LIMIT_BYTES`], which `resolve` would
    /// also refuse.
    Oversized { bytes: u64, limit: u64 },
    /// The file could not be written — permissions, a read-only
    /// installation, a full disk. **Nothing was changed.**
    Unwritable,
}

impl WriteFault {
    /// The sentence for `lang`. Never carries a path.
    pub fn message(self, lang: koru_i18n::Lang) -> String {
        use koru_i18n::{Key, fill, number::decimal, t};
        match self {
            Self::Empty => t(lang, Key::DelegatePreambleSaveEmpty).to_owned(),
            Self::Oversized { bytes, limit } => fill(
                t(lang, Key::DelegatePreambleSaveOversized),
                &[&decimal(lang, bytes), &decimal(lang, limit)],
            ),
            Self::Unwritable => t(lang, Key::DelegatePreambleSaveUnwritable).to_owned(),
        }
    }
}

/// Write an edited preamble back to the file it was resolved from.
///
/// **The two checks are the two `resolve` would make anyway** — empty and
/// oversized — applied *before* anything is written, so a save that would
/// produce a file the next run refuses is refused now instead, while the
/// text is still on screen. Encoding cannot fail (a Rust `str` is UTF-8) and
/// existence cannot fail (the caller passes the path a read succeeded at).
///
/// **A failure changes nothing.** The bytes go to a sibling `.part` file
/// with this process's id in its name and are renamed over the target, the
/// way `koru-export-markdown` writes: a crash or a refusal leaves the file
/// that was there, never half of a new one.
///
/// Writes the disk; call it off any thread that answers a person.
pub fn write(path: &Path, text: &str) -> Result<(), WriteFault> {
    if text.trim().is_empty() {
        return Err(WriteFault::Empty);
    }
    if text.len() > SIZE_LIMIT_BYTES {
        return Err(WriteFault::Oversized {
            bytes: text.len() as u64,
            limit: SIZE_LIMIT_BYTES as u64,
        });
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let part = path.with_file_name(format!("{file_name}.{}.part", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&part)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&part, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
        return Err(WriteFault::Unwritable);
    }
    Ok(())
}

/// The five objective checks, in the order they can be made: existence,
/// readability, size, encoding, emptiness.
fn read_edited(path: &Path) -> Result<String, Fault> {
    let text = decode(read_bytes(path)?)?;
    // Preserve the existing artifact editor's Notepad compatibility. The
    // common session resolver deliberately retains every valid text byte.
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// A bounded source read, independent of either document's text or precedence.
fn read_bytes(path: &Path) -> Result<Vec<u8>, Fault> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(Fault::Missing),
        // Permission denied, a directory on Windows, a path whose parent is
        // a file — every other refusal is "there is something there and it
        // cannot be read", which is the one thing a person needs to hear.
        Err(_) => return Err(Fault::Rejected(Notice::Unreadable)),
    };
    // The exact size when the filesystem will say; the bounded read below is
    // what actually protects memory, so a filesystem that reports zero for
    // something it then streams is still cut off.
    if let Ok(metadata) = file.metadata()
        && metadata.len() > SIZE_LIMIT_BYTES as u64
    {
        return Err(Fault::Rejected(Notice::Oversized {
            bytes: metadata.len(),
            limit: SIZE_LIMIT_BYTES as u64,
        }));
    }
    let mut bytes = Vec::new();
    // On Unix a directory opens and then refuses to be read; that lands here.
    if Read::by_ref(&mut file)
        .take(SIZE_LIMIT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(Fault::Rejected(Notice::Unreadable));
    }
    Ok(bytes)
}

/// Objective validity only. A BOM alone is not content; a valid file is
/// otherwise returned exactly as written, including its BOM and whitespace.
fn decode(bytes: Vec<u8>) -> Result<String, Fault> {
    if bytes.len() > SIZE_LIMIT_BYTES {
        return Err(Fault::Rejected(Notice::Oversized {
            bytes: bytes.len() as u64,
            limit: SIZE_LIMIT_BYTES as u64,
        }));
    }
    let text = String::from_utf8(bytes).map_err(|_| Fault::Rejected(Notice::BadEncoding))?;
    if text
        .strip_prefix('\u{feff}')
        .unwrap_or(&text)
        .trim()
        .is_empty()
    {
        return Err(Fault::Rejected(Notice::Empty));
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
