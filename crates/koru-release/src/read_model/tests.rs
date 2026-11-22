// SPDX-License-Identifier: MPL-2.0

//! **Nothing here reaches the network, Cloudflare, Python or a model.**
//!
//! What is expensive to get wrong is the same as for `voice-models` — the JSON
//! the store parses and the notices inside the zip — plus one thing voice does
//! not have: that the catalog really installs under the policy nightly uses.
//! All three are checked from fixture files this suite writes into a temporary
//! directory, as voice's suite does: three short byte strings stand in for the
//! converted model, and a row pinned to *them* stands in for the table. The
//! origin's answers are parameters ([`archive_source`]), so the publish
//! decision is a pure function with a literal on the other side.

use super::*;
use koru_models::{CatalogEntry, ModelErrorCode};
use table::{ConvertedFile, MODEL};

/// The SHA-256 a pin would carry.
fn digest_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

const FIXTURE_FILES: [(&str, &[u8]); 3] = [
    ("det.onnx", b"a detector, as far as this test knows"),
    ("rec.onnx", b"a recognizer, likewise"),
    ("rec_dict.txt", "a\nb\n\u{3042}\n\u{4e2d}\n".as_bytes()),
];

/// Write the three fixture outputs into `<work>/models` and answer the table's
/// row pinned to them, with its archive unpinned.
fn fixture(work: &Path) -> ReadModel {
    let models = convert::Layout::under(work).models;
    std::fs::create_dir_all(&models).unwrap();
    let mut outputs = Vec::new();
    for (name, bytes) in FIXTURE_FILES {
        std::fs::write(models.join(name), bytes).unwrap();
        outputs.push(ConvertedFile {
            name,
            size: bytes.len() as u64,
            sha256: leak(digest_of(bytes)),
        });
    }
    ReadModel {
        outputs: Box::leak(outputs.into_boxed_slice()),
        archive_size: 0,
        archive_sha256: "",
        ..MODEL
    }
}

fn options(work: &Path) -> Options {
    Options {
        work: work.to_path_buf(),
        clean: false,
        python: None,
        out: None,
        base_url: None,
        upload: false,
    }
}

fn zip_names(path: &Path) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .unwrap()
                .name()
                .unwrap()
                .into_owned()
        })
        .collect()
}

fn zip_entry(path: &Path, name: &str) -> String {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut out = String::new();
    entry.read_to_string(&mut out).unwrap();
    out
}

// ------------------------------------------------------------- the table

/// **The row is a descriptor the store accepts**, with the tokens the reader's
/// worker and the consent step are built against.
#[test]
fn the_table_row_builds_a_valid_descriptor() {
    let entry = catalog::entry(&MODEL, 31_000_000, &"a".repeat(64)).unwrap();
    let descriptor = &entry.descriptor;
    assert_eq!(descriptor.id.as_str(), "pp-ocrv6-small");
    assert_eq!(descriptor.engine.as_str(), "pp-ocr");
    assert_eq!(descriptor.tier.as_str(), "standard");
    let languages: Vec<&str> = descriptor
        .languages
        .iter()
        .map(|tag| tag.as_str())
        .collect();
    assert_eq!(languages, ["und-Latn", "zh", "ja"]);
    assert_eq!(descriptor.working_set_bytes, 1024 * 1024 * 1024);
    assert_eq!(descriptor.licence.as_str(), "Apache-2.0");
    // What a key will one day sign, rendered today.
    let canonical = String::from_utf8(descriptor.canonical_bytes().unwrap()).unwrap();
    assert!(
        canonical.starts_with("koru-model-descriptor/1\n"),
        "{canonical}"
    );
    assert!(canonical.contains("\nengine\tpp-ocr\n"), "{canonical}");
    assert!(
        canonical.contains("\nlanguages\tund-Latn,zh,ja\n"),
        "{canonical}"
    );
}

/// Every pin is a pin: 64 lowercase hex characters and a length. The archive
/// is pinned whole or not at all, because a key without a length cannot be
/// checked against the origin's answer.
#[test]
fn every_pin_in_the_table_is_a_digest_and_a_length() {
    for weight in MODEL.weights {
        assert!(
            table::is_digest(weight.sha256),
            "{}/{}",
            weight.part,
            weight.file
        );
        assert!(weight.size > 0);
        assert_eq!(weight.commit.len(), 40, "{}", weight.repository);
        assert!(
            weight
                .url()
                .starts_with("https://huggingface.co/PaddlePaddle/")
        );
        assert!(weight.url().contains(weight.commit));
    }
    for output in MODEL.outputs {
        assert!(table::is_digest(output.sha256), "{}", output.name);
        assert!(output.size > 0);
    }
    assert_eq!(
        table::is_digest(MODEL.archive_sha256),
        MODEL.archive_size > 0,
        "the archive's digest and length are pinned together"
    );
    assert!(
        !table::is_digest("A".repeat(64).as_str()),
        "uppercase is not a pin"
    );
}

/// The three files the reader's worker opens, and only those, are what is
/// converted and zipped — the worker resolves them by name beneath the
/// directory the store hands it.
#[test]
fn the_converted_files_are_the_three_the_worker_reads() {
    let names: Vec<&str> = MODEL.outputs.iter().map(|output| output.name).collect();
    assert_eq!(names, ["det.onnx", "rec.onnx", "rec_dict.txt"]);
    let parts: std::collections::BTreeSet<&str> =
        MODEL.weights.iter().map(|weight| weight.part).collect();
    assert_eq!(parts.into_iter().collect::<Vec<_>>(), ["det", "rec"]);
}

/// **The notice and the lock name one converter.** The lock is the pin; the
/// notice is the sentence Apache-2.0 asks for. If one moves without the other
/// the zip would describe a conversion that did not happen.
#[test]
fn the_notice_names_the_converter_the_lock_pins() {
    let lock = include_str!("../../read-model/requirements.lock");
    let pin = table::CONVERTER.replace(' ', "==");
    assert!(
        lock.lines()
            .any(|line| line.trim_end_matches(" \\").trim() == pin),
        "requirements.lock does not pin {pin}"
    );
    // The one wheel the conversion cannot do without (OCR QUESTIONS Q10).
    assert!(lock.contains("paddlepaddle-3.0.0.dev20250426-cp312-cp312-win_amd64.whl"));
    assert!(lock.contains(
        "--hash=sha256:c4dad498f278829e2352ca9d52cca09a11b121ea14a9ecea19a7b738b673381d"
    ));
}

/// **One copy of the pins.** Plan 0.1's script survives as a wrapper so its
/// reproduce command still works, and it must not grow a second copy of any
/// digest the table or the lock holds.
#[test]
fn the_old_script_holds_no_pin_of_its_own() {
    let script = include_str!("../../../../scripts/dev/read-model/convert.sh");
    let hex_runs = script
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|run| run.len() >= 40)
        .count();
    assert_eq!(hex_runs, 0, "convert.sh carries a digest or a commit");
    assert!(script.contains("read-model convert"));
}

// ------------------------------------------------------------ the archive

/// **The zip carries the three files, the licence and the notice**, in that
/// order, and nothing else. Apache-2.0 asks for the licence text (§4(a)) and a
/// statement of what was changed (§4(b)); both travel with the weights, as
/// voice's notices do.
#[test]
fn the_zip_carries_the_files_the_licence_and_the_notice() {
    let dir = tempfile::tempdir().unwrap();
    let model = fixture(dir.path());
    let zip = dir.path().join("model.zip");
    let packaged =
        archive::build(&model, &convert::Layout::under(dir.path()).models, &zip).unwrap();
    assert_eq!(packaged.size, std::fs::metadata(&zip).unwrap().len());
    assert_eq!(
        zip_names(&zip),
        [
            "det.onnx",
            "rec.onnx",
            "rec_dict.txt",
            "LICENCE.txt",
            "NOTICE.txt"
        ]
    );
    assert_eq!(
        zip_entry(&zip, "LICENCE.txt"),
        repack::licence_text("spdx/Apache-2.0.txt").unwrap()
    );
    assert!(zip_entry(&zip, "LICENCE.txt").starts_with("Apache License"));
    assert_eq!(
        zip_entry(&zip, "rec_dict.txt"),
        "a\nb\n\u{3042}\n\u{4e2d}\n"
    );

    let notice = zip_entry(&zip, "NOTICE.txt");
    assert!(
        notice.contains("Copyright (c) PaddlePaddle Authors."),
        "{notice}"
    );
    assert!(
        notice.contains("SPDX licence identifier: Apache-2.0"),
        "{notice}"
    );
    assert!(notice.contains(MODEL.attribution), "{notice}");
    assert!(notice.contains(table::CONVERTER), "{notice}");
    assert!(notice.contains("Changes made:"), "{notice}");
    for weight in MODEL.weights {
        assert!(notice.contains(&weight.url()), "{notice}");
        assert!(notice.contains(weight.sha256), "{notice}");
    }
    for output in model.outputs {
        assert!(notice.contains(output.sha256), "{notice}");
    }
}

/// **The same files always make the same zip**, which is what lets the table
/// pin the archive and a nightly name its key without building anything.
#[test]
fn packaging_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let model = fixture(dir.path());
    let models = convert::Layout::under(dir.path()).models;
    let one = archive::build(&model, &models, &dir.path().join("one.zip")).unwrap();
    let two = archive::build(&model, &models, &dir.path().join("two.zip")).unwrap();
    assert_eq!(one.digest, two.digest);
    assert_eq!(one.size, two.size);
}

/// **A converted file that differs from its pin is refused**, not zipped.
#[test]
fn an_output_that_differs_from_its_pin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let model = fixture(dir.path());
    let models = convert::Layout::under(dir.path()).models;
    std::fs::write(models.join("rec.onnx"), b"a different recognizer").unwrap();
    let error = archive::build(&model, &models, &dir.path().join("x.zip")).unwrap_err();
    assert!(error.contains("rec.onnx"), "{error}");
    assert!(error.contains("Refusing it"), "{error}");
    assert!(!dir.path().join("x.zip").exists());
}

/// **An unpinned output is never packaged**: a file nobody recorded must not
/// get a published key by being zipped.
#[test]
fn an_unpinned_output_is_never_packaged() {
    let dir = tempfile::tempdir().unwrap();
    let pinned = fixture(dir.path());
    let mut outputs = pinned.outputs.to_vec();
    outputs[0].sha256 = "";
    let model = ReadModel {
        outputs: Box::leak(outputs.into_boxed_slice()),
        ..pinned
    };
    let models = convert::Layout::under(dir.path()).models;
    let error = archive::build(&model, &models, &dir.path().join("x.zip")).unwrap_err();
    assert!(error.contains("no pinned SHA-256"), "{error}");
}

/// A zip whose three files matched but which hashes differently from the
/// archive pin is refused, and an unpinned archive is reported, not refused.
#[test]
fn an_archive_that_differs_from_its_pin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let model = fixture(dir.path());
    let models = convert::Layout::under(dir.path()).models;
    let packaged = archive::build(&model, &models, &dir.path().join("x.zip")).unwrap();
    check_archive_pin(&model, &packaged).unwrap();
    let pinned_elsewhere = ReadModel {
        archive_size: packaged.size,
        archive_sha256: leak("0".repeat(64)),
        ..model
    };
    let error = check_archive_pin(&pinned_elsewhere, &packaged).unwrap_err();
    assert!(error.contains("new version, not a new digest"), "{error}");
}

// ------------------------------------------------- the staged catalog installs

/// Package the fixture into `<dir>/out`, answering the row (now with its
/// archive pinned) and what was staged.
fn staged(dir: &Path, base_url: Option<&str>) -> (ReadModel, Staged) {
    let model = fixture(dir);
    let models = convert::Layout::under(dir).models;
    let packaged = archive::build(&model, &models, &zip_path(&model, dir)).unwrap();
    let model = ReadModel {
        archive_size: packaged.size,
        archive_sha256: leak(packaged.digest.clone()),
        ..model
    };
    let staged = stage(&model, &packaged, &dir.join("out"), base_url).unwrap();
    (model, staged)
}

/// **The publishing step's catalog verifies under the digest-only policy a
/// nightly uses** — not by re-implementing the check, but by installing it
/// with the model store's own `DiskModelStore`, opened by the reader's consent
/// record, and finding every converted file inside the installed directory.
#[test]
fn the_staged_catalog_installs_under_the_digest_only_policy_nightly_uses() {
    let dir = tempfile::tempdir().unwrap();
    let (model, staged) = staged(dir.path(), None);

    // The origin's layout, on disk.
    let out = dir.path().join("out");
    let archive = out.join(catalog::archive_key(&model, model.archive_sha256));
    assert!(archive.is_file());
    assert_eq!(staged.archive_dir, archive.parent().unwrap());
    assert_eq!(
        std::fs::read(out.join(catalog::CATALOG_KEY)).unwrap(),
        staged.catalog_bytes
    );
    let descriptor: CatalogEntry =
        serde_json::from_slice(&std::fs::read(out.join(catalog::descriptor_key(&model))).unwrap())
            .unwrap();
    assert_eq!(descriptor.descriptor.digest.as_str(), model.archive_sha256);
    assert_eq!(descriptor.descriptor.size, model.archive_size);

    let scratch = dir.path().join("scratch");
    let installed =
        verify_installs(&model, &staged.catalog_bytes, &staged.archive_dir, &scratch).unwrap();
    assert_eq!(installed.model, format!("{}/{}", model.id, model.version));
    assert_eq!(installed.files, 5);
    assert!(!scratch.exists(), "the scratch cache is removed");
}

/// **The same catalog is refused where it must be.** A stable build cannot
/// even choose the digest-only policy, and the signed policy — the only one
/// stable has — refuses an entry no pinned key signed. Nothing here changes
/// that; it is the state of the tree until a model-signing key exists.
#[test]
fn a_stable_build_still_refuses_the_unsigned_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let (_, staged) = staged(dir.path(), None);
    let catalog = Catalog::parse(&staged.catalog_bytes).unwrap();
    let entry = &catalog.entries[0];

    let error = TrustPolicy::digest_only_on(Some("stable")).unwrap_err();
    assert_eq!(error.code, ModelErrorCode::NoTrustRoot);

    let store = DiskModelStore::with_trust(
        ModelPaths::at(dir.path().join("cache")),
        Box::new(DirectorySource::new(&staged.archive_dir)),
        TrustPolicy::SIGNED,
        koru_voice_consent::READ,
    );
    let grant = koru_voice_consent::READ.recorded(entry.terms(), 0);
    let error = store
        .install(
            entry,
            TrustedKeys::pinned(),
            &grant,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.code, ModelErrorCode::NoTrustRoot, "{error}");
}

/// A grant somebody gave a speech model does not install the reader, even
/// though both are licence-compatible offers (OCR TECHNICAL D10).
#[test]
fn a_voice_grant_does_not_install_the_reader() {
    let dir = tempfile::tempdir().unwrap();
    let (_, staged) = staged(dir.path(), None);
    let catalog = Catalog::parse(&staged.catalog_bytes).unwrap();
    let entry = &catalog.entries[0];
    let store = DiskModelStore::with_trust(
        ModelPaths::at(dir.path().join("cache")),
        Box::new(DirectorySource::new(&staged.archive_dir)),
        TrustPolicy::digest_only_on(Some(NIGHTLY)).unwrap(),
        koru_voice_consent::READ,
    );
    let voice_grant = koru_voice_consent::VOICE.recorded(entry.terms(), 0);
    let error = store
        .install(
            entry,
            TrustedKeys::pinned(),
            &voice_grant,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.code, ModelErrorCode::ConsentRequired, "{error}");
}

/// `--base-url` rewrites only the catalog written to disk, for a debug build;
/// what was validated and what `publish` uploads keeps the origin's HTTPS URL.
#[test]
fn a_local_base_url_rewrites_only_the_staged_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let (model, staged) = staged(dir.path(), Some("http://127.0.0.1:8787/"));
    let on_disk: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("out").join(catalog::CATALOG_KEY)).unwrap(),
    )
    .unwrap();
    let key = catalog::archive_key(&model, model.archive_sha256);
    assert_eq!(
        on_disk["entries"][0]["url"].as_str().unwrap(),
        format!("http://127.0.0.1:8787/{key}")
    );
    let publishable = Catalog::parse(&staged.catalog_bytes).unwrap();
    assert_eq!(
        publishable.entries[0].url.as_str(),
        catalog::archive_url(&model, model.archive_sha256)
    );
}

// ---------------------------------------------------------- what is written

/// **The reader's catalog is its own**: its own key beside voice's, at the
/// address the installer will default to.
#[test]
fn the_catalog_is_the_readers_own_address() {
    assert_eq!(catalog::CATALOG_KEY, "read/catalog-v1.json");
    assert_ne!(catalog::CATALOG_KEY, voice::catalog::CATALOG_KEY);
    assert_eq!(
        catalog::catalog_url(),
        format!("https://{}/read/catalog-v1.json", crate::cloud::HOST)
    );
}

/// The archive and descriptor keys share voice's `models/` namespace, so the
/// reader's id must never be a speech model's.
#[test]
fn the_readers_id_never_collides_with_a_speech_model() {
    for artifact in voice::table::ARTIFACTS {
        assert_ne!(artifact.id, MODEL.id);
    }
    assert!(catalog::descriptor_key(&MODEL).starts_with("models/pp-ocrv6-small/"));
}

/// The entry names the key its archive is published under, and the
/// per-model document is one entry that parses on its own.
#[test]
fn the_entrys_url_is_the_key_the_archive_is_published_under() {
    let digest = "d".repeat(64);
    let documents = catalog::documents(&MODEL, 100, &digest).unwrap();
    let key = catalog::archive_key(&MODEL, &digest);
    assert!(key.ends_with(&format!("{digest}.zip")));
    assert_eq!(
        documents.entry.url.as_str(),
        format!("https://{}/{key}", crate::cloud::HOST)
    );
    let parsed: CatalogEntry = serde_json::from_slice(&documents.descriptor).unwrap();
    assert_eq!(parsed, documents.entry);
    let catalog = Catalog::parse(&documents.catalog).unwrap();
    assert_eq!(catalog.entries, vec![documents.entry]);
    assert_eq!(
        catalog.entries[0].signature.as_str(),
        voice::catalog::UNSIGNED
    );
}

// ----------------------------------------------------------------- publish

/// **What a publish does with the archive**, for every answer the origin and
/// this machine can give. Already published needs no file; a different length
/// under a content-addressed key is voice's refusal; missing from both is a
/// refusal that names the conversion, because a nightly never converts.
#[test]
fn a_publish_never_needs_to_convert() {
    let model = ReadModel {
        archive_size: 500,
        archive_sha256: leak("e".repeat(64)),
        ..MODEL
    };
    let key = || catalog::archive_key(&model, model.archive_sha256);

    let already = archive_source(&model, key(), Some(Some(500)), None).unwrap();
    assert!(!writes(&already));
    let unknown_length = archive_source(&model, key(), Some(None), None).unwrap();
    assert!(!writes(&unknown_length));

    let wrong = archive_source(&model, key(), Some(Some(499)), None).unwrap_err();
    assert!(wrong.contains("cannot both be right"), "{wrong}");

    let local = archive_source(&model, key(), None, Some(PathBuf::from("x.zip"))).unwrap();
    assert!(writes(&local));
    assert!(matches!(&local.body, voice::Body::File(path) if path == Path::new("x.zip")));
    assert_eq!(local.cache_control, voice::catalog::ARCHIVE_CACHE);

    let neither = archive_source(&model, key(), None, None).unwrap_err();
    assert!(neither.contains("never in a nightly"), "{neither}");
    assert!(neither.contains("read-model convert"), "{neither}");
}

/// An unpinned archive is refused before anything is asked of the network.
#[test]
fn a_publish_refuses_an_archive_the_table_has_not_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let model = fixture(dir.path());
    let error = publish(&model, &options(dir.path())).unwrap_err();
    assert!(error.contains("no pinned archive"), "{error}");
}

/// A zip on this machine is offered only when it matches the archive pin —
/// the one `package` left, or one rebuilt from outputs that match theirs.
#[test]
fn a_local_archive_is_used_only_when_it_matches_its_pin() {
    let dir = tempfile::tempdir().unwrap();
    let (model, _) = staged(dir.path(), None);
    let zip = zip_path(&model, dir.path());
    assert_eq!(
        local_archive(&model, dir.path()).unwrap(),
        Some(zip.clone())
    );

    // The zip gone, the outputs still verified: rebuilt to the same digest.
    std::fs::remove_file(&zip).unwrap();
    assert_eq!(
        local_archive(&model, dir.path()).unwrap(),
        Some(zip.clone())
    );

    // Neither: nothing to offer, and the caller refuses.
    std::fs::remove_file(&zip).unwrap();
    std::fs::remove_dir_all(convert::Layout::under(dir.path()).models).unwrap();
    assert_eq!(local_archive(&model, dir.path()).unwrap(), None);
}

// ----------------------------------------------------------------- convert

/// **The conversion says plainly where it cannot run.** On Windows x64 it may;
/// anywhere else the refusal names the platform and the verbs that do work.
#[test]
fn the_conversion_runs_only_on_windows_x64() {
    let answer = convert::supported_here();
    if cfg!(all(windows, target_arch = "x86_64")) {
        assert!(answer.is_ok());
    } else {
        let error = answer.unwrap_err();
        assert!(error.contains("runs only on Windows x64"), "{error}");
        assert!(error.contains("read-model package"), "{error}");
    }
}

/// **Nothing outside the lock may take part in a conversion.** paddle2onnx
/// switches polygraphy's auto-install on inside its own process, so the lock
/// must name every module the fold imports — onnx_graphsurgeon and ml_dtypes
/// were the two that slipped through before — and a package that arrives
/// anyway is named by the check that runs after converting.
#[test]
fn the_environment_check_names_any_package_the_lock_does_not_pin() {
    let lock = include_str!("../../read-model/requirements.lock");
    let everything_locked = "onnx_graphsurgeon==0.6.1\nml-dtypes==0.6.0\nPyYAML==6.0.3\n\
                             paddlepaddle==3.0.0.dev20250426\npip==25.0.1\n";
    assert!(convert::unlocked(lock, everything_locked).is_empty());
    let stray = "numpy==2.5.4\ncolored==2.3.0\nOnnx.GraphSurgeon==0.6.1\n";
    assert_eq!(convert::unlocked(lock, stray), ["colored"]);
    assert_eq!(
        convert::unlocked("numpy==1 \\\n    --hash=sha256:00\n", "Some_Other.pkg==1\n"),
        ["some-other-pkg"]
    );
}

/// A weight already on disk that does not match its pin is refused and left
/// alone; one that matches is used without a download.
#[test]
fn a_weight_that_does_not_match_its_pin_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let good = b"the bytes we pinned";
    let target = dir.path().join("inference.json");
    std::fs::write(&target, b"something else").unwrap();
    let url = "https://example.invalid/inference.json";
    let error =
        convert::ensure_weight(url, good.len() as u64, &digest_of(good), &target).unwrap_err();
    assert!(error.contains("not the pinned"), "{error}");
    assert_eq!(std::fs::read(&target).unwrap(), b"something else");

    std::fs::write(&target, good).unwrap();
    convert::ensure_weight(url, good.len() as u64, &digest_of(good), &target).unwrap();
}

/// **The record step refuses an output that differs** after measuring every
/// one, and leaves `SHA256SUMS` for the comparison two clean runs make.
#[test]
fn the_record_step_refuses_an_output_that_differs_from_its_pin() {
    let dir = tempfile::tempdir().unwrap();
    let model = fixture(dir.path());
    let models = convert::Layout::under(dir.path()).models;
    convert::record(&model, &models).unwrap();
    let sums = std::fs::read_to_string(models.join("SHA256SUMS")).unwrap();
    for output in model.outputs {
        assert!(
            sums.contains(&format!("{}  {}", output.sha256, output.name)),
            "{sums}"
        );
    }

    std::fs::write(models.join("det.onnx"), b"another detector").unwrap();
    let error = convert::record(&model, &models).unwrap_err();
    assert!(error.contains("det.onnx"), "{error}");
    assert!(error.contains("refused"), "{error}");
}

// -------------------------------------------------------------------- misc

/// The work directory is under `dist/`, which git ignores, and named in
/// `.gitignore` for the reason voice's is: a venv and 31 MB of weights must
/// never reach a commit.
#[test]
fn the_work_directory_is_ignored_by_git() {
    assert!(DEFAULT_WORK.starts_with("dist/"));
    let ignore = include_str!("../../../../.gitignore");
    assert!(ignore.lines().any(|line| line.trim() == "dist/read-model/"));
}

/// An explicit `--work` needs no git, and an unknown argument is refused with
/// the usage.
#[test]
fn options_parse_and_refuse_by_name() {
    let root = Path::new(".");
    let parsed = Options::parse(
        root,
        ["--work", "w", "--clean", "--upload"]
            .iter()
            .map(|s| s.to_string()),
    )
    .unwrap();
    assert_eq!(parsed.work, PathBuf::from("w"));
    assert!(parsed.clean && parsed.upload);
    let error = Options::parse(root, ["--nope"].iter().map(|s| s.to_string())).unwrap_err();
    assert!(error.contains("unexpected argument: --nope"), "{error}");
    assert!(error.contains("read-model convert"), "{error}");
}
