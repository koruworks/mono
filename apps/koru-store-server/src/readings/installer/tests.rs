// SPDX-License-Identifier: MPL-2.0

//! **The installer, end to end on a temporary directory**: a catalog file and
//! a model archive staged on disk, installed through the model store's own
//! `DirectorySource` — no network, no model loaded (the archive holds three
//! small stand-in files), the thread driven by hand.

use std::io::Write as _;
use std::sync::Mutex;

use koru_models::{
    CATALOG_SCHEMA_VERSION, DESCRIPTOR_SCHEMA_VERSION, DirectorySource, LanguageTag, LicenceRef,
    ModelDescriptor, ModelId, ModelSignature, ModelVersion, Sha256Digest, Token,
};
use koru_store_api::routes::reader::ReaderService;
use koru_store_core::ids::WorkspaceId;
use koru_store_wire::reader::{InstallFrom, ReaderUnavailable};

use super::*;

/// A staged reader: a catalog file, the archive it names, and the entry.
pub(crate) struct Staged {
    pub dir: tempfile::TempDir,
    pub catalog: PathBuf,
    pub archives: PathBuf,
    pub entry: CatalogEntry,
}

/// Stage a model whose archive holds `det.onnx`, `rec.onnx` and
/// `rec_dict.txt` — stand-ins, never loaded.
pub(crate) fn stage() -> Staged {
    let dir = tempfile::tempdir().unwrap();
    let archives = dir.path().join("archives");
    std::fs::create_dir_all(&archives).unwrap();
    let archive = archives.join("pp-ocrv6-small-test.zip");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        for (name, body) in [
            ("det.onnx", "detector"),
            ("rec.onnx", "recognizer"),
            ("rec_dict.txt", "a\nb\n"),
        ] {
            zip.start_file::<_, ()>(name, zip::write::FileOptions::default())
                .unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    let (size, digest) = Sha256Digest::of_file(&archive).unwrap();
    let entry = CatalogEntry {
        descriptor: ModelDescriptor {
            schema_version: DESCRIPTOR_SCHEMA_VERSION,
            id: ModelId::parse("pp-ocrv6-small").unwrap(),
            version: ModelVersion::parse("onnx-test").unwrap(),
            engine: Token::parse(READER_ENGINE).unwrap(),
            tier: Token::parse(READER_TIER).unwrap(),
            languages: ["und-Latn", "zh", "ja"]
                .into_iter()
                .map(|tag| LanguageTag::parse(tag).unwrap())
                .collect(),
            size,
            digest,
            working_set_bytes: 1 << 30,
            licence: LicenceRef::parse("Apache-2.0").unwrap(),
            attribution: "PaddleOCR".into(),
        },
        signature: ModelSignature::parse("unsigned").unwrap(),
        url: url::Url::parse("https://updates.koru.do/read/pp-ocrv6-small-test.zip").unwrap(),
        mirrors: Vec::new(),
    };
    let catalog = dir.path().join("catalog-v1.json");
    std::fs::write(
        &catalog,
        serde_json::to_vec(&Catalog {
            schema_version: CATALOG_SCHEMA_VERSION,
            entries: vec![entry.clone()],
        })
        .unwrap(),
    )
    .unwrap();
    Staged {
        dir,
        catalog,
        archives,
        entry,
    }
}

impl Staged {
    /// The configuration a nightly desktop would have, pointed here.
    pub(crate) fn config(&self, source: Box<dyn ModelSource>) -> InstallerConfig {
        let (trust, no_trust_root) =
            trust_from(false, TrustPolicy::digest_only_on(Some("nightly")));
        InstallerConfig {
            models: ModelPaths::at(self.dir.path().join("models")),
            state_dir: self.dir.path().join("reader"),
            catalog: CatalogSource::parse(self.catalog.to_str().unwrap()),
            trust,
            no_trust_root,
            source: Some(source),
        }
    }

    pub(crate) fn directory(&self) -> Box<dyn ModelSource> {
        Box::new(DirectorySource::new(&self.archives))
    }
}

/// A hub with an installer and a coordinator's knowledge of a worker that can
/// recognize, the way a desktop has once its coordinator has probed.
pub(crate) fn desktop(staged: &Staged) -> (Arc<ReaderHub>, Worker, Installer) {
    let hub = ReaderHub::new();
    let config = staged.config(staged.directory());
    hub.set_installing_here(config.no_trust_root);
    hub.attach_coordinator();
    hub.publish_worker(super::super::hub::WorkerKnowledge {
        built: Some(true),
        ..Default::default()
    });
    let mut worker = Worker::new(config, Arc::clone(&hub), Inbox::new());
    worker.open();
    (Arc::clone(&hub), worker, Installer::idle(hub))
}

/// Run every command the hub holds, as the thread would.
pub(crate) fn drain(hub: &ReaderHub, worker: &mut Worker) {
    while let Some(command) = hub.next_command(Duration::ZERO) {
        worker.run(command);
    }
}

fn offer_terms(installer: &Installer, hub: &ReaderHub, worker: &mut Worker) -> ReaderTerms {
    let _ = installer.state();
    drain(hub, worker);
    installer.state().offer.expect("the catalog was read")
}

use koru_store_wire::reader::ReaderTerms;

/// **An install under a root, from a local catalog**: the terms come from the
/// catalog, the consent is recorded in `read-consent.conf`, the model lands in
/// the root's cache and the disk is what says it is installed.
#[test]
fn an_install_from_a_local_catalog() {
    let staged = stage();
    let (hub, mut worker, installer) = desktop(&staged);
    let terms = offer_terms(&installer, &hub, &mut worker);
    assert_eq!(terms.download_bytes, staged.entry.descriptor.size);
    assert_eq!(terms.licence, "Apache-2.0");
    assert_eq!(terms.languages, ["und-Latn", "zh", "ja"]);
    assert_eq!(installer.state().catalog.state, CatalogStage::Ready);

    let started = installer
        .install(&InstallReader {
            terms: terms.clone(),
            from: InstallFrom::Offer,
        })
        .unwrap();
    assert_eq!(
        started.install.as_ref().map(|p| p.stage),
        Some(InstallStage::Starting),
        "the request returns at once; the work is the thread's"
    );
    drain(&hub, &mut worker);
    let state = installer.state();
    assert_eq!(state.install.unwrap().stage, InstallStage::Installed);
    let installed = state.installed.expect("installed");
    assert_eq!(installed.model, "pp-ocrv6-small");
    let model = hub.installed().unwrap();
    for file in ["det.onnx", "rec.onnx", "rec_dict.txt"] {
        assert!(model.file(file).unwrap().is_file(), "{file}");
    }
    assert!(model.root.starts_with(staged.dir.path().join("models")));
    let consent = koru_voice_consent::READ
        .read(&koru_voice_consent::READ.path_in(&staged.dir.path().join("reader")));
    assert!(
        consent
            .granted()
            .is_some_and(|granted| granted.terms().download_bytes == terms.download_bytes)
    );

    // A second installer over the same disk finds it: installed is derived.
    let (again, _worker, _) = desktop(&staged);
    assert!(again.installed().is_some());
}

/// **Stop, then Turn on again**: a stopped download is not a failure, and
/// the next install carries on to the end.
#[test]
fn a_stopped_download_is_stopped_and_the_next_install_finishes() {
    let staged = stage();
    let gate = Gate {
        stopped_once: Mutex::new(false),
        inner: DirectorySource::new(&staged.archives),
    };
    let hub = ReaderHub::new();
    let config = staged.config(Box::new(gate));
    hub.set_installing_here(false);
    hub.attach_coordinator();
    hub.publish_worker(super::super::hub::WorkerKnowledge {
        built: Some(true),
        ..Default::default()
    });
    let mut worker = Worker::new(config, Arc::clone(&hub), Inbox::new());
    worker.open();
    let installer = Installer::idle(Arc::clone(&hub));
    let terms = offer_terms(&installer, &hub, &mut worker);
    installer
        .install(&InstallReader {
            terms: terms.clone(),
            from: InstallFrom::Offer,
        })
        .unwrap();
    let command = hub.next_command(Duration::ZERO).unwrap();
    // The download runs on its own thread; Stop arrives from a request.
    let running = std::thread::spawn(move || {
        worker.run(command);
        worker
    });
    while !matches!(
        hub.install_progress().map(|p| p.stage),
        Some(InstallStage::Downloading)
    ) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopping = installer.stop();
    assert_eq!(stopping.install.unwrap().stage, InstallStage::Stopping);
    let mut worker = running.join().unwrap();
    assert_eq!(
        installer.state().install.unwrap().stage,
        InstallStage::Stopped
    );
    assert!(installer.state().installed.is_none());

    installer
        .install(&InstallReader {
            terms,
            from: InstallFrom::Offer,
        })
        .unwrap();
    drain(&hub, &mut worker);
    assert_eq!(
        installer.state().install.unwrap().stage,
        InstallStage::Installed
    );
    assert!(installer.state().installed.is_some());
}

/// The first fetch waits for Stop and answers it; the next copies.
struct Gate {
    stopped_once: Mutex<bool>,
    inner: DirectorySource,
}

impl ModelSource for Gate {
    fn fetch(
        &self,
        url: &url::Url,
        descriptor: &ModelDescriptor,
        destination: &Path,
        progress: &mut dyn FnMut(koru_models::InstallProgress),
        cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        let mut stopped = self.stopped_once.lock().unwrap();
        if !*stopped {
            progress(koru_models::InstallProgress {
                stage: koru_models::InstallStage::Downloading,
                received: 1,
                total: descriptor.size,
            });
            while !cancel.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            *stopped = true;
            return Err(ModelErrorCode::Cancelled.with("stopped"));
        }
        self.inner
            .fetch(url, descriptor, destination, progress, cancel)
    }
}

/// **A decline silences the offer; Settings installs anyway; the next import
/// with pictures offers once more** (EXPERIENCE §3 "Not now").
#[test]
fn a_decline_silences_and_settings_ignores_it() {
    let staged = stage();
    let (hub, mut worker, installer) = desktop(&staged);
    let terms = offer_terms(&installer, &hub, &mut worker);
    let ws = WorkspaceId::new();
    hub.publish_counts(
        [(
            ws,
            super::super::hub::ScopeCounts {
                total: 5,
                unread: 5,
                needs_reader: 5,
                not_here: 0,
            },
        )]
        .into(),
        Default::default(),
    );
    assert!(hub.reading_counts(Some(&[ws]), &[]).unwrap().offer);

    assert!(installer.decline().unwrap().offer_declined);
    drain(&hub, &mut worker);
    assert!(!hub.reading_counts(Some(&[ws]), &[]).unwrap().offer);
    assert!(read_declined(&staged.dir.path().join("reader")), "on disk");
    // It survives a restart of the installer.
    let (again, _w, _i) = desktop(&staged);
    assert!(again.declined());

    // A single file pasted is not an import: nothing changes.
    installer
        .import_finished(&ImportFinished {
            pictures_and_scans: 0,
        })
        .unwrap();
    assert!(hub.declined());

    // The Settings row can always turn it on.
    installer
        .install(&InstallReader {
            terms,
            from: InstallFrom::Settings,
        })
        .unwrap();
    drain(&hub, &mut worker);
    assert!(installer.state().installed.is_some());
    assert!(!hub.declined());
    assert!(!read_declined(&staged.dir.path().join("reader")));
}

/// An import that brought pictures clears a *Not now*.
#[test]
fn an_import_with_pictures_offers_once_more() {
    let staged = stage();
    let (hub, mut worker, installer) = desktop(&staged);
    installer.decline().unwrap();
    drain(&hub, &mut worker);
    let state = installer
        .import_finished(&ImportFinished {
            pictures_and_scans: 412,
        })
        .unwrap();
    assert!(!state.offer_declined);
    drain(&hub, &mut worker);
    assert!(!read_declined(&staged.dir.path().join("reader")));
}

/// **On the stable channel the install is refused with its code**, and no
/// catalog is read: a stable build pins no model-signing key.
#[test]
fn a_stable_build_refuses_the_install_with_its_code() {
    let (trust, no_root) = trust_from(false, TrustPolicy::digest_only_on(Some("stable")));
    assert!(no_root);
    assert_eq!(trust, TrustPolicy::SIGNED);
    // And with a key pinned, signed, whatever the channel.
    assert_eq!(
        trust_from(true, TrustPolicy::digest_only_on(Some("stable"))),
        (TrustPolicy::SIGNED, false)
    );

    let staged = stage();
    let hub = ReaderHub::new();
    let mut config = staged.config(staged.directory());
    config.trust = trust;
    config.no_trust_root = no_root;
    hub.set_installing_here(config.no_trust_root);
    hub.attach_coordinator();
    let mut worker = Worker::new(config, Arc::clone(&hub), Inbox::new());
    worker.open();
    let installer = Installer::idle(Arc::clone(&hub));
    let state = installer.state();
    assert_eq!(state.unavailable, Some(ReaderUnavailable::NoTrustRoot));
    assert!(
        hub.next_command(Duration::ZERO).is_none(),
        "no catalog read"
    );
    let refused = installer
        .install(&InstallReader {
            terms: terms_of(&staged.entry),
            from: InstallFrom::Settings,
        })
        .unwrap_err();
    assert_eq!(refused.code(), Code::ReaderUnavailable);
    // Were it asked anyway, the model store itself refuses: no trust root.
    let grant = koru_voice_consent::READ.recorded(staged.entry.terms(), 0);
    let error = worker
        .store()
        .install(
            &staged.entry,
            TrustedKeys::pinned(),
            &grant,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.code, ModelErrorCode::NoTrustRoot);
}

/// **A voice consent does not let the reader install** (TECHNICAL D10): the
/// reader's store refuses a grant from voice's record, however well its
/// licence and size match.
#[test]
fn a_voice_consent_does_not_open_the_readers_store() {
    let staged = stage();
    let (_hub, worker, _installer) = desktop(&staged);
    let voice = koru_voice_consent::VOICE.recorded(staged.entry.terms(), 0);
    let error = worker
        .store()
        .install(
            &staged.entry,
            TrustedKeys::pinned(),
            &voice,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.code, ModelErrorCode::ConsentRequired);
    // And a voice record on disk beside the reader's is not the reader's.
    let dir = staged.dir.path().join("reader");
    std::fs::create_dir_all(&dir).unwrap();
    koru_voice_consent::VOICE
        .grant(
            &koru_voice_consent::VOICE.path_in(&dir),
            staged.entry.terms(),
            0,
        )
        .unwrap();
    assert!(
        !koru_voice_consent::READ
            .read(&koru_voice_consent::READ.path_in(&dir))
            .is_granted()
    );
}

/// The terms a surface sends must be the ones the Store would install.
#[test]
fn changed_terms_are_refused() {
    let staged = stage();
    let (hub, mut worker, installer) = desktop(&staged);
    let mut terms = offer_terms(&installer, &hub, &mut worker);
    terms.download_bytes += 1;
    let refused = installer
        .install(&InstallReader {
            terms,
            from: InstallFrom::Offer,
        })
        .unwrap_err();
    assert_eq!(refused.code(), Code::ReaderTermsChanged);
    assert!(
        hub.next_command(Duration::ZERO).is_none(),
        "nothing started"
    );
}

/// **Remove** deletes the model from disk and keeps the consent.
#[test]
fn remove_takes_the_model_off_the_disk() {
    let staged = stage();
    let (hub, mut worker, installer) = desktop(&staged);
    let terms = offer_terms(&installer, &hub, &mut worker);
    installer
        .install(&InstallReader {
            terms,
            from: InstallFrom::Settings,
        })
        .unwrap();
    drain(&hub, &mut worker);
    let root = hub.installed().unwrap().root;
    // The coordinator lets the model go when the revision moves.
    let (revision, _) = hub.wanted_model();
    hub.ack_model(revision + 1);
    installer.remove().unwrap();
    drain(&hub, &mut worker);
    let state = installer.state();
    assert!(state.installed.is_none());
    assert_eq!(state.install.unwrap().stage, InstallStage::Removed);
    assert!(!root.exists());
    assert!(
        koru_voice_consent::READ
            .read(&koru_voice_consent::READ.path_in(&staged.dir.path().join("reader")))
            .is_granted(),
        "removing the bytes does not revoke the consent"
    );
}

/// **The reader installed and removed reach the event bus**, once each, as
/// registered events carrying counts only (OCR TECHNICAL §10); reading the
/// catalog is not news.
#[test]
fn an_install_and_a_removal_are_announced() {
    let staged = stage();
    let (hub, mut worker, installer) = desktop(&staged);
    let heard: Arc<Mutex<Vec<koru_events::Envelope>>> = Arc::default();
    let ear = Arc::clone(&heard);
    worker.announce = Some(Box::new(move |draft| {
        let ctx = koru_events::EmitContext {
            session_id: koru_events::SessionId::mint(),
            device_id: None,
            profile_id: None,
            actor: koru_events::Actor::system("store"),
        };
        ear.lock().unwrap().push(draft.into_envelope(&ctx));
    }));
    let terms = offer_terms(&installer, &hub, &mut worker);
    assert!(
        heard.lock().unwrap().is_empty(),
        "reading the catalog is not news"
    );
    installer
        .install(&InstallReader {
            terms,
            from: InstallFrom::Offer,
        })
        .unwrap();
    drain(&hub, &mut worker);
    let (revision, _) = hub.wanted_model();
    hub.ack_model(revision + 1);
    installer.remove().unwrap();
    drain(&hub, &mut worker);

    let heard = heard.lock().unwrap();
    let names: Vec<&str> = heard.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["reading.reader.installed", "reading.reader.removed"]
    );
    for envelope in heard.iter() {
        koru_events::validate(envelope).unwrap_or_else(|v| panic!("{}: {v}", envelope.name));
    }
    assert_eq!(
        heard[0].attrs.get("size_mb_bucket"),
        Some(&koru_events::AttrValue::Bucket(koru_events::bucket(
            koru_voice_consent::megabytes(staged.entry.descriptor.size)
        )))
    );
    assert!(heard[1].attrs.is_empty());
}

/// The cache root, by the one rule.
#[test]
fn the_model_cache_is_resolved_by_one_rule() {
    let store = PathBuf::from("store");
    let local = PathBuf::from("local");
    assert_eq!(
        models_root(
            Some(PathBuf::from("override")),
            &store,
            Some(store.clone()),
            Some(local.clone())
        ),
        PathBuf::from("override")
    );
    assert_eq!(
        models_root(None, &store, Some(store.clone()), Some(local.clone())),
        local.join("Koru").join("read-models"),
        "the channel's own store: the channel-independent cache"
    );
    assert_eq!(
        models_root(
            None,
            &store,
            Some(PathBuf::from("elsewhere")),
            Some(local.clone())
        ),
        store.join("read-models"),
        "a rooted store keeps its model under its root"
    );
    assert_eq!(
        models_root(None, &store, Some(store.clone()), None),
        store.join("read-models")
    );
}

/// The *Not now* flag reads total: a damaged file means "not declined".
#[test]
fn a_damaged_not_now_reads_as_not_declined() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!read_declined(dir.path()));
    write_declined(dir.path(), true).unwrap();
    assert!(read_declined(dir.path()));
    std::fs::write(dir.path().join(OFFER_FILE), b"\0\0").unwrap();
    assert!(!read_declined(dir.path()));
}

/// The catalog entry is chosen by engine and tier, never by name.
#[test]
fn the_entry_is_chosen_by_engine_and_tier() {
    let staged = stage();
    let mut other = staged.entry.clone();
    other.descriptor.engine = Token::parse("sherpa").unwrap();
    other.descriptor.id = ModelId::parse("a-speech-model").unwrap();
    let catalog = Catalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        entries: vec![other, staged.entry.clone()],
    };
    assert_eq!(choose(&catalog), Some(staged.entry.clone()));
    let none = Catalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        entries: vec![],
    };
    assert_eq!(choose(&none), None);
}
