// SPDX-License-Identifier: MPL-2.0

//! **The second milestone's use cases on real processes** (OCR ROADMAP §2,
//! UC6 to UC10), as far as they go without a window.
//!
//! ```text
//! cargo build -p koru-store-server --example reader_acceptance
//! cargo build -p koru-read-worker --features recognition      # ORT_LIB_PATH set
//! <fixtures venv>/python -I scripts/dev/read-model/make_cases.py <cases>   # words made up per run
//! koru-release read-model package --work <copy of the converted model> \
//!     --base-url http://127.0.0.1:<port>                      # then serve <work>/serve on <port>
//! target/debug/examples/reader_acceptance <run dir> <cases> <catalog url>
//! ```
//!
//! It starts standalone Stores itself (`koru-store-server serve --root <dir>
//! --read`, beside this example's directory) on throwaway roots under
//! `<run dir>`, with `KORU_READ_CATALOG` pointing at the catalog given, and
//! drives each over its API the way a surface would: the counts and the offer
//! flag a search answer carries, `install` with `from: offer` or `from:
//! settings`, the progress a surface polls, *Not now*, the hits a result row is
//! drawn from, and the *could not be read* list. It stops every Store it
//! started.
//!
//! What it prints is ids, counts, stages, page numbers and timings; a made-up
//! word appears only as "found" or "not found". After the run it looks for
//! every made-up word in every log the Stores and their read workers wrote.
//!
//! **What it cannot show is the window**: the line in search, the Settings
//! card, the picture mark, landing on the picture. ACCEPTANCE.md has those.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Node, NodeId};
use koru_store_client::Client;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::BlobHash;
use koru_store_wire::reader::{InstallFrom, InstallReader, InstallStage, ReaderState};
use koru_store_wire::readings::ReadingAnswer;
use koru_store_wire::search::{ReadingCounts, SearchHit, SearchQuery, SearchRequest};

// ------------------------------------------------------------------ a Store

/// A standalone Store this run started, and stops.
struct Store {
    child: Child,
    client: Client,
}

impl Store {
    fn start(root: &Path, catalog: &str) -> Store {
        std::fs::create_dir_all(root).expect("the Store's root");
        let exe = std::env::current_exe()
            .expect("this example's path")
            .parent()
            .and_then(Path::parent)
            .expect("target/<profile>/examples")
            .join(format!("koru-store-server{}", std::env::consts::EXE_SUFFIX));
        let ready = root.join("standalone-ready.json");
        let _ = std::fs::remove_file(&ready);
        let child = Command::new(&exe)
            .args(["serve", "--read", "--root"])
            .arg(root)
            .env(koru_store_server::INTERNAL_READY_ENV, &ready)
            .env("KORU_STORE_NO_DIALOG", "1")
            .env("KORU_READ_CATALOG", catalog)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("starting {}: {e}", exe.display()));
        let deadline = Instant::now() + Duration::from_secs(60);
        let client = loop {
            if let Ok(bytes) = std::fs::read(&ready)
                && let Ok(ready) =
                    serde_json::from_slice::<koru_store_server::StandaloneReady>(&bytes)
                && let Ok(discovery) = ready.discovery()
            {
                let client = Client::new(discovery);
                if client.status().is_ok() {
                    break client;
                }
            }
            assert!(Instant::now() < deadline, "the Store never became ready");
            std::thread::sleep(Duration::from_millis(50));
        };
        Store { child, client }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --------------------------------------------------------------- the clock

struct Clock(Instant);

impl Clock {
    fn say(&self, what: impl AsRef<str>) {
        println!(
            "[{:>7.2}s] {}",
            self.0.elapsed().as_secs_f64(),
            what.as_ref()
        );
    }
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) -> Duration {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
    started.elapsed()
}

// ------------------------------------------------------- what a person adds

/// A file, the way the Files view adds one: a blob and a file entry.
fn add_file(
    client: &Client,
    ws: WorkspaceId,
    bytes: Vec<u8>,
    mime: &str,
    title: &str,
) -> (DocumentId, BlobHash) {
    let hash = client.put_blob(ws, bytes, mime, None).expect("blob").hash;
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

/// A page with one picture pasted into it, below a line of typed text — a
/// picture node in a document, not a file. `name` is what a person typed as
/// the picture's name, if anything.
fn paste_picture(
    client: &Client,
    ws: WorkspaceId,
    title: &str,
    bytes: Vec<u8>,
    name: Option<&str>,
) -> (DocumentId, NodeId, BlobHash) {
    let hash = client
        .put_blob(ws, bytes, "image/png", None)
        .expect("blob")
        .hash;
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), title).unwrap();
    let root = doc
        .insert(Parent::Root, Node::document(title).unwrap())
        .unwrap();
    doc.insert(
        Parent::Node(root),
        Node::paragraph("Pasted below.").unwrap(),
    )
    .unwrap();
    let mut picture = Node::image(
        koru_core::BlobHash(hash.as_str().to_owned()),
        "image/png",
        Some(1280),
        Some(720),
    )
    .unwrap();
    picture.meta.name = name.map(str::to_owned);
    let picture = doc.insert(Parent::Node(root), picture).unwrap();
    let page = client
        .import_document(ws, title, "page", doc.export_snapshot().unwrap())
        .expect("page");
    (page, picture, hash)
}

// ---------------------------------------------------------- what is asked

fn search(client: &Client, ws: WorkspaceId, text: &str) -> (Vec<SearchHit>, Option<ReadingCounts>) {
    let answer = client
        .search(&SearchRequest {
            query: SearchQuery {
                text: text.to_owned(),
                workspaces: vec![ws],
                ..SearchQuery::default()
            },
            include_facets: false,
            recent_queries: None,
        })
        .expect("search");
    (answer.page.hits, answer.page.reading)
}

fn hits_on(client: &Client, ws: WorkspaceId, text: &str, document: DocumentId) -> Vec<SearchHit> {
    let key = document.to_storage();
    search(client, ws, text)
        .0
        .into_iter()
        .filter(|hit| hit.document.as_deref() == Some(key.as_str()))
        .collect()
}

fn counts(reading: Option<ReadingCounts>) -> String {
    reading.map_or("none".into(), |c| {
        format!(
            "offer={} needs_reader={} unread={} total={} not_here={}",
            c.offer, c.needs_reader, c.unread, c.total, c.not_here
        )
    })
}

fn reading(client: &Client, ws: WorkspaceId, hash: &BlobHash) -> Option<ReadingAnswer> {
    client.reading(ws, hash).expect("GET reading")
}

fn status(answer: &Option<ReadingAnswer>) -> String {
    match answer {
        None => "nothing read".into(),
        Some(r) => format!(
            "{}{} pages_done={:?} pages_with_words={}",
            r.status.as_deref().unwrap_or("?"),
            r.reason
                .as_ref()
                .map(|r| format!("/{r}"))
                .unwrap_or_default(),
            r.pages_done,
            r.pages.len()
        ),
    }
}

fn machine(state: &ReaderState) -> String {
    let c = &state.counts;
    format!(
        "installed={} declined={} total={} read={} partly_read={} could_not_be_read={} {:?} unread={} needs_reader={}",
        state.installed.is_some(),
        state.offer_declined,
        c.total,
        c.read,
        c.partly_read,
        c.could_not_be_read,
        c.could_not_be_read_by_reason,
        c.unread,
        c.needs_reader
    )
}

/// The offer's terms, once the Store has read its catalog.
fn offer(client: &Client) -> koru_store_wire::reader::ReaderTerms {
    let mut terms = None;
    wait_for("the offer's terms", Duration::from_secs(60), || {
        terms = client.reader().expect("reader").offer;
        terms.is_some()
    });
    terms.unwrap()
}

/// Turn the reader on, and watch it the way a surface polls: every stage, and
/// the bytes while it downloads.
fn install(clock: &Clock, client: &Client, from: InstallFrom) -> Duration {
    let terms = offer(client);
    clock.say(format!(
        "offer: {} {}, {} bytes ({} MB as a sentence says it), languages {:?}",
        terms.model,
        terms.version,
        terms.download_bytes,
        koru_voice_consent::megabytes(terms.download_bytes),
        terms.languages
    ));
    let started = Instant::now();
    let answer = client
        .install_reader(&InstallReader { terms, from })
        .expect("install");
    clock.say(format!(
        "install from {from:?} answered stage {:?}",
        answer.install.as_ref().map(|p| p.stage)
    ));
    let mut last: Option<(InstallStage, u64)> = None;
    loop {
        let state = client.reader().expect("reader");
        if let Some(progress) = &state.install {
            let now = (progress.stage, progress.received);
            if last != Some(now) {
                clock.say(format!(
                    "  progress: {:?} {} of {} bytes",
                    progress.stage, progress.received, progress.total
                ));
                last = Some(now);
            }
            match progress.stage {
                InstallStage::Installed if state.installed.is_some() => break,
                InstallStage::Failed | InstallStage::Stopped => {
                    panic!("install ended {progress:?}")
                }
                _ => {}
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the install never finished"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    started.elapsed()
}

fn verdict(name: &str, pass: bool, what: &str) -> bool {
    println!("{name} {}: {what}", if pass { "PASS" } else { "FAIL" });
    pass
}

// ------------------------------------------------------------- the cases

struct Cases {
    dir: PathBuf,
    words: serde_json::Value,
}

impl Cases {
    fn bytes(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }
    fn word(&self, key: &str) -> String {
        self.words[key].as_str().expect(key).to_owned()
    }
    fn page_word(&self, n: u32) -> String {
        self.words["uc7_pages"][n.to_string()]
            .as_str()
            .expect("page word")
            .to_owned()
    }
    fn scan_pages(&self) -> u32 {
        self.words["uc7_pages"]
            .as_object()
            .map_or(0, |pages| pages.len() as u32)
    }
    /// Every made-up word this run used.
    fn all_words(&self) -> Vec<String> {
        let mut out = vec![
            self.word("uc6_named"),
            self.word("uc6_unnamed"),
            self.word("uc10_password"),
        ];
        out.extend((1..=self.scan_pages()).map(|n| self.page_word(n)));
        out
    }
}

/// UC6, UC7 and UC10 on one root: no reader until UC6 turns it on.
fn screenshot_scan_and_failures(run: &Path, cases: &Cases, catalog: &str) -> Vec<bool> {
    let clock = Clock(Instant::now());
    let store = Store::start(&run.join("uc6-7-10"), catalog);
    let client = &store.client;
    let profile = client
        .create_profile("M2 run", None, None, Some("en"))
        .expect("profile");
    let ws = client
        .create_workspace(profile.id, "Pictures and scans")
        .expect("workspace")
        .id;
    let mut verdicts = Vec::new();

    // ---- UC6: a screenshot pasted into a page; offered; turned on.
    println!("\n== UC6: a screenshot inside a document");
    let named = cases.word("uc6_named");
    let unnamed = cases.word("uc6_unnamed");
    let (page_a, picture_a, _) = paste_picture(
        client,
        ws,
        "Account notes",
        cases.bytes("uc6-named.png"),
        Some("Settings screenshot"),
    );
    let (page_b, picture_b, _) = paste_picture(
        client,
        ws,
        "Backup notes",
        cases.bytes("uc6-unnamed.png"),
        None,
    );
    clock.say(format!(
        "two pages pasted with a picture each: {page_a} (picture named), {page_b} (picture unnamed)"
    ));
    let _ = client.reader();
    let counted = wait_for(
        "both pictures to wait for the reader",
        Duration::from_secs(60),
        || {
            search(client, ws, "nothingmatchesthis")
                .1
                .is_some_and(|c| c.needs_reader >= 2)
        },
    );
    let (before_hits, before_counts) = search(client, ws, &named);
    let before_found = before_hits
        .iter()
        .any(|h| h.document.as_deref() == Some(page_a.to_storage().as_str()));
    clock.say(format!(
        "{:.2}s later a search answer counts them; the screenshot's word: {} | {}",
        counted.as_secs_f64(),
        if before_found { "found" } else { "not found" },
        counts(before_counts)
    ));
    let (_, with_results) = search(client, ws, "notes");
    clock.say(format!(
        "a search with results (the pages' titles): {}",
        counts(with_results)
    ));
    let offered = before_counts.is_some_and(|c| c.offer && c.needs_reader >= 2)
        && with_results.is_some_and(|c| c.offer);

    let took = install(&clock, client, InstallFrom::Offer);
    clock.say(format!(
        "installed {:.2}s after Turn on",
        took.as_secs_f64()
    ));
    let installed_at = Instant::now();
    let found_after = wait_for(
        "the screenshot to be found",
        Duration::from_secs(120),
        || !hits_on(client, ws, &named, page_a).is_empty(),
    );
    clock.say(format!(
        "the page is found by the screenshot's word {:.2}s after the install finished",
        found_after.as_secs_f64()
    ));
    let hit = hits_on(client, ws, &named, page_a).remove(0);
    clock.say(format!(
        "  hit: field={} kind={} document_kind={} node={:?} (the picture is {picture_a}) picture_name={:?} page={:?}",
        hit.field, hit.kind, hit.document_kind, hit.node, hit.picture_name, hit.page
    ));
    wait_for("the second screenshot", Duration::from_secs(60), || {
        !hits_on(client, ws, &unnamed, page_b).is_empty()
    });
    let hit_b = hits_on(client, ws, &unnamed, page_b).remove(0);
    clock.say(format!(
        "  unnamed: field={} kind={} node={:?} (the picture is {picture_b}) picture_name={:?}",
        hit_b.field, hit_b.kind, hit_b.node, hit_b.picture_name
    ));
    let (_, after_counts) = search(client, ws, &named);
    clock.say(format!(
        "after: {} ({:.2}s after the install)",
        counts(after_counts),
        installed_at.elapsed().as_secs_f64()
    ));
    let state = client.reader().expect("reader");
    clock.say(format!("reader: {}", machine(&state)));
    verdicts.push(verdict(
        "UC6",
        offered
            && !before_found
            && hit.field == "read"
            && hit.kind == "image"
            && hit.node.as_deref() == Some(picture_a.to_string().as_str())
            && hit.picture_name.as_deref() == Some("Settings screenshot")
            && hit_b.node.as_deref() == Some(picture_b.to_string().as_str())
            && hit_b.picture_name.is_none()
            && after_counts.is_some_and(|c| !c.offer),
        "offered and counted before; installed from the offer with progress; found by a word in the \
         screenshot, field read, on the picture node, named when named; offer gone",
    ));

    // ---- UC7: a scanned PDF, findable page by page.
    let pages = cases.scan_pages();
    println!("\n== UC7: a scanned PDF of {pages} pages");
    let early = 3_u32;
    let late = pages;
    let (scan_doc, scan) = add_file(
        client,
        ws,
        cases.bytes("uc7-scan.pdf"),
        "application/pdf",
        "Scanned manual.pdf",
    );
    let added = Instant::now();
    clock.say(format!("scan added: file {scan_doc}"));
    let (mut early_at, mut late_at) = (None::<Duration>, None::<Duration>);
    let mut seen_early_not_late = false;
    let mut last = String::new();
    while late_at.is_none() {
        let early_found = !hits_on(client, ws, &cases.page_word(early), scan_doc).is_empty();
        let late_found = !hits_on(client, ws, &cases.page_word(late), scan_doc).is_empty();
        if early_found && early_at.is_none() {
            early_at = Some(added.elapsed());
        }
        if late_found {
            late_at = Some(added.elapsed());
        }
        if early_found && !late_found {
            seen_early_not_late = true;
        }
        let now = status(&reading(client, ws, &scan));
        if now != last {
            clock.say(format!(
                "  scan: {now}; page {early}'s word {}, page {late}'s word {}",
                if early_found { "found" } else { "not found" },
                if late_found { "found" } else { "not found" }
            ));
            last = now;
        }
        assert!(
            added.elapsed() < Duration::from_secs(1_800),
            "the scan never finished"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let hit = hits_on(client, ws, &cases.page_word(late), scan_doc).remove(0);
    clock.say(format!(
        "page {early} found {:.1}s after the drop; page {late} found {:.1}s after it; hit field={} page={:?}",
        early_at.unwrap_or_default().as_secs_f64(),
        late_at.unwrap_or_default().as_secs_f64(),
        hit.field,
        hit.page
    ));
    let answer = reading(client, ws, &scan);
    let exact = answer.as_ref().map_or(0, |r| {
        r.pages
            .iter()
            .filter(|p| p.text.contains(&cases.page_word(p.page)))
            .count()
    });
    clock.say(format!(
        "pages whose made-up word was read exactly: {exact} of {pages}"
    ));
    verdicts.push(verdict(
        "UC7",
        seen_early_not_late && hit.page == Some(late) && hit.field == "read",
        "an early page was found while a late one was not; then the late one was found, with its page",
    ));

    // ---- UC10: a password-protected PDF and a photograph with no text.
    println!("\n== UC10: a password-protected PDF and a photograph with no text");
    let before = client.reader().expect("reader");
    clock.say(format!("before: {}", machine(&before)));
    let (locked_doc, locked) = add_file(
        client,
        ws,
        cases.bytes("uc10-password.pdf"),
        "application/pdf",
        "Contract.pdf",
    );
    let (photo_doc, photo) = add_file(
        client,
        ws,
        cases.bytes("uc10-photo.jpg"),
        "image/jpeg",
        "Garden.jpg",
    );
    clock.say(format!(
        "added: protected PDF {locked_doc}, photograph {photo_doc}"
    ));
    let took = wait_for("both to settle", Duration::from_secs(120), || {
        let a = reading(client, ws, &locked);
        let b = reading(client, ws, &photo);
        // A photograph is settled whether the reader found nothing (`empty`)
        // or a stray mark in the noise (`read`): either way it is no failure.
        a.is_some_and(|r| r.status.as_deref() == Some("failed"))
            && b.is_some_and(|r| matches!(r.status.as_deref(), Some("empty" | "read")))
    });
    let photo_reading = reading(client, ws, &photo);
    let stray = photo_reading.as_ref().map_or(0, |r| {
        r.pages
            .iter()
            .map(|p| p.text.chars().filter(|c| !c.is_whitespace()).count())
            .sum::<usize>()
    });
    clock.say(format!(
        "settled in {:.2}s: protected PDF {}; photograph {} ({} characters read, confidence {:?})",
        took.as_secs_f64(),
        status(&reading(client, ws, &locked)),
        status(&photo_reading),
        stray,
        photo_reading.as_ref().and_then(|r| r.confidence)
    ));
    let items = client.reader_items().expect("items");
    let mut listed: BTreeMap<String, String> = BTreeMap::new();
    for item in &items.items {
        listed.insert(
            item.hash.clone(),
            format!("{:?}/{}", item.kind, item.reason),
        );
        clock.say(format!(
            "  listed: workspace {} document {} kind {:?} reason {}",
            item.workspace, item.document, item.kind, item.reason
        ));
    }
    let locked_listed = items
        .items
        .iter()
        .any(|i| i.hash == locked.as_str() && i.document == locked_doc && i.reason == "password");
    let photo_listed = items.items.iter().any(|i| i.hash == photo.as_str());
    let after = client.reader().expect("reader");
    clock.say(format!("after: {}", machine(&after)));
    let password_text = !hits_on(client, ws, &cases.word("uc10_password"), locked_doc).is_empty();
    let by_name = !hits_on(client, ws, "Contract", locked_doc).is_empty();
    clock.say(format!(
        "the protected PDF by a word inside it: {}; by its name: {}",
        if password_text { "found" } else { "not found" },
        if by_name { "found" } else { "not found" }
    ));
    verdicts.push(verdict(
        "UC10",
        locked_listed
            && !photo_listed
            && after.counts.read == before.counts.read + 1
            && after.counts.could_not_be_read == before.counts.could_not_be_read + 1
            && !password_text
            && by_name,
        "the protected PDF is listed with reason password and found by name only; the photograph is in \
         no list and counts as read",
    ));
    drop(store);
    verdicts
}

/// UC8 and UC9 on one root: voice already agreed to, the reader never asked.
fn voice_consent_and_not_now(
    run: &Path,
    cases: &Cases,
    catalog: &str,
    catalog_file: &Path,
) -> Vec<bool> {
    let clock = Clock(Instant::now());
    let channel = run.join("uc8-9");
    let root = channel.join("store");
    let mut verdicts = Vec::new();

    println!("\n== UC8: voice already set up; the reader never asked");
    // Voice's consent, the way voice writes it (`koru_voice_consent::VOICE`),
    // where a rooted Agent keeps it (`<channel root>/agent/`), and — the
    // strongest form — a copy beside where the reader's own record will go,
    // naming the reader's exact terms. Neither may open the reader's download.
    let catalog_bytes = std::fs::read(catalog_file).expect("the staged catalog");
    let entry = koru_models::Catalog::parse(&catalog_bytes)
        .expect("catalog")
        .entries
        .remove(0);
    let now = koru_voice_consent::now_unix();
    for dir in [channel.join("agent"), root.join("reader")] {
        std::fs::create_dir_all(&dir).unwrap();
        koru_voice_consent::VOICE
            .grant(&koru_voice_consent::VOICE.path_in(&dir), entry.terms(), now)
            .expect("voice's consent");
    }
    clock.say("voice's consent written (agent/ and beside the reader's record), with the reader's own terms");
    let store = Store::start(&root, catalog);
    let client = &store.client;
    let profile = client
        .create_profile("M2 voice", None, None, Some("en"))
        .expect("profile");
    let ws = client
        .create_workspace(profile.id, "Voice user")
        .expect("workspace")
        .id;
    let word = cases.word("uc6_unnamed");
    let (page, _, _) = paste_picture(
        client,
        ws,
        "Pasted screenshot",
        cases.bytes("uc6-unnamed.png"),
        None,
    );
    let _ = client.reader();
    wait_for(
        "the picture to wait for the reader",
        Duration::from_secs(60),
        || {
            search(client, ws, "nothingmatchesthis")
                .1
                .is_some_and(|c| c.needs_reader >= 1)
        },
    );
    let terms = offer(client);
    // Give a download that consent might have opened every chance to start.
    std::thread::sleep(Duration::from_secs(3));
    let (_, reading_counts) = search(client, ws, "nothingmatchesthis");
    let state = client.reader().expect("reader");
    let read_consent = root.join("reader").join(koru_voice_consent::READ.name());
    let cached = files_under(&root.join("read-models"));
    clock.say(format!(
        "search: {} | reader: {} install={:?} offer={} {} | read-consent.conf exists: {} | files in the model cache: {cached}",
        counts(reading_counts),
        machine(&state),
        state.install.as_ref().map(|p| p.stage),
        terms.model,
        terms.version,
        read_consent.exists()
    ));
    verdicts.push(verdict(
        "UC8",
        reading_counts.is_some_and(|c| c.offer)
            && state.installed.is_none()
            && state.install.is_none()
            && cached == 0
            && !read_consent.exists(),
        "the reader still offers; voice's consent opened no download and wrote no reader consent",
    ));

    println!("\n== UC9: Not now, search again, then Turn on from Settings");
    let declined = client.decline_reader().expect("decline");
    clock.say(format!(
        "Not now: offer_declined={}",
        declined.offer_declined
    ));
    let (_, empty) = search(client, ws, "nothingmatchesthis");
    let (hits, with_results) = search(client, ws, "Pasted");
    clock.say(format!(
        "search with no results: {} | search with {} result(s): {}",
        counts(empty),
        hits.len(),
        counts(with_results)
    ));
    let quiet = empty.is_some_and(|c| !c.offer) && with_results.is_some_and(|c| !c.offer);
    let took = install(&clock, client, InstallFrom::Settings);
    clock.say(format!(
        "installed from Settings in {:.2}s",
        took.as_secs_f64()
    ));
    let found = wait_for("the picture to be found", Duration::from_secs(120), || {
        !hits_on(client, ws, &word, page).is_empty()
    });
    let state = client.reader().expect("reader");
    clock.say(format!(
        "found by its word {:.2}s later | reader: {} | read-consent.conf exists: {}",
        found.as_secs_f64(),
        machine(&state),
        read_consent.exists()
    ));
    verdicts.push(verdict(
        "UC9",
        declined.offer_declined && quiet && state.installed.is_some() && !state.offer_declined && read_consent.exists(),
        "no offer after Not now, in either position; Turn on from Settings installed it all the same",
    ));
    drop(store);
    verdicts
}

/// How many files are under `dir`, at any depth.
fn files_under(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| {
                if e.path().is_dir() {
                    files_under(&e.path())
                } else {
                    1
                }
            })
            .sum()
    })
}

/// Every log under `root`, searched for every made-up word. Counts only.
fn no_word_in_any_log(run: &Path, words: &[String]) -> bool {
    let mut files = 0;
    let mut lines = 0;
    let mut leaks = 0;
    let mut stack = vec![run.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.parent().is_some_and(|p| p.ends_with("logs")) {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                files += 1;
                lines += text.lines().count();
                for word in words {
                    if text.contains(word.as_str()) {
                        leaks += 1;
                        println!("  a made-up word is in {}", path.display());
                    }
                }
            }
        }
    }
    verdict(
        "LOGS",
        leaks == 0 && files > 0,
        &format!(
            "{} made-up words looked for in {files} log files ({lines} lines): {leaks} found",
            words.len()
        ),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, run, cases, catalog] = args.as_slice() else {
        eprintln!("usage: reader_acceptance <run dir> <cases dir> <catalog url or file>");
        std::process::exit(2);
    };
    let run = PathBuf::from(run);
    let cases_dir = PathBuf::from(cases);
    let cases = Cases {
        words: serde_json::from_slice(
            &std::fs::read(cases_dir.join("words.json")).expect("words.json"),
        )
        .expect("words"),
        dir: cases_dir,
    };
    let catalog_file = match catalog.strip_prefix("http://") {
        // The terms voice's consent is written with come from the same
        // catalog the Store reads; fetch it once.
        Some(_) => {
            let path = run.join("catalog.json");
            std::fs::create_dir_all(&run).unwrap();
            let bytes = reqwest::blocking::get(catalog.as_str())
                .and_then(|r| r.bytes())
                .expect("the catalog");
            std::fs::write(&path, bytes).unwrap();
            path
        }
        None => PathBuf::from(catalog),
    };
    let mut all = Vec::new();
    all.extend(screenshot_scan_and_failures(&run, &cases, catalog));
    all.extend(voice_consent_and_not_now(
        &run,
        &cases,
        catalog,
        &catalog_file,
    ));
    all.push(no_word_in_any_log(&run, &cases.all_words()));
    let passed = all.iter().filter(|p| **p).count();
    println!("\n{passed} of {} passed", all.len());
    if passed != all.len() {
        std::process::exit(1);
    }
}
