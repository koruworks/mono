// SPDX-License-Identifier: MPL-2.0

//! **`koru-release read-model` — convert the text-recognition model, package
//! it, and publish it beside the speech models.**
//!
//! The reader's model (PaddleOCR's PP-OCRv6 small detector and recognizer,
//! OCR TECHNICAL D1) is **our own conversion** of PaddlePaddle's official
//! weights to ONNX. Converting is the expensive, platform-bound step and it
//! happens when the model changes; publishing happens before every nightly.
//! So the verbs split exactly there:
//!
//! | | runs on | reads the origin | converts | **writes to production** |
//! | --- | --- | --- | --- | --- |
//! | `convert` | Windows x64 only | no | yes, and refuses an output that differs from the table | no |
//! | `package` | anywhere | no | no | no — stages the origin's files under `--out` |
//! | `publish` | anywhere | yes, publicly | no | **only with `--upload`** |
//!
//! **A nightly runs `publish --upload` and nothing else** ([`publish_for_nightly`],
//! beside voice's). The archive's key and length are pinned in the table, so a
//! nightly computes every key without any file, finds the archive already on
//! the origin, and writes only the two small JSON documents when they changed.
//! If the archive is *not* there — the first publish of a new version — the
//! verified zip has to be on this machine (left by `convert` and `package`);
//! otherwise it refuses, naming the step. **A nightly never converts.**
//!
//! What is pinned where, and why the zip's size is pinned when voice's is not,
//! is `table.rs`'s module note. What the zip carries is `archive.rs`'s. The
//! catalog's address and shape are `catalog.rs`'s.
//!
//! ## What this tool deliberately does not do
//!
//! What `voice-models` does not: sign anything, or mint a key.
//! `MODEL_SIGNING_KEYS` is empty, stable builds refuse every model install,
//! and the catalog carries `voice::catalog::UNSIGNED` in its signature field.
//! `package` proves the staged catalog installs under the digest-only policy a
//! nightly uses, by installing it into a scratch cache.

pub mod archive;
pub mod catalog;
pub mod convert;
pub mod table;

use crate::voice::{self, Job, repack};
use fs2::FileExt;
use koru_models::{
    Catalog, DirectorySource, DiskModelStore, ModelPaths, ModelStore, TrustPolicy, TrustedKeys,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use table::ReadModel;

const USAGE: &str = "usage:\n  \
     koru-release read-model convert [--work <dir>] [--clean] [--python <python.exe>]\n  \
     koru-release read-model package [--work <dir>] [--out <dir>] [--base-url <url>]\n  \
     koru-release read-model publish [--work <dir>] [--upload]";

/// The conversion's environment, the weights, the outputs and the zip, under
/// the main checkout's `dist/` — shared by every worktree and gitignored, as
/// voice's is (ADR-254).
pub const DEFAULT_WORK: &str = "dist/read-model";

/// The channel whose trust policy `package` proves the catalog under. Nightly
/// and beta both use the digest-only policy; stable refuses every unsigned
/// model, and that is not this tool's to change.
const NIGHTLY: &str = "nightly";

#[derive(Debug)]
struct Options {
    work: PathBuf,
    clean: bool,
    python: Option<String>,
    out: Option<PathBuf>,
    base_url: Option<String>,
    upload: bool,
}

impl Options {
    fn parse(root: &Path, mut args: impl Iterator<Item = String>) -> Result<Options, String> {
        let mut options = Options {
            work: PathBuf::new(),
            clean: false,
            python: None,
            out: None,
            base_url: None,
            upload: false,
        };
        let mut work = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--work" => work = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
                "--clean" => options.clean = true,
                "--python" => options.python = Some(args.next().ok_or(USAGE)?),
                "--out" => options.out = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
                "--base-url" => options.base_url = Some(args.next().ok_or(USAGE)?),
                "--upload" => options.upload = true,
                other => return Err(format!("unexpected argument: {other}\n{USAGE}")),
            }
        }
        options.work = match work {
            Some(work) => work,
            None => {
                let common = voice::git_output(root, &["rev-parse", "--git-common-dir"])?;
                voice::shared_dir_from_common(root, common.trim(), DEFAULT_WORK)?
            }
        };
        Ok(options)
    }
}

pub fn run(root: &Path, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let subcommand = args.next().ok_or(USAGE)?;
    let options = Options::parse(root, args)?;
    match subcommand.as_str() {
        "convert" => {
            // Refused before the lock is taken, so a machine that cannot
            // convert creates nothing at all.
            convert::supported_here()?;
            let _lock = lock_work(&options.work)?;
            let python = match &options.python {
                Some(program) => convert::Python {
                    program: program.clone(),
                    args: Vec::new(),
                },
                None => convert::Python::launcher(),
            };
            convert::run(root, &table::MODEL, &options.work, options.clean, &python)
        }
        "package" => {
            let _lock = lock_work(&options.work)?;
            package(&table::MODEL, &options)
        }
        "publish" => {
            let _lock = lock_work(&options.work)?;
            publish(&table::MODEL, &options)
        }
        other => Err(format!("unknown subcommand: {other}\n{USAGE}")),
    }
}

/// **Make the reader's model reachable before a nightly is uploaded**, beside
/// `voice::publish_for_nightly`. Publishes the already-converted,
/// digest-pinned archive and the two documents; never converts.
pub fn publish_for_nightly(root: &Path) -> Result<(), String> {
    let mut options = Options::parse(root, std::iter::empty())?;
    options.upload = true;
    let _lock = lock_work(&options.work)?;
    publish(&table::MODEL, &options)
}

fn lock_work(work: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(work).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(work.join(".lock"))
        .map_err(|error| error.to_string())?;
    lock.lock_exclusive().map_err(|error| error.to_string())?;
    Ok(lock)
}

/// The zip a row builds, in the work directory.
fn zip_path(model: &ReadModel, work: &Path) -> PathBuf {
    work.join(format!("{}-{}.zip", model.id, model.version))
}

// ---------------------------------------------------------------- package

/// **Zip the converted files, stage the origin's three objects under `--out`,
/// and prove the catalog installs under the digest-only policy.**
fn package(model: &ReadModel, options: &Options) -> Result<(), String> {
    let layout = convert::Layout::under(&options.work);
    let out = options
        .out
        .clone()
        .unwrap_or_else(|| options.work.join("serve"));
    println!("[Koru] text-recognition model — package");
    println!("       from: {}", layout.models.display());
    println!("       to:   {}", out.display());

    let packaged = archive::build(model, &layout.models, &zip_path(model, &options.work))?;
    println!(
        "\n  zip {} ({} bytes)\n  sha256 {}",
        packaged.path.display(),
        packaged.size,
        packaged.digest
    );
    check_archive_pin(model, &packaged)?;

    let staged = stage(model, &packaged, &out, options.base_url.as_deref())?;
    let scratch = options.work.join("verify-cache");
    let installed = verify_installs(model, &staged.catalog_bytes, &staged.archive_dir, &scratch)?;
    println!(
        "\n[Koru] verified: the staged catalog installs {} under the digest-only policy {NIGHTLY} uses\n       \
         ({} files checked in a scratch cache, since removed).",
        installed.model, installed.files
    );
    println!("\n[Koru] staged:");
    for path in &staged.written {
        println!("    {}", path.display());
    }
    if options.base_url.is_some() {
        println!(
            "\n[Koru] the catalog above names a local server: a debug build reads it through\n       \
             KORU_READ_CATALOG={} and a release build refuses it. Never upload this tree;\n       \
             `read-model publish` builds its own documents.",
            out.join(catalog::CATALOG_KEY).display()
        );
    }
    Ok(())
}

/// A zip that differs from its pin is refused; an unpinned one prints the
/// line to paste.
fn check_archive_pin(model: &ReadModel, packaged: &repack::Packaged) -> Result<(), String> {
    if model.archive_is_pinned() {
        if packaged.size != model.archive_size || packaged.digest != model.archive_sha256 {
            return Err(format!(
                "{} is {} bytes, sha256 {}; the table pins {} bytes, sha256 {}. The three \
                 files inside it matched their pins, so the zip itself changed (the notice, \
                 the licence text, or the zip writer); that is a new version, not a new digest.",
                packaged.path.display(),
                packaged.size,
                packaged.digest,
                model.archive_size,
                model.archive_sha256
            ));
        }
        println!("  matches the archive pinned in the table");
    } else {
        println!(
            "\n[Koru] the archive is unpinned. Paste into read_model/table.rs:\n    \
             archive_size: {},\n    archive_sha256: \"{}\",",
            packaged.size, packaged.digest
        );
    }
    Ok(())
}

/// What [`stage`] wrote, and what [`verify_installs`] needs from it.
struct Staged {
    /// The publishable catalog: what `publish` uploads, byte for byte.
    catalog_bytes: Vec<u8>,
    /// The directory holding `<digest>.zip`, the archive key's last segment.
    archive_dir: PathBuf,
    written: Vec<PathBuf>,
}

/// **The origin's layout, on disk**: `models/<id>/<version>/<digest>.zip`,
/// `models/<id>/<version>.json` and `read/catalog-v1.json` under `out`.
///
/// With `base_url`, the catalog written names that server instead of the
/// origin — voice's `local` verb, for a debug build — after the publishable
/// shape has been validated with its HTTPS URL.
fn stage(
    model: &ReadModel,
    packaged: &repack::Packaged,
    out: &Path,
    base_url: Option<&str>,
) -> Result<Staged, String> {
    let documents = catalog::documents(model, packaged.size, &packaged.digest)?;
    let archive_key = catalog::archive_key(model, &packaged.digest);
    let archive = out.join(&archive_key);
    let descriptor = out.join(catalog::descriptor_key(model));
    let catalog_path = out.join(catalog::CATALOG_KEY);
    for path in [&archive, &descriptor, &catalog_path] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    std::fs::copy(&packaged.path, &archive).map_err(|error| {
        format!(
            "copying {} to {}: {error}",
            packaged.path.display(),
            archive.display()
        )
    })?;
    std::fs::write(&descriptor, &documents.descriptor).map_err(|error| error.to_string())?;
    let catalog_bytes = match base_url {
        None => documents.catalog.clone(),
        Some(base) => {
            let base = base.trim_end_matches('/');
            let mut local = catalog::catalog(vec![documents.entry.clone()])?;
            for entry in &mut local.entries {
                entry.url = format!("{base}/{archive_key}")
                    .parse()
                    .map_err(|error| format!("{base}/{archive_key} is not a URL: {error}"))?;
                entry.mirrors.clear();
            }
            voice::catalog::render(&local)?
        }
    };
    std::fs::write(&catalog_path, &catalog_bytes).map_err(|error| error.to_string())?;
    Ok(Staged {
        catalog_bytes: documents.catalog,
        archive_dir: archive
            .parent()
            .expect("an archive key has a directory")
            .to_path_buf(),
        written: vec![archive, descriptor, catalog_path],
    })
}

/// What a successful verification installed.
#[derive(Debug)]
struct Installed {
    model: String,
    files: usize,
}

/// **Install the catalog the way a nightly desktop would, minus the network.**
///
/// The model store's own `DiskModelStore`, under
/// `TrustPolicy::digest_only_on(Some("nightly"))` and the reader's consent
/// record, fetching from the staged directory: it parses the catalog, validates
/// the descriptor, skips the signature (none exists), copies the archive,
/// checks its signed size and digest, extracts it with the hardened extractor
/// and publishes it. Then every converted file is checked inside the
/// installed directory, and the scratch cache is removed.
fn verify_installs(
    model: &ReadModel,
    catalog_bytes: &[u8],
    archive_dir: &Path,
    scratch: &Path,
) -> Result<Installed, String> {
    let fail = |error: koru_models::ModelError| error.to_string();
    let catalog = Catalog::parse(catalog_bytes).map_err(fail)?;
    let [entry] = catalog.entries.as_slice() else {
        return Err(format!(
            "the reader's catalog lists {} entries, not one",
            catalog.entries.len()
        ));
    };
    if scratch.exists() {
        std::fs::remove_dir_all(scratch).map_err(|error| error.to_string())?;
    }
    let result = (|| {
        let store = DiskModelStore::with_trust(
            ModelPaths::at(scratch),
            Box::new(DirectorySource::new(archive_dir)),
            TrustPolicy::digest_only_on(Some(NIGHTLY)).map_err(fail)?,
            koru_voice_consent::READ,
        );
        let grant = koru_voice_consent::READ.recorded(entry.terms(), 0);
        let installed = store
            .install(
                entry,
                TrustedKeys::pinned(),
                &grant,
                &mut |_| {},
                &AtomicBool::new(false),
            )
            .map_err(fail)?;
        let mut files = 0;
        for output in model.outputs {
            let path = installed.file(output.name).map_err(fail)?;
            archive::verify_output(output.name, output.size, output.sha256, &path)?;
            files += 1;
        }
        for notice in [repack::LICENCE_FILE, repack::NOTICE_FILE] {
            if !installed.file(notice).map_err(fail)?.is_file() {
                return Err(format!("the installed model has no {notice}"));
            }
            files += 1;
        }
        Ok(Installed {
            model: format!(
                "{}/{}",
                installed.descriptor.id, installed.descriptor.version
            ),
            files,
        })
    })();
    let _ = std::fs::remove_dir_all(scratch);
    result
}

// ---------------------------------------------------------------- publish

/// **The one command that can write to production, and only with `--upload`.**
///
/// The same shape as `voice-models publish`, and the same functions: the
/// archive's and the documents' skip-or-write decisions are
/// `voice::archive_job` and `voice::document_job`, the announcement is
/// `voice::announce`, and the writing is `voice::upload`.
fn publish(model: &ReadModel, options: &Options) -> Result<(), String> {
    if !model.archive_is_pinned() {
        return Err(format!(
            "{} has no pinned archive, so its key cannot be named. Run `read-model convert` \
             (Windows x64) and `read-model package`, then paste the archive's size and digest \
             into read_model/table.rs.",
            model.id
        ));
    }
    let cloud = voice::cloud_for(options.upload)?;
    println!("[Koru] text-recognition model — publish");
    println!("       catalog: {}", catalog::catalog_url());

    let documents = catalog::documents(model, model.archive_size, model.archive_sha256)?;
    let key = catalog::archive_key(model, model.archive_sha256);
    let published = cloud.head_public(&key)?;
    let local = if published.is_none() {
        local_archive(model, &options.work)?
    } else {
        None
    };
    let mut work = vec![archive_source(model, key, published, local)?];
    let descriptor_key = catalog::descriptor_key(model);
    let existing = cloud.fetch_public(&descriptor_key)?;
    work.push(voice::document_job(
        descriptor_key,
        documents.descriptor,
        existing,
    ));
    let existing = cloud.fetch_public(catalog::CATALOG_KEY)?;
    work.push(voice::document_job(
        catalog::CATALOG_KEY.to_string(),
        documents.catalog,
        existing,
    ));

    voice::announce(&work, options.upload);
    if !options.upload {
        println!(
            "\n[Koru] Nothing was written. Add --upload to publish this to {}.",
            crate::cloud::HOST
        );
        return Ok(());
    }
    voice::upload(&cloud, &work)
}

/// **Where the archive comes from, or why it cannot be published.**
///
/// `published` is what `HEAD` answered and `local` is a verified zip on this
/// machine, both passed in so the decision is a pure function, like
/// `voice::archive_job` which it ends in. Already on the origin needs no file;
/// missing from both is a refusal that names the step, because the one way to
/// make this zip is a conversion, and a nightly does not convert.
fn archive_source(
    model: &ReadModel,
    key: String,
    published: Option<Option<u64>>,
    local: Option<PathBuf>,
) -> Result<Job, String> {
    match (&published, local) {
        (Some(_), local) => voice::archive_job(
            key,
            local.unwrap_or_default(),
            model.archive_size,
            published,
        ),
        (None, Some(path)) => voice::archive_job(key, path, model.archive_size, None),
        (None, None) => Err(format!(
            "{key} is not on the origin, and no verified copy of it is on this machine. It is \
             made by converting, which runs on Windows x64 when the model's version changes and \
             never in a nightly: run `koru-release read-model convert` and \
             `koru-release read-model package` there, then `read-model publish --upload`."
        )),
    }
}

/// A zip on this machine that matches the archive pin: the one `package` left,
/// or one built now from converted files that match theirs. `None` when
/// neither exists.
fn local_archive(model: &ReadModel, work: &Path) -> Result<Option<PathBuf>, String> {
    let zip = zip_path(model, work);
    let matches = |path: &Path| -> Result<bool, String> {
        let (size, digest) = repack::hash_file(path)?;
        Ok(size == model.archive_size && digest == model.archive_sha256)
    };
    if zip.is_file() && matches(&zip)? {
        return Ok(Some(zip));
    }
    let models = convert::Layout::under(work).models;
    let all_converted = model.outputs.iter().all(|output| {
        archive::verify_output(
            output.name,
            output.size,
            output.sha256,
            &models.join(output.name),
        )
        .is_ok()
    });
    if !all_converted {
        return Ok(None);
    }
    let packaged = archive::build(model, &models, &zip)?;
    check_archive_pin(model, &packaged)?;
    Ok(Some(packaged.path))
}

/// Whether a job will write anything. Used by the tests; the publish itself
/// reads the same field through `voice::announce`.
#[cfg(test)]
fn writes(job: &Job) -> bool {
    !matches!(job.body, voice::Body::Skip(_))
}

#[cfg(test)]
mod tests;
