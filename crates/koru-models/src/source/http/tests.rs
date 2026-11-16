// SPDX-License-Identifier: MPL-2.0

//! **The resume suite.** Every one of these drives a real `reqwest` against a
//! real socket on loopback; none of them reaches the network.
//!
//! The bodies are deterministic pseudo-random rather than a repeated byte, on
//! purpose: a resume bug that concatenates 40 bytes onto 100 produces a file of
//! the wrong length, but a resume bug that writes the *wrong* 60 bytes at the
//! right offset produces a file of the right length — and only a body whose
//! every byte differs from its neighbours turns that into a digest failure the
//! test can name.

use super::*;
use crate::descriptor::{ModelVersion, Sha256Digest};
use crate::fixtures;
use crate::testserver::{FixtureServer, Reply};
use crate::token::ModelId;

const NAME: &str = "parakeet-1.2.0.zip";

/// A body whose bytes are all different from their neighbours — see the module
/// note.
fn body(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

/// A descriptor that describes exactly these bytes. Hashed rather than
/// hard-coded, so a change to the body is not a change to a constant somebody
/// has to remember.
fn describing(bytes: &[u8], scratch: &Path) -> ModelDescriptor {
    let sample = scratch.join("sample");
    fs::write(&sample, bytes).unwrap();
    let (size, digest) = Sha256Digest::of_file(&sample).unwrap();
    fs::remove_file(&sample).unwrap();
    ModelDescriptor {
        size,
        digest,
        ..fixtures::descriptor()
    }
}

struct Scene {
    _dir: tempfile::TempDir,
    parts: PathBuf,
    destination: PathBuf,
    descriptor: ModelDescriptor,
    source: HttpSource,
    bytes: Vec<u8>,
}

impl Scene {
    fn new(len: usize) -> Scene {
        let dir = tempfile::tempdir().unwrap();
        let parts = dir.path().join("parts");
        fs::create_dir_all(&parts).unwrap();
        let bytes = body(len);
        let descriptor = describing(&bytes, dir.path());
        Scene {
            destination: parts.join(format!("{}.archive", descriptor.digest)),
            parts,
            descriptor,
            source: HttpSource::new().unwrap(),
            bytes,
            _dir: dir,
        }
    }

    fn part(&self) -> PathBuf {
        self.parts.join(format!("{}.part", self.descriptor.digest))
    }

    fn fetch(&self, url: &Url) -> Result<Vec<InstallProgress>, ModelError> {
        let mut seen = Vec::new();
        let cancel = AtomicBool::new(false);
        let outcome = self.source.fetch(
            url,
            &self.descriptor,
            &self.destination,
            &mut |p| seen.push(p),
            &cancel,
        );
        outcome.map(|()| seen)
    }

    fn part_len(&self) -> u64 {
        self.part().metadata().map(|m| m.len()).unwrap_or(0)
    }

    fn arrived(&self) -> Vec<u8> {
        fs::read(&self.destination).unwrap()
    }
}

/// **The headline property.** A dropped connection leaves a partial, and the
/// next attempt asks for exactly the byte after it — not byte zero, and not
/// some rounded offset.
#[test]
fn a_truncated_download_resumes_from_the_exact_byte() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![
        Reply::ranged(&scene.bytes).truncated_after(1500),
        Reply::ranged(&scene.bytes),
    ]);
    let url = server.url(NAME);

    let first = scene.fetch(&url).unwrap_err();
    assert_eq!(first.code, ModelErrorCode::DownloadFailed);
    assert_eq!(
        scene.part_len(),
        1500,
        "the partial was deleted, so the next attempt is a restart"
    );
    assert!(
        !scene.destination.exists(),
        "an incomplete download must leave nothing a later read mistakes for an archive"
    );

    scene.fetch(&url).unwrap();
    assert_eq!(scene.arrived(), scene.bytes);
    scene.descriptor.verify_archive(&scene.destination).unwrap();

    let seen = server.seen();
    assert_eq!(
        seen[0].path,
        format!("/{NAME}"),
        "a resume that changed the URL is not a resume of the same thing"
    );
    assert_eq!(seen[1].path, seen[0].path);
    assert_eq!(seen[0].header("range"), None, "the first attempt is whole");
    assert_eq!(
        seen[1].header("range"),
        Some("bytes=1500-"),
        "the resume did not ask for the exact byte"
    );
}

/// **A server that answers `200` to a ranged request restarts cleanly.**
///
/// This is the case `Accept-Ranges` cannot warn about and the status can: a
/// client that appended would produce a file of the wrong length here and — on
/// a body whose length happened to work out — of the wrong content anywhere.
#[test]
fn a_two_hundred_to_a_ranged_request_restarts_rather_than_concatenating() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![
        Reply::ranged(&scene.bytes).truncated_after(1500),
        Reply::ignores_range(&scene.bytes),
    ]);
    let url = server.url(NAME);

    scene.fetch(&url).unwrap_err();
    assert_eq!(scene.part_len(), 1500);

    scene.fetch(&url).unwrap();
    assert_eq!(
        scene.arrived().len(),
        scene.bytes.len(),
        "the partial was concatenated onto the whole body"
    );
    assert_eq!(scene.arrived(), scene.bytes);
    assert_eq!(
        server.seen()[1].header("range"),
        Some("bytes=1500-"),
        "the client did not even try to resume"
    );
}

/// **`416` is handled**: our partial outlived the resource it was a prefix of,
/// so it is discarded and the whole thing is asked for again — inside the same
/// call, because a caller should not have to know what a `416` was.
#[test]
fn a_range_not_satisfiable_discards_the_partial_and_starts_again() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![
        Reply::ranged(&scene.bytes).truncated_after(1500),
        Reply::status(416),
        Reply::ranged(&scene.bytes),
    ]);
    let url = server.url(NAME);

    scene.fetch(&url).unwrap_err();
    assert_eq!(scene.part_len(), 1500);

    scene.fetch(&url).unwrap();
    assert_eq!(scene.arrived(), scene.bytes);

    let seen = server.seen();
    assert_eq!(seen[1].header("range"), Some("bytes=1500-"));
    assert_eq!(
        seen[2].header("range"),
        None,
        "after a 416 the request must carry no range at all"
    );
}

/// **A changed `ETag` discards the partial.**
///
/// The first line of defence is `If-Range`, which a correct server answers with
/// a `200`. This asserts the second: a server that changed the resource and
/// still answered `206` is refused rather than appended to, because appending
/// to a stale prefix is how a resume becomes silent corruption.
#[test]
fn a_changed_etag_discards_the_partial() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![
        Reply::ranged(&scene.bytes)
            .with_etag("\"v1\"")
            .truncated_after(1500),
        Reply::ranged(&scene.bytes).with_etag("\"v2\""),
        Reply::ranged(&scene.bytes).with_etag("\"v2\""),
    ]);
    let url = server.url(NAME);

    scene.fetch(&url).unwrap_err();
    assert_eq!(scene.part_len(), 1500);

    scene.fetch(&url).unwrap();
    assert_eq!(scene.arrived(), scene.bytes);

    let seen = server.seen();
    assert_eq!(
        seen[1].header("if-range"),
        Some("\"v1\""),
        "a resume that sends no validator is asking to be corrupted"
    );
    assert_eq!(seen[1].header("range"), Some("bytes=1500-"));
    assert_eq!(
        seen[2].header("range"),
        None,
        "the partial was kept across a validator change"
    );
}

/// A `Content-Range` that does not begin where we are is refused rather than
/// appended. The partial is left exactly as it was, so the next attempt is
/// still a resume.
#[test]
fn a_content_range_that_does_not_start_where_we_are_is_refused() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![
        Reply::ranged(&scene.bytes).truncated_after(1500),
        Reply::ranged(&scene.bytes).with_content_range("bytes 0-4095/4096"),
    ]);
    let url = server.url(NAME);

    scene.fetch(&url).unwrap_err();
    let error = scene.fetch(&url).unwrap_err();

    assert_eq!(error.code, ModelErrorCode::DownloadFailed);
    assert!(error.message.contains("starting at 0"), "{}", error.message);
    assert_eq!(scene.part_len(), 1500, "the partial was written into");
    assert!(!scene.destination.exists());
}

/// A `Content-Length` that disagrees with the signed size is refused before a
/// byte is written. Answerable from metadata, so paying for the download to
/// find out would be work nobody needed to do.
#[test]
fn a_declared_length_that_is_not_the_signed_size_is_refused_early() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![Reply::ranged(&body(5000))]);

    let error = scene.fetch(&server.url(NAME)).unwrap_err();

    assert_eq!(error.code, ModelErrorCode::SizeMismatch);
    assert_eq!(scene.part_len(), 0, "bytes were written before the check");
}

/// Cancelling leaves nothing a later read would mistake for a complete
/// download — the contract [`ModelSource::fetch`] states.
#[test]
fn a_cancelled_download_leaves_no_archive() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![Reply::ranged(&scene.bytes)]);
    let cancel = AtomicBool::new(true);

    let error = scene
        .source
        .fetch(
            &server.url(NAME),
            &scene.descriptor,
            &scene.destination,
            &mut |_| {},
            &cancel,
        )
        .unwrap_err();

    assert_eq!(error.code, ModelErrorCode::Cancelled);
    assert!(!scene.destination.exists());
}

/// The same rule the catalog applies at parse time, applied again at the moment
/// bytes would actually be fetched — because a store may be handed an entry
/// that never went through `Catalog::parse`.
#[test]
fn plain_http_is_refused_away_from_loopback() {
    let scene = Scene::new(64);
    let url = Url::parse("http://models.example.com/parakeet.zip").unwrap();

    let error = scene.fetch(&url).unwrap_err();

    assert_eq!(error.code, ModelErrorCode::DownloadFailed);
    assert!(error.message.contains("HTTPS"));
}

/// A finished download leaves the archive and nothing else: a `.part` or a
/// resume file left behind is a resume that would start from the wrong place.
#[test]
fn a_finished_download_leaves_no_resume_state() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![Reply::ranged(&scene.bytes).with_etag("\"v1\"")]);

    scene.fetch(&server.url(NAME)).unwrap();

    assert!(scene.destination.is_file());
    assert!(!scene.part().exists());
    assert!(
        !scene
            .parts
            .join(format!("{}.part.json", scene.descriptor.digest))
            .exists()
    );
}

/// Progress is reported against the signed total and is coalesced — the whole
/// of it, over a body small enough that a per-chunk emitter would still only
/// manage two events, so what is asserted is the *shape* rather than the count.
#[test]
fn progress_counts_from_what_was_already_there() {
    let scene = Scene::new(4096);
    let server = FixtureServer::start(vec![
        Reply::ranged(&scene.bytes).truncated_after(1500),
        Reply::ranged(&scene.bytes),
    ]);
    let url = server.url(NAME);

    scene.fetch(&url).unwrap_err();
    let seen = scene.fetch(&url).unwrap();

    assert!(seen.iter().all(|p| p.stage == InstallStage::Downloading));
    assert!(seen.iter().all(|p| p.total == scene.descriptor.size));
    assert_eq!(
        seen.first().map(|p| p.received),
        Some(1500),
        "a resume that reports zero makes the bar jump backwards"
    );
    assert_eq!(seen.last().map(|p| p.received), Some(scene.descriptor.size));
}

/// `Content-Range` parsing, including the `*` a server is allowed to send when
/// it does not know the total. Refusing what it cannot read is the point: the
/// alternative is guessing, and a guess here concatenates.
#[test]
fn content_range_is_read_or_refused() {
    assert_eq!(
        parse_content_range("bytes 40-99/100"),
        Some((40, Some(100)))
    );
    assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, None)));
    assert_eq!(
        parse_content_range(" bytes  40-99 / 100 "),
        Some((40, Some(100)))
    );
    assert_eq!(parse_content_range("items 40-99/100"), None);
    assert_eq!(parse_content_range("bytes 40-99"), None);
    assert_eq!(parse_content_range("bytes */100"), None);
    assert_eq!(parse_content_range(""), None);
}

/// The part file is named by the digest and by nothing else — not the URL, not
/// the process. That is what makes it survive a restart and be reusable from
/// another mirror.
#[test]
fn a_part_file_is_named_by_the_digest_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut descriptor = fixtures::descriptor();
    descriptor.id = ModelId::parse("other-model").unwrap();
    descriptor.version = ModelVersion::parse("9.9.9").unwrap();
    let (part, meta) = part_paths(dir.path(), &descriptor);

    assert_eq!(
        part.file_name().unwrap().to_string_lossy(),
        format!("{}.part", descriptor.digest)
    );
    assert_eq!(
        meta.file_name().unwrap().to_string_lossy(),
        format!("{}.part.json", descriptor.digest)
    );
}
