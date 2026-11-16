// SPDX-License-Identifier: MPL-2.0

//! **`koru-release voice-models` — download the speech models, repackage them,
//! and publish them to Koru's own origin.**
//!
//! Four subcommands, and the split between them is which of the three
//! expensive, irreversible things each one is allowed to do:
//!
//! | | reads the origin | downloads gigabytes | **writes to production** |
//! | --- | --- | --- | --- |
//! | `plan` | yes, publicly | no | no |
//! | `pin` | no | yes | no |
//! | `fetch` | no | yes | no |
//! | `publish` | yes, publicly | only what is missing | **only with `--upload`** |
//!
//! **The default is a dry run**, in the same shape and for the same reason as
//! `release-index --dry-run`: everything the plan is built from is public, so
//! rehearsing a publish needs no Cloudflare token and is something anybody in
//! the tree can do. `updates.koru.do` is live, and a tool that could write to
//! it by being run without arguments is a tool that eventually does.
//!
//! ## What a re-run costs
//!
//! Nothing it does not have to. Three things make that true, and each is doing
//! separate work:
//!
//! - **The archive's key holds its digest**, so *present at the right length*
//!   is proof, and the check is a `HEAD` rather than a 475 MB `GET`.
//! - **The repacked digest is pinned in the table**, so every key can be
//!   computed with nothing downloaded at all. A publish where everything is
//!   already up costs four `HEAD`s and two `GET`s of a few kilobytes.
//! - **The repack is deterministic**, which is what makes the line above
//!   trustworthy rather than optimistic.
//!
//! The two JSON documents are compared byte for byte and skipped when
//! identical, so a re-publish after adding one model rewrites one descriptor
//! and the catalog and nothing else.
//!
//! ## What this tool deliberately does not do
//!
//! It does not sign anything, and it does not mint a key. `MODEL_SIGNING_KEYS`
//! is empty because holding a private half is an operations decision the owner
//! has deferred until before public release; the descriptor carries every field
//! that key would one day sign, and `catalog::UNSIGNED` is what sits in the
//! signature field until then. A publisher that generated its own keypair to
//! make the field look right would be manufacturing exactly the trust ADR-250
//! exists to refuse.

pub mod catalog;
pub mod repack;
pub mod table;

use crate::cloud::{Cloud, s3};
use fs2::FileExt;
use koru_voice_models::CatalogEntry;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use table::ModelArtifact;

const USAGE: &str = "usage:\n  \
     koru-release voice-models plan    [--work <dir>] [--only <id>]\n  \
     koru-release voice-models pin     [--work <dir>] [--only <id>] [--keep]\n  \
     koru-release voice-models fetch   [--work <dir>] [--only <id>]\n  \
     koru-release voice-models publish [--work <dir>] [--only <id>] [--upload]\n  \
     koru-release voice-models local   [--work <dir>] [--base-url <url>]";

/// Where downloads and repacked zips live by default.
///
/// Under the main checkout's `dist/`, shared by all linked worktrees and
/// gitignored. Half a gigabyte per row is paid once per developer machine.
const DEFAULT_WORK: &str = "dist/voice-models";

#[derive(Debug)]
struct Options {
    work: PathBuf,
    sources: Vec<PathBuf>,
    only: Option<&'static ModelArtifact>,
    upload: bool,
    keep: bool,
    /// Where a locally served archive will be reachable, for `local`.
    base_url: Option<String>,
}

impl Options {
    fn parse(root: &Path, mut args: impl Iterator<Item = String>) -> Result<Options, String> {
        let mut work = None;
        let mut only = None;
        let mut upload = false;
        let mut keep = false;
        let mut base_url = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--work" => work = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
                "--only" => {
                    let id = args.next().ok_or(USAGE)?;
                    only = Some(table::find(&id).ok_or_else(|| {
                        format!(
                            "no model is called {id}; this publisher knows {}",
                            table::ARTIFACTS
                                .iter()
                                .map(|artifact| artifact.id)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?);
                }
                "--upload" => upload = true,
                "--keep" => keep = true,
                "--base-url" => base_url = Some(args.next().ok_or(USAGE)?),
                other => return Err(format!("unexpected argument: {other}\n{USAGE}")),
            }
        }
        let (default_work, sources) = shared_work_and_sources(root)?;
        Ok(Options {
            work: work.unwrap_or(default_work),
            sources,
            only,
            upload,
            keep,
            base_url,
        })
    }

    /// The rows this run is about.
    fn rows(&self) -> Vec<&'static ModelArtifact> {
        match self.only {
            Some(artifact) => vec![artifact],
            None => table::product_artifacts().collect(),
        }
    }
}

/// Resolve the channel-independent model cache under the main checkout. Old
/// worktree-local caches remain read-only migration sources so their verified
/// bytes can seed the shared directory without another download.
fn shared_work_and_sources(root: &Path) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let common_output = git_output(root, &["rev-parse", "--git-common-dir"])?;
    let work = shared_work_from_common(root, common_output.trim())?;

    let listing = git_output(root, &["worktree", "list", "--porcelain"])?;
    let mut sources = Vec::new();
    for line in listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
    {
        let candidate = PathBuf::from(line).join(DEFAULT_WORK);
        if candidate != work && !sources.contains(&candidate) {
            sources.push(candidate);
        }
    }
    let current = root.join(DEFAULT_WORK);
    if current != work && !sources.contains(&current) {
        sources.insert(0, current);
    }
    Ok((work, sources))
}

pub(crate) fn shared_work_from_common(root: &Path, common: &str) -> Result<PathBuf, String> {
    shared_dir_from_common(root, common, DEFAULT_WORK)
}

/// `<main checkout>/<relative>`, from Git's common directory. The reader's
/// model publisher keeps its own work directory beside voice's for the same
/// reason ADR-254 gives: one verified copy per machine, not one per worktree.
pub(crate) fn shared_dir_from_common(
    root: &Path,
    common: &str,
    relative: &str,
) -> Result<PathBuf, String> {
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };
    let main = common
        .parent()
        .ok_or_else(|| format!("{} has no parent checkout", common.display()))?;
    Ok(main.join(relative))
}

pub(crate) fn git_output(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("running git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn lock_work(options: &Options) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(&options.work).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(options.work.join(".lock"))
        .map_err(|error| error.to_string())?;
    lock.lock_exclusive().map_err(|error| error.to_string())?;
    Ok(lock)
}

pub fn run(root: &Path, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let subcommand = args.next().ok_or(USAGE)?;
    let options = Options::parse(root, args)?;
    let _lock = if subcommand == "plan" {
        None
    } else {
        Some(lock_work(&options)?)
    };
    match subcommand.as_str() {
        "plan" => plan(&options),
        "pin" => pin(&options),
        "fetch" => fetch(&options),
        "publish" => publish(&options),
        "local" => local(&options),
        other => Err(format!("unknown subcommand: {other}\n{USAGE}")),
    }
}

/// Make the two product models and catalog available before a nightly release
/// itself is uploaded. Content-addressed objects are skipped on every re-run.
pub fn publish_for_nightly(root: &Path) -> Result<(), String> {
    let mut options = Options::parse(root, std::iter::empty())?;
    options.upload = true;
    let _lock = lock_work(&options)?;
    fetch(&options)?;
    publish(&options)
}

// ------------------------------------------------------------------- plan

/// What exists, what is pinned, and what a publish would write.
///
/// Reads the origin publicly and downloads nothing, so it is the command to
/// run first and the one that costs nothing to run again.
fn plan(options: &Options) -> Result<(), String> {
    let cloud = Cloud::public_reader()?;
    println!("[Koru] speech models — plan");
    println!("       work directory: {}", options.work.display());
    println!("       catalog:        {}", catalog::catalog_url());
    if let Some(warning) = catalog::catalog_host_warning() {
        println!("\n[Koru] NOTE: {warning}");
    }
    println!();

    let mut unpinned = 0;
    let mut to_upload = 0u64;
    for artifact in options.rows() {
        println!("  {} {}", artifact.id, artifact.version);
        println!(
            "    engine {}  tier {}  licence {}",
            artifact.engine, artifact.tier, artifact.licence
        );
        println!(
            "    upstream {} ({} bytes, sha256 {})",
            artifact.file_name,
            artifact.upstream_size,
            if artifact.is_pinned() {
                artifact.upstream_sha256
            } else {
                "UNPINNED — run `voice-models pin`"
            }
        );
        if !artifact.is_pinned() {
            unpinned += 1;
        }
        match known_digest(artifact, options)? {
            None => println!(
                "    archive  key unknown until the zip is built — run `voice-models fetch`"
            ),
            Some(packaged) => {
                let key = catalog::archive_key(artifact, &packaged.digest);
                let published = cloud.head_public(&key)?;
                let state = match published {
                    Some(Some(size)) if size == packaged.size => "already published".to_string(),
                    Some(Some(size)) => format!("PRESENT BUT {size} BYTES — investigate"),
                    Some(None) => "already published (length unknown)".to_string(),
                    None => {
                        to_upload += packaged.size;
                        format!("to upload, {} bytes", packaged.size)
                    }
                };
                println!("    archive  {key}");
                println!("             {state}");
                println!("    entry    {}", catalog::descriptor_key(artifact));
            }
        }
        println!();
    }
    println!("  catalog  {}", catalog::CATALOG_KEY);
    println!("\n[Koru] {to_upload} bytes of archive would be uploaded.");
    if unpinned > 0 {
        println!(
            "[Koru] {unpinned} row(s) carry no upstream SHA-256 and will not be fetched.\n       \
             `koru-release voice-models pin` downloads each once and prints the row to paste."
        );
    }
    Ok(())
}

// -------------------------------------------------------------------- pin

/// Download each artifact once, hash it, and print the table row.
///
/// **This is the only path that reads bytes nobody has vouched for**, and it is
/// separate from `fetch` for exactly that reason: a single command that fell
/// back to "download it unverified and trust it" the first time a row was empty
/// would make the pinning discipline optional, which is the same as not having
/// it. Nothing here publishes and nothing here is kept unless `--keep` says so.
fn pin(options: &Options) -> Result<(), String> {
    println!("[Koru] speech models — pin");
    println!(
        "       downloading each artifact once to measure it. Nothing is verified\n       \
         against anything, because there is nothing to verify against yet — that\n       \
         is what this command produces."
    );
    std::fs::create_dir_all(&options.work).map_err(|error| error.to_string())?;
    for artifact in options.rows() {
        println!("\n  {}", artifact.id);
        let target = options.work.join(artifact.file_name);
        let (size, digest) = download_and_hash(artifact.url, &target, options.keep)?;
        println!("    upstream_size:   {size},");
        println!("    upstream_sha256: \"{digest}\",");
        if size != artifact.upstream_size {
            println!(
                "    NOTE: the table says {} bytes. Upstream has changed under this row.",
                artifact.upstream_size
            );
        }
        if options.keep {
            let zip = zip_path(artifact, options);
            let packaged = repack::repack(artifact, &target, &zip, &digest)?;
            println!("    repacked_sha256: \"{}\",", packaged.digest);
            println!("    (repacked size {} bytes)", packaged.size);
        } else {
            println!(
                "    repacked_sha256 needs the archive on disk; re-run with --keep to get it."
            );
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ fetch

/// Download, verify, repackage. Publishes nothing.
fn fetch(options: &Options) -> Result<(), String> {
    println!("[Koru] speech models — fetch");
    std::fs::create_dir_all(&options.work).map_err(|error| error.to_string())?;
    for artifact in options.rows() {
        println!("\n  {} {}", artifact.id, artifact.version);
        let packaged = ensure_packaged(artifact, options)?;
        println!(
            "    zip {} ({} bytes)\n    sha256 {}",
            packaged.path.display(),
            packaged.size,
            packaged.digest
        );
        if artifact.repack_is_pinned() && artifact.repacked_sha256 != packaged.digest {
            return Err(format!(
                "{} repacked to {} but the table pins {}. Either the upstream bytes moved \
                 or the repack stopped being deterministic; both are worth understanding \
                 before anything is published.",
                artifact.id, packaged.digest, artifact.repacked_sha256
            ));
        }
        if !artifact.repack_is_pinned() {
            println!(
                "    repacked_sha256: \"{}\",  <- paste into the table",
                packaged.digest
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- publish

/// **The one command that can write to production, and only with `--upload`.**
///
/// Without the flag it prints exactly what it would write, to which keys, at
/// what total size, and stops. With it, it prints the same thing first and then
/// does it — so the record of what was about to happen is above the record of
/// what did.
fn publish(options: &Options) -> Result<(), String> {
    let rows = options.rows();
    if rows.iter().any(|artifact| !table::is_product(artifact)) {
        return Err("only product models can be published; research candidates may be pinned and fetched, but never enter the product catalog".into());
    }
    let cloud = cloud_for(options.upload)?;

    // Every product entry, whether or not `--only` narrowed the archive work:
    // a catalog that listed one model would silently withdraw the other one.
    let mut all_entries = Vec::new();
    for artifact in table::product_artifacts() {
        let Some(packaged) = known_digest(artifact, options)? else {
            return Err(format!(
                "{} has no repacked digest, so the catalog cannot be written. Run \
                 `koru-release voice-models fetch` (or fill in repacked_sha256).",
                artifact.id
            ));
        };
        let entry = entry_for(artifact, &packaged)?;
        all_entries.push((artifact, packaged, entry));
    }

    let catalog_document = catalog::catalog(
        all_entries
            .iter()
            .map(|(_, _, entry)| entry.clone())
            .collect(),
    )?;
    let catalog_bytes = catalog::render(&catalog_document)?;

    let mut work: Vec<Job> = Vec::new();
    for artifact in &rows {
        let (_, packaged, entry) = all_entries
            .iter()
            .find(|(candidate, _, _)| candidate.id == artifact.id)
            .expect("every row is in the full set");
        let key = catalog::archive_key(artifact, entry.descriptor.digest.as_str());
        let path = packaged.path.clone();
        let size = entry.descriptor.size;
        let published = cloud.head_public(&key)?;
        work.push(archive_job(key, path, size, published)?);
        let descriptor_key = catalog::descriptor_key(artifact);
        let descriptor_bytes = catalog::render(entry)?;
        let existing = cloud.fetch_public(&descriptor_key)?;
        work.push(document_job(descriptor_key, descriptor_bytes, existing));
    }
    let existing = cloud.fetch_public(catalog::CATALOG_KEY)?;
    work.push(document_job(
        catalog::CATALOG_KEY.to_string(),
        catalog_bytes,
        existing,
    ));

    announce(&work, options.upload);
    if let Some(warning) = catalog::catalog_host_warning() {
        println!("\n[Koru] NOTE: {warning}");
    }
    if !options.upload {
        println!(
            "\n[Koru] Nothing was written. Add --upload to publish this to {}.",
            crate::cloud::HOST
        );
        return Ok(());
    }
    upload(&cloud, &work)
}

/// **The client a publish talks to.** A dry run reads only what is already
/// public, so it needs no token. The upload path needs the real client, and
/// asking for the token here rather than lazily means a missing one is an
/// error before anything is fetched. Shared with `read-model publish`, so the
/// two model publishers cannot disagree about what `--upload` requires.
pub(crate) fn cloud_for(upload: bool) -> Result<Cloud, String> {
    if upload {
        let paths = crate::release_paths()?;
        let config: crate::cloud::CloudConfig = crate::read_json(&paths.cloud).map_err(|_| {
            "release cloud configuration is missing; run `koru-release cloud-setup` first"
                .to_string()
        })?;
        Cloud::from_config(crate::cloudflare_token()?, config)
    } else {
        Cloud::public_reader()
    }
}

/// **Write the jobs [`announce`] has already printed.** The one upload loop
/// both model publishers use, so the reader's model reaches the origin by
/// exactly the mechanism voice's does.
pub(crate) fn upload(cloud: &Cloud, work: &[Job]) -> Result<(), String> {
    println!("\n[Koru] writing to {} ...", crate::cloud::HOST);
    for job in work {
        match &job.body {
            Body::Skip(_) => continue,
            Body::File(path) => {
                cloud.put_file_any_size(&job.key, path, job.content_type, job.cache_control)?
            }
            Body::Bytes(bytes) => {
                println!("    uploading {} ({} bytes)", job.key, bytes.len());
                cloud.put_bytes(&job.key, bytes.clone(), job.content_type, job.cache_control)?
            }
        }
    }
    println!("[Koru] done.");
    Ok(())
}

/// One object the publish would write, or deliberately would not.
#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) key: String,
    pub(crate) size: u64,
    pub(crate) content_type: &'static str,
    pub(crate) cache_control: &'static str,
    pub(crate) body: Body,
}

#[derive(Debug)]
pub(crate) enum Body {
    File(PathBuf),
    Bytes(Vec<u8>),
    Skip(&'static str),
}

impl Job {
    fn file(
        key: String,
        path: PathBuf,
        size: u64,
        content_type: &'static str,
        cache_control: &'static str,
    ) -> Job {
        Job {
            key,
            size,
            content_type,
            cache_control,
            body: Body::File(path),
        }
    }

    fn skip(key: String, size: u64, why: &'static str) -> Job {
        Job {
            key,
            size,
            content_type: "",
            cache_control: "",
            body: Body::Skip(why),
        }
    }
}

/// **Is this archive already there — and if something else is, stop.**
///
/// `published` is whatever `HEAD` answered, kept as a parameter rather than
/// fetched here so the decision is a pure function with no socket in it: the
/// four cases below are exactly what a re-run turns on, and each of them is
/// testable from a literal.
///
/// The middle case is the one worth having. The key holds the digest, so an
/// object of a *different* length under it cannot be a correct earlier upload
/// of the same bytes — it is a truncated upload, a hash collision nobody should
/// believe in, or a key computed by something other than this table. Overwriting
/// it would erase the evidence, so it refuses instead.
pub(crate) fn archive_job(
    key: String,
    path: PathBuf,
    size: u64,
    published: Option<Option<u64>>,
) -> Result<Job, String> {
    match published {
        Some(Some(existing)) if existing == size => Ok(Job::skip(key, size, "already published")),
        Some(Some(existing)) => Err(format!(
            "{key} is already published at {existing} bytes but this build says {size}. \
             The key holds the digest, so those cannot both be right — stop and find out \
             which, rather than overwriting one with the other."
        )),
        Some(None) => Ok(Job::skip(key, size, "already published")),
        None => Ok(Job::file(
            key,
            path,
            size,
            "application/zip",
            catalog::ARCHIVE_CACHE,
        )),
    }
}

/// A JSON document, skipped when the origin already holds exactly these bytes.
///
/// A byte comparison rather than a digest: these are kilobytes, the fetch has
/// already happened, and *identical* is the only condition under which skipping
/// is safe. `existing` is passed in for the reason [`archive_job`] gives.
pub(crate) fn document_job(key: String, bytes: Vec<u8>, existing: Option<Vec<u8>>) -> Job {
    let size = bytes.len() as u64;
    if existing.as_deref() == Some(bytes.as_slice()) {
        return Job::skip(key, size, "unchanged");
    }
    Job {
        key,
        size,
        content_type: "application/json",
        cache_control: catalog::DOCUMENT_CACHE,
        body: Body::Bytes(bytes),
    }
}

/// **What will be written, before anything is.** The safety requirement, in one
/// place, so it cannot be printed by one path and skipped by another.
pub(crate) fn announce(work: &[Job], upload: bool) {
    println!(
        "\n[Koru] {}:\n",
        if upload {
            "about to write"
        } else {
            "would write (dry run)"
        }
    );
    let mut total = 0u64;
    for job in work {
        match &job.body {
            Body::Skip(why) => println!("    skip    {}  ({why})", job.key),
            Body::File(_) => {
                total += job.size;
                let route = if job.size > s3::MULTIPART_ABOVE_BYTES {
                    format!(
                        "S3 multipart, {} parts",
                        s3::parts(job.size, s3::PART_SIZE).len()
                    )
                } else {
                    "one PUT".to_string()
                };
                println!(
                    "    PUT     {}  {} bytes  [{route}]  {}",
                    job.key, job.size, job.cache_control
                );
            }
            Body::Bytes(_) => {
                total += job.size;
                println!(
                    "    PUT     {}  {} bytes  [one PUT]  {}",
                    job.key, job.size, job.cache_control
                );
            }
        }
    }
    println!("\n    total {total} bytes");
}

// ------------------------------------------------------------------ parts

/// The zip a row would produce, in the work directory.
fn zip_path(artifact: &ModelArtifact, options: &Options) -> PathBuf {
    options
        .work
        .join(format!("{}-{}.zip", artifact.id, artifact.version))
}

/// **The digest of this row's zip, at the cheapest source that knows it.**
///
/// The table first, because that costs nothing; then the zip already sitting in
/// the work directory; then nothing, which is a state a caller has to handle
/// rather than a reason to start a download somebody did not ask for.
fn known_digest(
    artifact: &ModelArtifact,
    options: &Options,
) -> Result<Option<repack::Packaged>, String> {
    let path = zip_path(artifact, options);
    if artifact.repack_is_pinned() {
        // The size still has to come from somewhere. When the zip is on disk it
        // is free; otherwise the descriptor's `size` is unknown and the row is
        // not usable for a publish — which is why `repacked_size` is not a
        // second pinned field: one number that can go stale is enough.
        if path.is_file() {
            let (size, digest) = repack::hash_file(&path)?;
            if digest == artifact.repacked_sha256 {
                return Ok(Some(repack::Packaged { path, size, digest }));
            }
            return Err(format!(
                "{} hashes to {digest}, not its pinned repacked digest {}. Run \
                 `voice-models fetch --only {}` to rebuild it.",
                path.display(),
                artifact.repacked_sha256,
                artifact.id
            ));
        }
    }
    if path.is_file() {
        let (size, digest) = repack::hash_file(&path)?;
        return Ok(Some(repack::Packaged { path, size, digest }));
    }
    let file_name = path.file_name().expect("zip has a file name");
    for source in &options.sources {
        let candidate = source.join(file_name);
        if !candidate.is_file() {
            continue;
        }
        let (size, digest) = repack::hash_file(&candidate)?;
        if artifact.repack_is_pinned() && digest != artifact.repacked_sha256 {
            continue;
        }
        return Ok(Some(repack::Packaged {
            path: candidate,
            size,
            digest,
        }));
    }
    Ok(None)
}

fn reuse_verified(source: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let part = target.with_extension("part");
    let _ = std::fs::remove_file(&part);
    if std::fs::hard_link(source, &part).is_err() {
        std::fs::copy(source, &part).map_err(|error| {
            format!(
                "copying {} to {}: {error}",
                source.display(),
                part.display()
            )
        })?;
    }
    std::fs::rename(&part, target).map_err(|error| error.to_string())
}

/// Download and repackage unless the zip is already here and correct.
fn ensure_packaged(
    artifact: &ModelArtifact,
    options: &Options,
) -> Result<repack::Packaged, String> {
    let zip = zip_path(artifact, options);
    if artifact.repack_is_pinned() && zip.is_file() {
        let (size, digest) = repack::hash_file(&zip)?;
        if digest == artifact.repacked_sha256 {
            println!("    zip already built and matches the pinned digest");
            return Ok(repack::Packaged {
                path: zip,
                size,
                digest,
            });
        }
    }
    if artifact.repack_is_pinned() {
        if let Some(packaged) = known_digest(artifact, options)? {
            println!("    reusing verified zip from {}", packaged.path.display());
            reuse_verified(&packaged.path, &zip)?;
            return Ok(repack::Packaged {
                path: zip,
                size: packaged.size,
                digest: packaged.digest,
            });
        }
    }
    let downloaded = ensure_downloaded(artifact, options)?;
    println!("    repacking as a zip");
    repack::repack(artifact, &downloaded, &zip, artifact.upstream_sha256)
}

/// The verified upstream artifact, downloading it only if it is not already
/// here and correct.
///
/// **Verified into place, never in place**: the download goes to a `.part`
/// beside the target and is renamed only after its size and hash match, which
/// is the same discipline `fetch-voice-runtime` follows. An interrupted
/// download must not leave a file the next run happily repackages.
fn ensure_downloaded(artifact: &ModelArtifact, options: &Options) -> Result<PathBuf, String> {
    if !artifact.is_pinned() {
        return Err(format!(
            "{} carries no upstream SHA-256, so it will not be fetched. Run \
             `koru-release voice-models pin --only {}` to measure it, check the value \
             against the publisher, and paste it into voice/table.rs.",
            artifact.id, artifact.id
        ));
    }
    let target = options.work.join(artifact.file_name);
    if target.is_file() {
        let (size, digest) = repack::hash_file(&target)?;
        if size == artifact.upstream_size && digest == artifact.upstream_sha256 {
            println!("    upstream archive already here and verified");
            return Ok(target);
        }
        // Somebody put it there and this tool is not the thing that should
        // decide it was wrong — the same rule `fetch-voice-runtime` follows.
        return Err(format!(
            "{} is already here but hashes to {digest} ({size} bytes), not the pinned \
             {} ({} bytes). Move it aside rather than letting this overwrite it.",
            target.display(),
            artifact.upstream_sha256,
            artifact.upstream_size
        ));
    }
    for source in &options.sources {
        let candidate = source.join(artifact.file_name);
        if !candidate.is_file() {
            continue;
        }
        let (size, digest) = repack::hash_file(&candidate)?;
        if size == artifact.upstream_size && digest == artifact.upstream_sha256 {
            println!(
                "    reusing verified upstream archive from {}",
                candidate.display()
            );
            reuse_verified(&candidate, &target)?;
            return Ok(target);
        }
    }
    let (size, digest) = download_and_hash(artifact.url, &target, true)?;
    if size != artifact.upstream_size || digest != artifact.upstream_sha256 {
        let _ = std::fs::remove_file(&target);
        return Err(format!(
            "{} downloaded as {size} bytes / {digest}, but the table pins {} bytes / {}. \
             Nothing was kept.",
            artifact.id, artifact.upstream_size, artifact.upstream_sha256
        ));
    }
    Ok(target)
}

/// Stream the artifact, hashing as it goes.
///
/// `keep` writes it to `target` through a `.part`; otherwise the bytes are
/// hashed and dropped, which is what `pin` wants — measuring an artifact should
/// not require half a gigabyte of disk somebody then has to remember to delete.
///
/// Takes the URL rather than a row so `read-model convert` fetches the
/// reader's official weights through this same streamer.
pub(crate) fn download_and_hash(
    url: &str,
    target: &Path,
    keep: bool,
) -> Result<(u64, String), String> {
    use sha2::{Digest, Sha256};
    println!("    downloading {url}");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        // No overall timeout: half a gigabyte on a domestic connection is
        // minutes, and a deadline here would fail the slow download rather
        // than the stuck one.
        .timeout(None)
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|error| format!("{url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let part = target.with_extension("part");
    let mut sink: Box<dyn Write> = if keep {
        if let Some(parent) = part.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        Box::new(std::io::BufWriter::new(
            std::fs::File::create(&part).map_err(|error| error.to_string())?,
        ))
    } else {
        Box::new(std::io::sink())
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    let mut announced = 0u64;
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("{url}: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        sink.write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
        total += read as u64;
        if total - announced >= 64 * 1024 * 1024 {
            announced = total;
            println!("      {total} bytes");
        }
    }
    sink.flush().map_err(|error| error.to_string())?;
    drop(sink);
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if keep {
        std::fs::rename(&part, target).map_err(|error| error.to_string())?;
    }
    Ok((total, digest))
}

/// The catalog entry for a row whose zip is measured.
/// **Write the catalog and its entries to disk, pointing at a local server.**
///
/// The store already accepts a *file* for `KORU_VOICE_CATALOG` and already
/// allows a loopback host in a debug build, and this is what makes those two
/// exceptions add up to something usable: a catalog on disk whose entries name
/// `http://127.0.0.1:<port>/...` installs a model that was never uploaded.
///
/// It is not a shortcut around the origin. Nothing here is signed differently,
/// verified less, or reachable from a release build — `require_https` allows
/// loopback only under `debug_assertions`, so a stable binary handed this exact
/// catalog refuses every row in it.
fn local(options: &Options) -> Result<(), String> {
    let base = options
        .base_url
        .clone()
        .unwrap_or_else(|| "http://127.0.0.1:8787".to_string());
    let base = base.trim_end_matches('/').to_string();
    println!("[Koru] speech models - local catalog");
    println!("       base URL: {base}");

    let mut entries = Vec::new();
    for artifact in table::product_artifacts() {
        let Some(packaged) = known_digest(artifact, options)? else {
            println!("  {} - no repacked digest yet, skipped", artifact.id);
            continue;
        };
        let entry = entry_for(artifact, &packaged)?;
        entries.push((artifact, packaged, entry));
    }
    if entries.is_empty() {
        return Err("no row has a repacked digest; run `voice-models fetch` first".into());
    }

    // Every archive has to be reachable under the key its entry names, so the
    // served directory mirrors the origin's layout rather than the work
    // directory's flat one.
    for (artifact, packaged, entry) in &entries {
        let key = catalog::archive_key(artifact, entry.descriptor.digest.as_str());
        let target = options.work.join("serve").join(&key);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::copy(&packaged.path, &target).map_err(|error| {
            format!(
                "copying {} to {}: {error}",
                packaged.path.display(),
                target.display()
            )
        })?;
        println!("  {} -> serve/{key}", artifact.id);
    }

    // Validate the complete publishable shape first, while every entry still
    // has the HTTPS URL `catalog::entry` gave it. The only thing local testing
    // changes is where the already-validated archive is fetched from. A
    // release-built publishing tool cannot validate loopback HTTP because the
    // product rule correctly allows that exception only in debug builds.
    let mut document = catalog::catalog(entries.iter().map(|(_, _, e)| e.clone()).collect())?;
    for ((artifact, _, _), entry) in entries.iter().zip(&mut document.entries) {
        let key = catalog::archive_key(artifact, entry.descriptor.digest.as_str());
        entry.url = format!("{base}/{key}")
            .parse()
            .map_err(|error| format!("{base}/{key} is not a URL: {error}"))?;
        entry.mirrors.clear();
    }
    let bytes = catalog::render(&document)?;
    let catalog_path = options.work.join("serve").join(catalog::CATALOG_KEY);
    if let Some(parent) = catalog_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(&catalog_path, &bytes).map_err(|error| error.to_string())?;

    let served = options.work.join("serve");
    println!(
        "
[Koru] catalog written.
"
    );
    println!(
        "  serve it:   python -m http.server 8787 --directory {}",
        served.display()
    );
    println!(
        "  then set:   KORU_VOICE_CATALOG={}",
        catalog_path.display()
    );
    println!(
        "
       The path is read directly, so the server is only needed for the archives."
    );
    println!("       A release build refuses a loopback URL; this is a debug-build path.");
    Ok(())
}

fn entry_for(
    artifact: &ModelArtifact,
    packaged: &repack::Packaged,
) -> Result<CatalogEntry, String> {
    catalog::entry(artifact, packaged.size, &packaged.digest)
}

#[cfg(test)]
mod tests;
