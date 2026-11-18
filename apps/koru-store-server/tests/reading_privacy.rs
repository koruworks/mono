// SPDX-License-Identifier: MPL-2.0

//! **Nothing a file contains reaches a log** (OCR TECHNICAL §13), against a
//! real Store and the real read worker it supervises.
//!
//! The gate the design names, in the shape `search.rs` proves its query
//! reaches no log: a word nobody wrote, invented per run, is put **inside**
//! the files, in their **names**, in the **titles** of the pages that hold
//! them and in a **picture's name**; the Store reads them through
//! `koru-read-worker`, the reader's routes are walked the way the surfaces walk
//! them, and then every file under `<root>/logs` — the Store's own log and the
//! worker's, which sits beside it — is searched for the word. It runs three
//! times: with `KORU_READ_DEBUG` off; with it on, under the filters a Store
//! ships with; and with it on and the reading targets of both processes at
//! `debug` through `RUST_LOG`, which is the most either will ever say.
//!
//! Each run also asserts that the logs say something — the routes were logged,
//! the worker started, the per-job lines appeared exactly when asked for — or
//! a silent log would pass it.
//!
//! **The worker must be beside the Store's executable**:
//! `target/<profile>/koru-read-worker.exe`. `cargo test --workspace` builds it,
//! because `apps/koru-read` has a test of its own binary; alone, run
//! `cargo build -p koru-read-worker` first.
//!
//! What this cannot do in an ordinary run is recognize a picture: that needs
//! the worker built with `recognition` and an installed model, and a test may
//! load none. [`a_recognized_picture_leaves_no_word_in_any_log`] is the same
//! gate for that path, `#[ignore]`d, run by hand (its comment says how).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Node, NodeId};
use koru_store_client::Client;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::BlobHash;
use koru_store_wire::reader::ImportFinished;
use koru_store_wire::reader::{InstallFrom, InstallReader, ReaderTerms};
use koru_store_wire::search::{SearchQuery, SearchRequest};

/// The reading targets of both processes at `debug`, and the Store's own
/// default for everything else: the loudest either says about a file.
const LOUD: &str = "warn,koru_store=info,koru_store_api=info,koru_store_core=info,\
                    koru_readings=debug,koru_read_worker=debug,koru_read=debug,koru_read_paddle=debug";

/// How much the two processes are asked to say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Loudness {
    /// What a person's computer runs: no `KORU_READ_DEBUG`, no `RUST_LOG`.
    Quiet,
    /// `KORU_READ_DEBUG=1` under the shipped filters.
    ReadDebug,
    /// `KORU_READ_DEBUG=1` and [`LOUD`].
    Loud,
}

struct Store {
    child: Option<Child>,
    root: PathBuf,
}

impl Store {
    /// A Store that reads (`--read`) and logs to `<root>/logs/` (no
    /// `--console`), with nothing reachable on the network: the catalog is a
    /// file that does not exist, and the model cache is under the root.
    fn start(root: &Path, loudness: Loudness, models: Option<&Path>) -> Store {
        let worker = Path::new(env!("CARGO_BIN_EXE_koru-store-server"))
            .with_file_name(format!("koru-read-worker{}", std::env::consts::EXE_SUFFIX));
        assert!(
            worker.is_file(),
            "{} is missing: this gate reads through the real worker; `cargo build -p koru-read-worker`",
            worker.display()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-store-server"));
        command
            .args(["serve", "--read", "--root"])
            .arg(root)
            .env("KORU_STORE_NO_DIALOG", "1")
            .env("KORU_READ_CATALOG", root.join("no-catalog-here.json"));
        match models {
            Some(models) => command.env("KORU_READ_MODELS", models),
            None => command.env_remove("KORU_READ_MODELS"),
        };
        command.env_remove("KORU_READ_DEBUG").env_remove("RUST_LOG");
        if loudness != Loudness::Quiet {
            command.env("KORU_READ_DEBUG", "1");
        }
        if loudness == Loudness::Loud {
            command.env("RUST_LOG", LOUD);
        }
        common::configure(&mut command, root);
        let child = command.spawn().expect("spawn koru-store-server");
        let _ = common::wait_for_discovery(root);
        Store {
            child: Some(child),
            root: root.to_path_buf(),
        }
    }

    fn client(&self) -> Client {
        Client::new(common::discovery(&self.root).expect("discover"))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn root_for(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "koru-reading-privacy-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp root");
    dir
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A word nobody wrote: lowercase letters only, so search indexes it as one
/// word, and different on every run.
fn made_up_word() -> String {
    let mut n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        ^ (u128::from(std::process::id()) << 64);
    let mut word = String::from("zq");
    for _ in 0..12 {
        word.push(char::from(b'a' + (n % 26) as u8));
        n /= 26;
    }
    word
}

// ------------------------------------------------------------ the files

/// A one-page PDF with a real text layer: 12-point Helvetica, one line each.
fn text_pdf(lines: &[&str]) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf 72 720 Td\n");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            content.push_str("0 -16 Td\n");
        }
        content.push_str(&format!("({line}) Tj\n"));
    }
    content.push_str("ET\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A damaged PDF — a header and nothing a parser can recover — with the word
/// in its bytes, in the clear, where a careless error message could quote it.
fn damaged_pdf(word: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    for _ in 0..20 {
        out.extend_from_slice(format!("({word}) this is not an object or a trailer\n").as_bytes());
    }
    out.extend_from_slice(b"%%EOF\n");
    out
}

/// A one-pixel PNG. Its pixels say nothing; its page's title and its name do.
const PIXEL_PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn add_file(
    client: &Client,
    ws: WorkspaceId,
    bytes: Vec<u8>,
    mime: &str,
    title: &str,
) -> (DocumentId, BlobHash) {
    let hash = client
        .put_blob(ws, bytes, mime, Some(title))
        .expect("blob")
        .hash;
    let id = DocumentId::new();
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), title).unwrap();
    let mut node = Node::file(title, koru_core::BlobHash(hash.as_str().to_owned()), mime).unwrap();
    node.id = NodeId::from_uuid(id.as_uuid());
    doc.insert(Parent::Root, node).unwrap();
    client
        .import_document_at_with_mime(
            ws,
            id,
            title,
            "file",
            Some(mime),
            None,
            doc.export_snapshot().unwrap(),
        )
        .expect("file entry");
    (id, hash)
}

fn paste_picture(
    client: &Client,
    ws: WorkspaceId,
    title: &str,
    name: &str,
    bytes: Vec<u8>,
) -> (DocumentId, BlobHash) {
    let hash = client
        .put_blob(ws, bytes, "image/png", None)
        .expect("blob")
        .hash;
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), title).unwrap();
    let root = doc
        .insert(Parent::Root, Node::document(title).unwrap())
        .unwrap();
    let mut picture = Node::image(
        koru_core::BlobHash(hash.as_str().to_owned()),
        "image/png",
        None,
        None,
    )
    .unwrap();
    picture.meta.name = Some(name.to_owned());
    doc.insert(Parent::Node(root), picture).unwrap();
    let page = client
        .import_document(ws, title, "page", doc.export_snapshot().unwrap())
        .expect("page");
    (page, hash)
}

fn status(client: &Client, ws: WorkspaceId, hash: &BlobHash) -> Option<String> {
    client
        .reading(ws, hash)
        .ok()
        .flatten()
        .and_then(|reading| reading.status)
}

fn found(client: &Client, ws: WorkspaceId, word: &str, document: DocumentId) -> bool {
    let key = document.to_storage();
    client
        .search(&SearchRequest {
            query: SearchQuery {
                text: word.to_owned(),
                workspaces: vec![ws],
                ..SearchQuery::default()
            },
            include_facets: false,
            recent_queries: None,
        })
        .map(|answer| {
            answer
                .page
                .hits
                .iter()
                .any(|hit| hit.document.as_deref() == Some(key.as_str()))
        })
        .unwrap_or(false)
}

// ------------------------------------------------------------- the logs

/// Every log file under `<root>/logs`, by name, with its text — once the
/// non-blocking appenders have had a moment to write what they hold.
fn logs(root: &Path) -> Vec<(String, String)> {
    std::thread::sleep(Duration::from_millis(500));
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root.join("logs")).expect("a logs directory") {
        let path = entry.expect("entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        out.push((name, std::fs::read_to_string(&path).unwrap_or_default()));
    }
    out
}

/// One process's log: every day's file of it (a run that crosses midnight
/// has two), joined.
fn the_one(logs: &[(String, String)], prefix: &str) -> String {
    let texts: Vec<&str> = logs
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(_, text)| text.as_str())
        .collect();
    assert!(
        !texts.is_empty(),
        "no {prefix} log among {:?}",
        logs.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    texts.concat()
}

fn assert_no_word(logs: &[(String, String)], word: &str) {
    for (name, text) in logs {
        if let Some(line) = text.lines().find(|line| line.contains(word)) {
            panic!("a word from a file reached {name}:\n{line}");
        }
    }
}

// ------------------------------------------------------------- the gate

/// Read a text PDF, fail on a damaged one, register a picture in a page,
/// search, and walk every reader route a surface calls; then look.
fn a_read_leaves_no_word_in_any_log(loudness: Loudness) {
    let word = made_up_word();
    let root = root_for(&format!("{loudness:?}").to_lowercase());
    let store = Store::start(&root, loudness, None);
    let client = store.client();
    let profile = client
        .create_profile("Gate", None, None, Some("en"))
        .expect("profile");
    let ws = client
        .create_workspace(profile.id, "Gate")
        .expect("workspace")
        .id;

    let text_line = format!("The reference word is {word} on this page.");
    let (text_doc, text) = add_file(
        &client,
        ws,
        text_pdf(&[&text_line, "and a second line"]),
        "application/pdf",
        &format!("{word} report.pdf"),
    );
    let (broken_doc, broken) = add_file(
        &client,
        ws,
        damaged_pdf(&word),
        "application/pdf",
        &format!("{word} broken.pdf"),
    );
    let _ = paste_picture(
        &client,
        ws,
        &format!("{word} page"),
        &format!("{word} picture"),
        PIXEL_PNG.to_vec(),
    );

    wait_for("the text PDF to be read", Duration::from_secs(60), || {
        status(&client, ws, &text).as_deref() == Some("read")
    });
    wait_for("the damaged PDF to fail", Duration::from_secs(60), || {
        status(&client, ws, &broken).as_deref() == Some("failed")
    });
    // It was really read: found by the word inside it — and this search is
    // the query path too.
    wait_for("the word to be found", Duration::from_secs(30), || {
        found(&client, ws, &word, text_doc)
    });

    // The reader's routes, as the line in search, the Settings card and the
    // right-click menu call them.
    let state = client.reader().expect("GET /v1/readings/reader");
    let items = client
        .reader_items()
        .expect("GET /v1/readings/reader/items");
    assert!(
        items.items.iter().any(|item| item.document == broken_doc),
        "the damaged PDF is listed: {:?}",
        items.items
    );
    let _ = client
        .reading(ws, &text)
        .expect("GET /v1/readings/{ws}/{hash}");
    let _ = client.read_all(ws, &text);
    client.read_again(ws, &text).expect("read-again accepted");
    let _ = client.install_reader(&InstallReader {
        terms: state.offer.clone().unwrap_or(ReaderTerms {
            model: format!("{word}-model"),
            version: "1".into(),
            licence: "Apache-2.0".into(),
            attribution: word.clone(),
            download_bytes: 1,
            languages: Vec::new(),
        }),
        from: InstallFrom::Offer,
    });
    client.stop_reader_install().expect("stop");
    // Off and on answer whether or not a reader is installed (OCR Plan 3.5).
    let _ = client.turn_reader_off();
    let _ = client.turn_reader_on();
    client.decline_reader().expect("decline");
    client
        .reader_import_finished(&ImportFinished {
            pictures_and_scans: 1,
        })
        .expect("imported");
    client.remove_reader().expect("remove");
    let _: serde_json::Value = client.get_json("/v1/debug/readings").expect("debug route");
    wait_for("Read again to be met", Duration::from_secs(60), || {
        client
            .get_json::<serde_json::Value>("/v1/debug/readings")
            .is_ok_and(|board| board["coordinator"]["done"]["requests_met"].as_u64() >= Some(1))
    });

    let logs = logs(&root);
    assert_no_word(&logs, &word);

    // ...and the logs are not simply silent.
    let store_log = the_one(&logs, "koru-store-server.log");
    let worker_log = the_one(&logs, "koru-read-worker.log");
    assert!(
        store_log.contains("/v1/readings/reader/decline"),
        "the routes were logged"
    );
    assert!(
        worker_log.contains("the read worker started"),
        "the worker logged"
    );
    // `ReadDebug` is the shipped filters: the job lines reaching the file
    // there is what proves the `koru_readings` target is not filtered out.
    let job_lines = store_log.matches("reading job").count();
    match loudness {
        Loudness::Quiet => assert_eq!(job_lines, 0, "no line per job without KORU_READ_DEBUG"),
        Loudness::ReadDebug | Loudness::Loud => assert!(
            job_lines >= 3,
            "KORU_READ_DEBUG writes one line per job, three here; saw {job_lines}"
        ),
    }
    if loudness == Loudness::Loud {
        assert!(
            worker_log.contains("a PDF was read"),
            "the worker's own job line, at debug"
        );
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_read_leaves_no_word_in_any_log_quietly() {
    a_read_leaves_no_word_in_any_log(Loudness::Quiet);
}

#[test]
fn a_read_leaves_no_word_in_any_log_with_read_debug() {
    a_read_leaves_no_word_in_any_log(Loudness::ReadDebug);
}

#[test]
fn a_read_leaves_no_word_in_any_log_at_debug() {
    a_read_leaves_no_word_in_any_log(Loudness::Loud);
}

/// **The same gate for a recognized picture.** Ignored: it needs the worker
/// built with recognition beside the Store, and an installed model, and a test
/// may load neither. By hand:
///
/// ```text
/// cargo build -p koru-read-worker --features recognition   # ORT_LIB_PATH set
/// set READING_GATE_MODELS=<a model cache holding the installed reader>
/// set READING_GATE_PICTURE=<a PNG that shows one made-up word>
/// set READING_GATE_WORD=<that word>
/// cargo test -p koru-store-server --test reading_privacy -- --ignored
/// ```
///
/// `scripts/dev/read-model/make_cases.py` writes such a picture and its word
/// (`uc6-named.png`, `words.json`); the cache is any `read-models` directory a
/// rooted Store installed into; the Store may leave a lease file in it.
#[test]
#[ignore]
fn a_recognized_picture_leaves_no_word_in_any_log() {
    let var = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; see this test's comment"))
    };
    let models = PathBuf::from(var("READING_GATE_MODELS"));
    let picture = std::fs::read(var("READING_GATE_PICTURE")).expect("the picture");
    let seen = var("READING_GATE_WORD");
    for loudness in [Loudness::Quiet, Loudness::ReadDebug, Loudness::Loud] {
        let word = made_up_word();
        let root = root_for(&format!("picture-{loudness:?}").to_lowercase());
        let store = Store::start(&root, loudness, Some(&models));
        let client = store.client();
        wait_for("the installed reader", Duration::from_secs(30), || {
            client.reader().is_ok_and(|state| state.installed.is_some())
        });
        let profile = client
            .create_profile("Gate", None, None, Some("en"))
            .expect("profile");
        let ws = client
            .create_workspace(profile.id, "Gate")
            .expect("workspace")
            .id;
        let (page, hash) = paste_picture(
            &client,
            ws,
            &format!("{word} page"),
            &format!("{word} picture"),
            picture.clone(),
        );
        wait_for(
            "the picture to be recognized",
            Duration::from_secs(120),
            || status(&client, ws, &hash).as_deref() == Some("read"),
        );
        wait_for(
            "the picture's word to be found",
            Duration::from_secs(30),
            || found(&client, ws, &seen, page),
        );
        let _ = client.read_again(ws, &hash).expect("read-again");
        wait_for("Read again to be met", Duration::from_secs(60), || {
            client
                .get_json::<serde_json::Value>("/v1/debug/readings")
                .is_ok_and(|board| board["coordinator"]["done"]["requests_met"].as_u64() >= Some(1))
        });
        let logs = logs(&root);
        assert_no_word(&logs, &seen);
        assert_no_word(&logs, &word);
        let worker_log = the_one(&logs, "koru-read-worker.log");
        assert!(
            worker_log.contains("recognition=true"),
            "the worker could recognize"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
    }
}
