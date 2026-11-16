// SPDX-License-Identifier: MPL-2.0

//! **`.tar.bz2` in, `.zip` out, with the licence inside it.**
//!
//! Every speech model this product ships is published upstream as a
//! `.tar.bz2`, and `koru-voice-models` reads exactly one archive format:
//! `koru_update::extract_zip`, the hardened one, with `ArchiveLimits`,
//! traversal defences and a zip-bomb bound. Q-V50 settled which side gives way
//! and the reasoning is worth restating, because the tempting alternative looks
//! cheaper right up until it is deployed:
//!
//! - A signed `format` field on the descriptor costs a schema bump and a
//!   re-sign of every catalog entry, and buys the ability to serve somebody
//!   else's archive layout — which we do not do. ADR-250 already says models
//!   come from our own infrastructure under our own signature, so the
//!   publishing step is ours and already rewrites the bytes.
//! - **An extractor is a security boundary, and one is cheaper to defend than
//!   two.** A tar reader and a bzip2 decoder in `koru-voice-models` would put
//!   those defences in the tree twice, and the second copy is the one nobody
//!   reviews. Here they sit in a tool run by hand a few times a year, on a
//!   machine that already holds the release token.
//!
//! The cost is honest and is paid here: our origin holds a second copy of
//! ~2 GB of weights, and the provenance chain gains one link — *upstream
//! `.tar.bz2` → our zip → our descriptor*. That link is recorded where the
//! licence is recorded, in `NOTICE.txt`, because a repackaged CC-BY-4.0
//! artifact still carries its attribution and a repackaged OpenMDW-1.1 one
//! still has to carry the agreement.
//!
//! ## Two things about the zip that are not cosmetic
//!
//! **The tar's top-level directory is stripped.** `DiskModelStore` extracts
//! into `<id>/<version>/<digest>/` and the engine adapter resolves
//! `encoder.int8.onnx` directly beneath the directory it is handed. A zip that
//! kept the wrapper installs a model one level too deep, and the only symptom
//! is `has neither encoder.int8.onnx nor encoder.onnx` from a directory that
//! visibly contains one.
//!
//! **The zip is deterministic.** Fixed entry timestamps, fixed permissions, and
//! the tar's own order — so the same pinned upstream bytes always produce the
//! same digest. The digest *is* the published key, so a zip that varied
//! between runs would make every re-run a fresh upload of bytes already there,
//! and `ModelArtifact::repacked_sha256` would be a lie rather than a shortcut.

use super::table::{ModelArtifact, Shape};
use bzip2::read::BzDecoder;
use koru_update::strict_relative_path;
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

/// The name the licence text travels under, at the root of the model
/// directory. British spelling, matching `ModelDescriptor::licence`.
pub const LICENCE_FILE: &str = "LICENCE.txt";

/// Copyright, origin, and the repackaging itself.
pub const NOTICE_FILE: &str = "NOTICE.txt";

/// What `DiskModelStore` will refuse at install time, refused here instead.
///
/// The numbers are `disk.rs`'s `LIMITS`, restated rather than imported because
/// they are private there — and a publisher that shipped an archive the store
/// declines is a failure discovered by a person on a slow connection after
/// half a gigabyte.
const MAX_ENTRIES: usize = 10_000;
const MAX_EXTRACTED: u64 = 8 * 1024 * 1024 * 1024;

/// A repackaged archive, and the two facts the descriptor needs about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packaged {
    pub path: PathBuf,
    pub size: u64,
    pub digest: String,
}

/// **The licence texts, compiled in.**
///
/// `include_str!` rather than a runtime read, so a missing licence file is a
/// build error rather than a package that shipped without one. That is the
/// whole reason this is a `match` over names and not a directory listing: a
/// row naming a text nobody added must not compile.
pub fn licence_text(file: &str) -> Result<Cow<'static, str>, String> {
    let text = match file {
        "OpenMDW-1.1.txt" => Ok(include_str!("../../licences/OpenMDW-1.1.txt")),
        "CC-BY-4.0.txt" => Ok(include_str!("../../licences/CC-BY-4.0.txt")),
        "MIT-openai-whisper.txt" => Ok(include_str!("../../licences/MIT-openai-whisper.txt")),
        // The plain Apache-2.0 text, the same file `THIRD-PARTY-NOTICES.txt`
        // takes it from. Apache-2.0 carries no holder placeholder, so unlike
        // MIT one copy serves every model published under it — today the
        // reader's (`read_model::table`).
        "spdx/Apache-2.0.txt" => Ok(include_str!("../../licences/spdx/Apache-2.0.txt")),
        other => Err(format!(
            "no licence text is compiled in for {other}; add it under \
             crates/koru-release/licences/ and name it in repack::licence_text"
        )),
    }?;
    Ok(normalize_line_endings(text))
}

/// Make text embedded in a content-addressed archive independent of checkout
/// policy. Git may materialize an otherwise identical `.txt` as CRLF on
/// Windows; hashing those bytes directly would give the model a different key
/// on each platform.
pub(super) fn normalize_line_endings(text: &'static str) -> Cow<'static, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// **What OpenMDW-1.1 requires be retained, plus the link we added.**
///
/// The licence's only substantive condition is that a distribution retains a
/// copy of the agreement *and* all copyright and origin notices applicable to
/// it. CC-BY-4.0 asks for the creator, the notice, the licence and a statement
/// of changes; MIT asks for the copyright and permission notice. This file is
/// where all three are satisfied, and the repackaging is stated as a change
/// because it is one.
/// `upstream_digest` is the hash of the bytes actually in hand, **not**
/// `artifact.upstream_sha256`.
///
/// The two differ in exactly one situation, and it is the one that matters:
/// during `pin` the table field is still empty, so reading it there writes
/// `(unpinned)` into the notice, hashes *that* archive, and prints a
/// `repacked_sha256` for a zip nobody can build again - the moment the
/// upstream hash is pasted in the notice changes and so does the digest.
pub fn notice(artifact: &ModelArtifact, upstream_digest: &str) -> String {
    let mut out = String::new();
    out.push_str("This directory contains a machine-learning model redistributed by Koru.\n");
    out.push_str("The model is not Koru's work. Its licence is reproduced in ");
    out.push_str(LICENCE_FILE);
    out.push_str(".\n\n");
    for line in artifact.origin_notice {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\n---\n\nRedistribution by Koru\n\n");
    out.push_str(&format!("SPDX licence identifier: {}\n", artifact.licence));
    out.push_str(&format!("Attribution: {}\n", artifact.attribution));
    out.push_str(&format!("Upstream artifact: {}\n", artifact.url));
    out.push_str(&format!(
        "Upstream SHA-256: {}\n",
        if upstream_digest.is_empty() {
            "(unpinned)"
        } else {
            upstream_digest
        }
    ));
    match artifact.shape {
        Shape::TarBz2 => out.push_str(
            "Change made: the upstream .tar.bz2 was unpacked and repackaged as a\n\
             .zip, with its single top-level directory removed, so that Koru has\n\
             one archive reader rather than two. No file was altered, added or\n\
             removed apart from this notice and the licence text beside it.\n",
        ),
        Shape::BareFile => out.push_str(
            "Change made: the upstream weight file was placed in a .zip beside\n\
             this notice and the licence text. Its bytes are unchanged.\n",
        ),
    }
    out
}

/// Build the zip, and hash it.
///
/// `source` is the verified upstream download; `destination` is where the zip
/// is written, replacing whatever was there — a half-written zip from an
/// interrupted run must not be mistaken for a finished one, and the digest is
/// what would notice, so replacing is cheaper than reasoning about it.
pub fn repack(
    artifact: &ModelArtifact,
    source: &Path,
    destination: &Path,
    upstream_digest: &str,
) -> Result<Packaged, String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let output = File::create(destination).map_err(|error| error.to_string())?;
    let mut writer = zip::ZipWriter::new(BufWriter::new(output));

    let mut written: Vec<String> = Vec::new();
    match artifact.shape {
        Shape::TarBz2 => copy_tar_bz2(source, &mut writer, &mut written)?,
        Shape::BareFile => {
            let mut file = BufReader::new(File::open(source).map_err(|error| error.to_string())?);
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            add(&mut writer, &mut written, artifact.file_name, &bytes)?;
        }
    }

    // **Last, and refused rather than merged if the upstream already has one.**
    // Silently overwriting an upstream `LICENCE.txt` would destroy exactly the
    // notice this function exists to preserve.
    add(
        &mut writer,
        &mut written,
        LICENCE_FILE,
        licence_text(artifact.licence_file)?.as_bytes(),
    )?;
    add(
        &mut writer,
        &mut written,
        NOTICE_FILE,
        notice(artifact, upstream_digest).as_bytes(),
    )?;

    writer
        .finish()
        .map_err(|error| error.to_string())?
        .flush()
        .map_err(|error| error.to_string())?;

    let (size, digest) = hash_file(destination)?;
    Ok(Packaged {
        path: destination.to_path_buf(),
        size,
        digest,
    })
}

/// **A zip of exactly these entries, in this order, and its digest.**
///
/// The same per-entry rules as [`repack`] — fixed timestamps, fixed modes, the
/// install-time path check, no duplicate and the entry cap — for an archive
/// whose files are already in hand rather than inside a tar. The reader's
/// model (`read_model::archive`) is built through this, so both model
/// archives are deterministic for one reason rather than two.
pub(crate) fn zip_entries(
    destination: &Path,
    entries: &[(&str, &[u8])],
) -> Result<Packaged, String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let output = File::create(destination).map_err(|error| error.to_string())?;
    let mut writer = zip::ZipWriter::new(BufWriter::new(output));
    let mut written: Vec<String> = Vec::new();
    for (name, bytes) in entries {
        add(&mut writer, &mut written, name, bytes)?;
    }
    writer
        .finish()
        .map_err(|error| error.to_string())?
        .flush()
        .map_err(|error| error.to_string())?;
    let (size, digest) = hash_file(destination)?;
    Ok(Packaged {
        path: destination.to_path_buf(),
        size,
        digest,
    })
}

/// Every regular file in the tar, with its first path component removed.
fn copy_tar_bz2<W: Write + std::io::Seek>(
    source: &Path,
    writer: &mut zip::ZipWriter<W>,
    written: &mut Vec<String>,
) -> Result<(), String> {
    let file = File::open(source).map_err(|error| error.to_string())?;
    let mut archive = tar::Archive::new(BzDecoder::new(BufReader::new(file)));
    let mut expanded = 0u64;
    for entry in archive
        .entries()
        .map_err(|error| format!("{} is not a tar: {error}", source.display()))?
    {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            // Directories carry no bytes and `extract_zip` creates parents on
            // demand, so an explicit entry for one is noise that would also
            // have to be sorted and named.
            continue;
        }
        if !kind.is_file() {
            // A symlink or a hard link. `extract_zip` refuses both, so shipping
            // one would be publishing an archive that cannot be installed.
            return Err(format!(
                "{} contains a {kind:?} entry, which a Koru archive may not carry",
                source.display()
            ));
        }
        let path = entry
            .path()
            .map_err(|error| error.to_string())?
            .to_path_buf();
        let Some(name) = strip_root(&path) else {
            // A file at the tar's top level, beside the model directory rather
            // than inside it. Nothing published does this, and guessing what it
            // meant is how a licence file quietly disappears.
            return Err(format!(
                "{} has {} at its root; every entry must be inside one top-level directory",
                source.display(),
                path.display()
            ));
        };
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        expanded = expanded
            .checked_add(bytes.len() as u64)
            .ok_or("archive expanded-size overflow")?;
        if expanded > MAX_EXTRACTED {
            return Err(format!(
                "{} expands beyond the {MAX_EXTRACTED} bytes the model store allows",
                source.display()
            ));
        }
        add(writer, written, &name, &bytes)?;
    }
    Ok(())
}

/// Drop the first component. `None` when there is nothing left, which means the
/// entry was at the tar's root rather than inside the model directory.
fn strip_root(path: &Path) -> Option<String> {
    let mut parts = path.components();
    parts.next()?;
    let rest: PathBuf = parts.collect();
    let rest = rest.to_string_lossy().replace('\\', "/");
    if rest.is_empty() { None } else { Some(rest) }
}

/// One entry, with every rule that makes the zip deterministic and installable.
fn add<W: Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    written: &mut Vec<String>,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    // The same check `extract_zip` will apply at install time, applied here so
    // an unpublishable archive fails on the machine that built it.
    strict_relative_path(name)?;
    if written.iter().any(|existing| existing == name) {
        return Err(format!("{name} appears twice in the archive"));
    }
    if written.len() >= MAX_ENTRIES {
        return Err(format!(
            "the archive holds more than the {MAX_ENTRIES} entries the model store allows"
        ));
    }
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        // **Both of these are what make the digest reproducible.** A timestamp
        // read from the clock, or a mode read from whatever the tar happened to
        // carry, would give the same model a different digest on every run —
        // and the digest is the published key.
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    writer
        .start_file(name, options)
        .map_err(|error| error.to_string())?;
    writer.write_all(bytes).map_err(|error| error.to_string())?;
    written.push(name.to_string());
    Ok(())
}

/// Length and lowercase-hex SHA-256, streamed.
///
/// `koru_update::sha256_file` answers exactly this and is what
/// `Sha256Digest::of_file` uses, so the publisher and the installer compute the
/// same number the same way; this wrapper exists only to turn its error into
/// this module's.
pub fn hash_file(path: &Path) -> Result<(u64, String), String> {
    koru_update::sha256_file(path).map_err(|error| format!("{}: {error}", path.display()))
}
