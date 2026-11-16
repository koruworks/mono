// SPDX-License-Identifier: MPL-2.0

//! **The third milestone's big run on real processes** (OCR ROADMAP Plan 3.4,
//! UC11 and UC12), as far as it goes without a window.
//!
//! ```text
//! cargo build -p koru-store-server --example reader_bulk_run
//! cargo build -p koru-read-worker --features recognition      # ORT_LIB_PATH set; last, see below
//! <fixtures venv>/python -I -B scripts/dev/read-model/make_archive.py <archive dir> [notes] [seed]
//! koru-release read-model package --work <copy of the converted model> \
//!     --base-url http://127.0.0.1:<port>                      # then serve <work>/serve on <port>
//! target/debug/examples/reader_bulk_run archive <run dir> <archive dir> <catalog url>
//! target/debug/examples/reader_bulk_run folder  <run dir> <folder> <catalog url>
//! ```
//!
//! **`archive`** imports a generated Evernote export through the real
//! importer (`koru-import-evernote`), driven the way the shell's import worker
//! drives it (`koru-workspace-host`, `App::import_content`): `inspect`, `build`
//! into a blob sink that stores through the Store's API and counts the
//! pictures and PDFs it keeps, each document written in order, then **one**
//! `reader_import_finished` with that count. Two things differ from the
//! shell, and only these: the tag sink is `NoTagSink` (the generated notes
//! carry no tags), and the count is taken by this file's copy of the shell's
//! rule (`reader_offer::counts_for_reader`, which is crate-private there).
//! Then it observes UC11 (the offer before the install; the counts in a search
//! answer falling after it, scoped to the workspace searched) and UC12 (the
//! Store killed in the middle of the backlog and started again on the same
//! root), checks every made-up word against what was read, and times searches.
//!
//! **`folder`** adds every PDF and picture under a folder to a workspace, the
//! way the Files view adds a file (a blob and a file entry), **titled by
//! number and never by name**, and observes the same reading. It is the run
//! for the maintainer's own folder: what it prints is counts, sizes, timings
//! and reason tokens, and nothing about any file. Before it ends it searches
//! every log under the run's roots for every file's name, in code, and prints
//! only how many matched.
//!
//! Both start their own standalone Stores (`koru-store-server serve --root
//! <dir> --read`, beside this example's directory) under `<run dir>`, with
//! `KORU_READ_CATALOG` set and `KORU_READ_DEBUG` removed, sample the Store's
//! and its read worker's CPU, memory and priority from the OS once a second,
//! and stop every process they started. They delete nothing: the run dir is
//! the caller's to delete.
//!
//! **A trap**: `cargo test -p koru-read-worker`, or any build of the worker
//! without `--features recognition`, replaces the recognizing worker in
//! `target/debug`. Build it last.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Node, NodeId};
use koru_store_client::Client;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::BlobHash;
use koru_store_wire::reader::{ImportFinished, InstallFrom, InstallReader, InstallStage};
use koru_store_wire::readings::ReadingAnswer;
use koru_store_wire::search::{ReadingCounts, SearchQuery, SearchRequest};

// ------------------------------------------------------------------ a Store

/// A standalone Store this run started, and stops.
struct Store {
    child: Child,
    client: Client,
    sampler: Sampler,
}

impl Store {
    fn start(root: &Path, catalog: &str, samples: &Path) -> Store {
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
            // The run that reads private files keeps the per-job lines off.
            .env_remove("KORU_READ_DEBUG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("starting the Store");
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
        let sampler = Sampler::start(child.id(), samples);
        Store {
            child,
            client,
            sampler,
        }
    }

    /// Ended the way a crash or a quit-without-goodbye ends it.
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.sampler.stop();
        // The read worker is the Store's child and dies with it (its Job
        // Object is kill-on-close); give the OS a moment to let it go.
        std::thread::sleep(Duration::from_secs(1));
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.sampler.stop();
    }
}

// ------------------------------------------------------- what the OS says

/// Once a second, the Store's and its children's CPU time, memory and
/// priority class, as the OS reports them, appended to a CSV. Process names,
/// ids and numbers only.
struct Sampler {
    child: Option<Child>,
}

impl Sampler {
    fn start(store: u32, csv: &Path) -> Sampler {
        let script = format!(
            "$ErrorActionPreference='SilentlyContinue'; $out='{out}'; \
             while ($true) {{ \
               $t=[DateTime]::UtcNow.ToString('o'); \
               $ids=@({store}) + @(Get-CimInstance Win32_Process -Filter 'ParentProcessId={store}' | ForEach-Object {{ $_.ProcessId }}); \
               foreach ($i in $ids) {{ $p=Get-Process -Id $i; if ($p) {{ \
                 Add-Content -Path $out -Value (\"$t,$($p.Id),$($p.ProcessName),$($p.TotalProcessorTime.TotalSeconds),$($p.WorkingSet64),$($p.PrivateMemorySize64),$($p.PeakWorkingSet64),$($p.PeakPagedMemorySize64),$($p.PriorityClass)\") }} }}; \
               Start-Sleep -Milliseconds 1000 }}",
            out = csv.display()
        );
        let child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
        Sampler { child }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What the samples say, per process name: CPU seconds used while sampled,
/// the highest private commit and working set any one process reached, the
/// priority classes seen and how many processes of that name there were.
fn summarize_samples(csv: &Path, wall: Duration) {
    let text = std::fs::read_to_string(csv).unwrap_or_default();
    #[derive(Default)]
    struct Per {
        first_last_cpu: HashMap<u32, (f64, f64)>,
        peak_commit: u64,
        peak_working: u64,
        priorities: BTreeMap<String, u64>,
        samples: u64,
    }
    let mut by_name: BTreeMap<String, Per> = BTreeMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 9 {
            continue;
        }
        let pid: u32 = f[1].parse().unwrap_or(0);
        let cpu: f64 = f[3].parse().unwrap_or(0.0);
        let per = by_name.entry(f[2].to_owned()).or_default();
        per.samples += 1;
        let entry = per.first_last_cpu.entry(pid).or_insert((cpu, cpu));
        entry.1 = cpu;
        per.peak_working = per.peak_working.max(f[6].parse().unwrap_or(0));
        per.peak_commit = per.peak_commit.max(f[7].parse().unwrap_or(0));
        *per.priorities.entry(f[8].to_owned()).or_default() += 1;
    }
    for (name, per) in by_name {
        // A process's whole CPU time when it was seen from its start (a
        // worker), its growth while sampled otherwise (the Store, sampled
        // from start too).
        let cpu: f64 = per.first_last_cpu.values().map(|(_, last)| *last).sum();
        println!(
            "  os: {name}: {} process(es), {} samples; CPU {:.1} s ({:.2} cores over {:.0} s); \
             peak private commit {:.0} MiB; peak working set {:.0} MiB; priority {:?}",
            per.first_last_cpu.len(),
            per.samples,
            cpu,
            cpu / wall.as_secs_f64().max(1.0),
            wall.as_secs_f64(),
            per.peak_commit as f64 / 1_048_576.0,
            per.peak_working as f64 / 1_048_576.0,
            per.priorities
        );
    }
}

// --------------------------------------------------------------- the clock

struct Clock(Instant);

impl Clock {
    fn say(&self, what: impl AsRef<str>) {
        println!(
            "[{:>8.1}s] {}",
            self.0.elapsed().as_secs_f64(),
            what.as_ref()
        );
    }
}

// ---------------------------------------------------------- what is asked

fn search_once(
    client: &Client,
    ws: &[WorkspaceId],
    text: &str,
) -> (usize, bool, Option<ReadingCounts>, f64) {
    let started = Instant::now();
    let answer = client
        .search(&SearchRequest {
            query: SearchQuery {
                text: text.to_owned(),
                workspaces: ws.to_vec(),
                ..SearchQuery::default()
            },
            include_facets: false,
            recent_queries: None,
        })
        .expect("search");
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    (
        answer.page.hits.len(),
        answer.page.next_offset.is_some(),
        answer.page.reading,
        ms,
    )
}

/// A search asked `reps` times: hits on the first page, whether there are
/// more, and the median and slowest milliseconds. Never the query's text.
fn timed(client: &Client, ws: &[WorkspaceId], text: &str, reps: usize) -> String {
    let mut times = Vec::new();
    let mut hits = 0;
    let mut more = false;
    for _ in 0..reps {
        let (h, m, _, ms) = search_once(client, ws, text);
        hits = h;
        more = m;
        times.push(ms);
    }
    times.sort_by(f64::total_cmp);
    format!(
        "{hits}{} hits on the first page, median {:.0} ms, slowest {:.0} ms ({reps} asks)",
        if more { "+" } else { "" },
        times[times.len() / 2],
        times[times.len() - 1]
    )
}

fn counts(reading: Option<ReadingCounts>) -> String {
    reading.map_or("none".into(), |c| {
        format!(
            "offer={} needs_reader={} unread={} total={} not_here={}",
            c.offer, c.needs_reader, c.unread, c.total, c.not_here
        )
    })
}

fn board(client: &Client) -> serde_json::Value {
    client.get_json("/v1/debug/readings").expect("board")
}

fn n(v: &serde_json::Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// Items the coordinator finished: read, empty, partial, failed, copied.
fn finished(board: &serde_json::Value) -> u64 {
    let d = &board["coordinator"]["done"];
    n(&d["read"]) + n(&d["empty"]) + n(&d["partial"]) + n(&d["failed"]) + n(&d["copied"])
}

fn queued(board: &serde_json::Value) -> u64 {
    let q = &board["coordinator"]["queued"];
    n(&q["fresh"]) + n(&q["requested"]) + n(&q["backlog"])
}

fn done_line(board: &serde_json::Value) -> String {
    let c = &board["coordinator"];
    format!(
        "done {} | queued {} | worker {} | mirror behind {}",
        c["done"], c["queued"], c["worker"], board["mirror"]["behind"]
    )
}

/// How long a backlog must stay quiet before it counts as drained: longer
/// than the coordinator's linger (`supervised::LINGER`, 30 s), so a pause that
/// ends with the next recompute after an idle wait is not taken for the end.
/// The first archive run used 5 s and stopped inside such a pause.
const SETTLED: Duration = Duration::from_secs(45);

/// Waits until nothing is queued, no job runs and the number of items the
/// coordinator finished has held still for `still`, calling `each` every
/// poll. Returns the time until the quiet began.
fn drain(
    client: &Client,
    limit: Duration,
    still: Duration,
    mut each: impl FnMut(&serde_json::Value),
) -> Duration {
    let started = Instant::now();
    let mut quiet_since: Option<Instant> = None;
    let mut last_finished = u64::MAX;
    loop {
        let b = board(client);
        each(&b);
        let idle = queued(&b) == 0 && b["coordinator"]["job"].is_null();
        let f = finished(&b);
        if idle && f == last_finished {
            let since = *quiet_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= still {
                return since.duration_since(started);
            }
        } else {
            quiet_since = None;
        }
        last_finished = f;
        assert!(
            started.elapsed() < limit,
            "never drained: {}",
            done_line(&b)
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn install(clock: &Clock, client: &Client) -> Duration {
    let started = Instant::now();
    let terms = loop {
        if let Some(terms) = client.reader().expect("reader").offer {
            break terms;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "no offer");
        std::thread::sleep(Duration::from_millis(100));
    };
    clock.say(format!(
        "offer: {} {}, {} bytes",
        terms.model, terms.version, terms.download_bytes
    ));
    client
        .install_reader(&InstallReader {
            terms,
            from: InstallFrom::Offer,
        })
        .expect("install");
    loop {
        let state = client.reader().expect("reader");
        if let Some(progress) = &state.install {
            match progress.stage {
                InstallStage::Installed if state.installed.is_some() => break,
                InstallStage::Failed | InstallStage::Stopped => {
                    panic!("install ended {progress:?}")
                }
                _ => {}
            }
        }
        assert!(started.elapsed() < Duration::from_secs(300), "install");
        std::thread::sleep(Duration::from_millis(50));
    }
    started.elapsed()
}

// ------------------------------------------------------------ sizes on disk

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| {
                let p = e.path();
                if p.is_dir() {
                    dir_size(&p)
                } else {
                    e.metadata().map_or(0, |m| m.len())
                }
            })
            .sum()
    })
}

/// Every file under `dir` whose name starts with `stem` (a database and its
/// `-wal` and `-shm`), in bytes.
fn files_named(dir: &Path, stem: &str) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if e.file_name().to_string_lossy().starts_with(stem) {
                total += e.metadata().map_or(0, |m| m.len());
            }
        }
    }
    total
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB ({bytes} bytes)", bytes as f64 / 1_048_576.0)
}

/// What a device that syncs this workspace receives: every document's whole
/// state, as the Store hands it to a client that has none
/// (`POST /v1/workspaces/{id}/snapshot/{document}`), grouped into what a
/// person can see (`explore::listed`) and what they cannot. Also the reading
/// documents' share, and how long one Store took to hand all of it over
/// loopback HTTP (not a first link: no second device is involved).
fn what_a_device_receives(
    client: &Client,
    ws: WorkspaceId,
    parents: &HashSet<DocumentId>,
) -> (u64, u64) {
    let entries = client.documents(ws).expect("documents");
    let mut by_kind: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut listed = 0u64;
    let mut unlisted = 0u64;
    let started = Instant::now();
    let mut seen: HashSet<DocumentId> = HashSet::new();
    let mut add = |id: DocumentId, kind: &str, by_kind: &mut BTreeMap<String, (u64, u64)>| {
        if !seen.insert(id) {
            return;
        }
        let bytes = client.snapshot(ws, id).map_or(0, |s| s.len() as u64);
        let slot = by_kind.entry(kind.to_owned()).or_default();
        slot.0 += 1;
        slot.1 += bytes;
    };
    for entry in &entries {
        if koru_core::explore::listed(&entry.kind) {
            listed += 1;
        } else {
            unlisted += 1;
        }
        add(entry.id, &entry.kind, &mut by_kind);
    }
    // Reading documents the listing does not name, by their derived id.
    let mut derived = 0u64;
    for parent in parents {
        let id = koru_store_core::ids::reading_document_id(*parent);
        if !entries.iter().any(|e| e.id == id) {
            derived += 1;
            add(id, "koru-reading (derived id)", &mut by_kind);
        }
    }
    let manifest = client.manifest(ws).expect("manifest");
    let manifest_bytes = client.snapshot(ws, manifest).map_or(0, |s| s.len() as u64);
    let took = started.elapsed();
    println!(
        "  documents in the listing: {} ({listed} a person can see, {unlisted} they cannot); \
         reading documents not in the listing: {derived}",
        entries.len()
    );
    let mut reading_bytes = 0;
    let mut total = manifest_bytes;
    for (kind, (count, bytes)) in &by_kind {
        println!("    {kind:<28} {count:>6} documents {:>14}", mib(*bytes));
        if kind.starts_with(koru_core::explore::KIND_READING) {
            reading_bytes += bytes;
        }
        total += bytes;
    }
    println!(
        "    {:<28} {:>6}           {:>14}",
        "manifest",
        1,
        mib(manifest_bytes)
    );
    println!(
        "    everything a syncing device receives: {}; of it reading documents {} ({:.1}%); \
         one Store handed all of it over loopback HTTP in {:.1} s",
        mib(total),
        mib(reading_bytes),
        100.0 * reading_bytes as f64 / total.max(1) as f64,
        took.as_secs_f64()
    );
    (manifest_bytes, reading_bytes)
}

fn manifest_bytes(client: &Client, ws: WorkspaceId) -> u64 {
    let manifest = client.manifest(ws).expect("manifest");
    client.snapshot(ws, manifest).map_or(0, |s| s.len() as u64)
}

// ------------------------------------------------- the importer, as the shell drives it

/// The shell's rule (`koru_workspace_host::reader_offer::counts_for_reader`),
/// copied: that one is crate-private.
fn counts_for_reader(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        mime.as_str(),
        "application/pdf"
            | "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-ms-bmp"
            | "image/tiff"
            | "image/tif"
    )
}

/// The shell's `StoreBlobSink` wrapped in its `CountingBlobSink`, in one: it
/// stores through the Store's API exactly what it is given and counts each
/// distinct picture or PDF once, by the address the Store gave it. It also
/// notes each blob's MD5 (what the archive names an attachment by) against
/// that address, so the run can find each attachment's reading.
struct CountingStoreSink<'a> {
    client: &'a Client,
    workspace: WorkspaceId,
    seen: Mutex<HashSet<String>>,
    by_md5: Mutex<HashMap<String, String>>,
    puts: Mutex<u64>,
}

impl CountingStoreSink<'_> {
    fn note(&self, md5: String, blob: &koru_store_core::model::Blob, asked: &str) {
        *self.puts.lock().unwrap() += 1;
        self.by_md5
            .lock()
            .unwrap()
            .insert(md5, blob.hash.as_str().to_owned());
        if counts_for_reader(&blob.mime) || (blob.mime.is_empty() && counts_for_reader(asked)) {
            self.seen
                .lock()
                .unwrap()
                .insert(blob.hash.as_str().to_owned());
        }
    }

    fn count(&self) -> u64 {
        self.seen.lock().unwrap().len() as u64
    }
}

impl koru_import::BlobSink for CountingStoreSink<'_> {
    fn put(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        let md5 = format!("{:x}", md5::compute(&bytes));
        let blob = self
            .client
            .put_blob(self.workspace, bytes, mime, filename)
            .map_err(|e| koru_import::ImportError::new(e.to_string()))?;
        self.note(md5, &blob, mime);
        Ok(koru_import::StoredBlob {
            hash: blob.hash.as_str().to_owned(),
            mime: blob.mime,
        })
    }

    fn put_from(
        &self,
        factory: &(dyn Fn() -> Box<dyn std::io::Read + Send> + Send + Sync),
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        let mut context = md5::Context::new();
        let mut reader = factory();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let got = reader
                .read(&mut buf)
                .map_err(|e| koru_import::ImportError::new(e.to_string()))?;
            if got == 0 {
                break;
            }
            context.consume(&buf[..got]);
        }
        let md5 = format!("{:x}", context.compute());
        let blob = self
            .client
            .put_blob_from(self.workspace, || factory(), mime, filename)
            .map_err(|e| koru_import::ImportError::new(e.to_string()))?;
        self.note(md5, &blob, mime);
        Ok(koru_import::StoredBlob {
            hash: blob.hash.as_str().to_owned(),
            mime: blob.mime,
        })
    }
}

/// `App::import_content`'s worker, for one `.enex`, against a standalone
/// Store. Returns the sink's MD5-to-address map.
fn import_archive(
    clock: &Clock,
    client: &Client,
    profile: koru_store_core::ids::ProfileId,
    ws: WorkspaceId,
    enex: &Path,
) -> HashMap<String, String> {
    use koru_import::Importer as _;
    let started = Instant::now();
    let prepared = koru_import_evernote::EvernoteImporter
        .inspect(
            koru_import::ImportSource::Files(vec![enex.to_path_buf()]),
            &koru_import::NoProgress,
        )
        .expect("inspect");
    let preview = prepared.preview();
    clock.say(format!(
        "inspected in {:.1} s: documents={} nodes={} images={} warnings={}",
        started.elapsed().as_secs_f64(),
        preview.document_count,
        preview.node_count,
        preview.image_count,
        preview.warnings.len()
    ));
    let occupied: std::collections::BTreeSet<String> = client
        .documents(ws)
        .expect("documents")
        .into_iter()
        .map(|e| e.title)
        .collect();
    let sink = CountingStoreSink {
        client,
        workspace: ws,
        seen: Mutex::new(HashSet::new()),
        by_md5: Mutex::new(HashMap::new()),
        puts: Mutex::new(0),
    };
    let context = koru_import::ImportContext {
        target_profile: Some(profile),
        target_workspace: Some(ws),
        occupied_titles: &occupied,
        blobs: &sink,
        tags: &koru_import::NoTagSink,
    };
    let built_at = Instant::now();
    let product = prepared
        .build(
            koru_import::ImportOptions {
                identity: koru_import::IdentityMode::Copy,
                accept_warnings: true,
            },
            &context,
            &koru_import::NoProgress,
        )
        .expect("build");
    let koru_import::ImportProduct::Documents(content) = product else {
        panic!("the importer produced a workspace");
    };
    let pictures_and_scans = sink.count();
    clock.say(format!(
        "converted in {:.1} s: documents={} warnings={} blobs stored={} pictures_and_scans={}",
        built_at.elapsed().as_secs_f64(),
        content.documents.len(),
        content.warnings.len(),
        *sink.puts.lock().unwrap(),
        pictures_and_scans
    ));
    let mut codes: BTreeMap<String, u64> = BTreeMap::new();
    for warning in &content.warnings {
        *codes
            .entry(
                warning
                    .code
                    .map_or("none".into(), |c| c.as_str().to_owned()),
            )
            .or_default() += 1;
    }
    if !codes.is_empty() {
        clock.say(format!("  warnings by code: {codes:?}"));
    }
    // As the shell does: ask the reader's state once, so the catalog is read
    // while the documents are written.
    if pictures_and_scans > 0 {
        let _ = client.reader();
    }
    let written_at = Instant::now();
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let mut failed = 0;
    for document in content.documents {
        let trashed_at = document.trashed.then(|| {
            document
                .trashed_at
                .unwrap_or_else(koru_store_core::model::Timestamp::now)
        });
        match client.import_document_at_position_trashed(
            ws,
            document.id,
            &document.title,
            &document.kind,
            document.mime.as_deref(),
            document.parent,
            None,
            trashed_at,
            document.snapshot,
        ) {
            Ok(_) => *by_kind.entry(document.kind).or_default() += 1,
            Err(_) => failed += 1,
        }
    }
    clock.say(format!(
        "documents written in {:.1} s: {by_kind:?}, {failed} refused",
        written_at.elapsed().as_secs_f64()
    ));
    // **Once, at the end, with the count** (`reader_offer::report_import`).
    let state = client
        .reader_import_finished(&ImportFinished { pictures_and_scans })
        .expect("reader_import_finished");
    clock.say(format!(
        "reader_import_finished(pictures_and_scans={pictures_and_scans}) called once; it answered: \
         installed={} offer={} offer_declined={} catalog={:?} | whole import {:.1} s",
        state.installed.is_some(),
        state.offer.is_some(),
        state.offer_declined,
        state.catalog.state,
        started.elapsed().as_secs_f64()
    ));
    sink.by_md5.into_inner().unwrap()
}

/// A file, the way the Files view adds one: a blob and a file entry.
fn add_file(
    client: &Client,
    ws: WorkspaceId,
    bytes: Vec<u8>,
    mime: &str,
    title: &str,
) -> Option<BlobHash> {
    let hash = client.put_blob(ws, bytes, mime, None).ok()?.hash;
    add_entry(client, ws, &hash, mime, title).then_some(hash)
}

fn add_entry(client: &Client, ws: WorkspaceId, hash: &BlobHash, mime: &str, title: &str) -> bool {
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
        .is_ok()
}

// --------------------------------------------------------- what was read

#[derive(Default)]
struct Settled {
    by_status: BTreeMap<String, u64>,
    by_source: BTreeMap<String, u64>,
    pages_with_words: u64,
    /// The highest page each reading reached: `pages_done`, else its last
    /// page with words, else 1 for a picture.
    pages_reached: u64,
    /// Each reading that holds words and carries a confidence: the
    /// confidence, and how many characters (not counting spaces) it holds.
    with_words: Vec<(u8, usize)>,
    /// The confidence an `empty` reading carries, when it carries one.
    empty_confidences: Vec<u8>,
    no_reading: u64,
}

fn settle(answers: &[Option<ReadingAnswer>]) -> Settled {
    let mut s = Settled::default();
    for answer in answers {
        let Some(r) = answer else {
            s.no_reading += 1;
            continue;
        };
        let status = format!(
            "{}{}",
            r.status.as_deref().unwrap_or("?"),
            r.reason
                .as_ref()
                .map(|x| format!("/{x}"))
                .unwrap_or_default()
        );
        *s.by_status.entry(status).or_default() += 1;
        *s.by_source
            .entry(r.source.clone().unwrap_or_else(|| "none".into()))
            .or_default() += 1;
        s.pages_with_words += r.pages.len() as u64;
        let last = r.pages.iter().map(|p| p.page).max().unwrap_or(0);
        s.pages_reached += u64::from(r.pages_done.unwrap_or(0).max(last).max(1));
        let chars: usize = r
            .pages
            .iter()
            .map(|p| p.text.chars().filter(|c| !c.is_whitespace()).count())
            .sum();
        match r.confidence {
            Some(c) if chars > 0 => s.with_words.push((c, chars)),
            Some(c) => s.empty_confidences.push(c),
            None => {}
        }
    }
    s
}

fn deciles(mut c: Vec<u8>) -> String {
    if c.is_empty() {
        return "none".into();
    }
    c.sort_unstable();
    let at = |q: usize| c[((c.len() - 1) * q) / 10];
    format!(
        "{} readings: min {} | deciles {} {} {} {} {} {} {} {} {} | max {}",
        c.len(),
        c[0],
        at(1),
        at(2),
        at(3),
        at(4),
        at(5),
        at(6),
        at(7),
        at(8),
        at(9),
        c[c.len() - 1]
    )
}

fn print_settled(s: &Settled) {
    println!(
        "  readings by status: {:?}; no reading: {}",
        s.by_status, s.no_reading
    );
    println!("  readings by source: {:?}", s.by_source);
    println!(
        "  pages with words: {}; pages reached: {}",
        s.pages_with_words, s.pages_reached
    );
    let w = &s.with_words;
    println!(
        "  confidence (0-100; a reading's is the mean of its recognized pages) over readings that hold words: {}",
        deciles(w.iter().map(|(c, _)| *c).collect())
    );
    let under = |t: u8| w.iter().filter(|(c, _)| *c < t).count();
    println!(
        "    under 10: {}; under 20: {}; under 30: {}; a single character: {} (their confidences {:?}); \
         under 5 characters: {} (confidences {:?})",
        under(10),
        under(20),
        under(30),
        w.iter().filter(|(_, n)| *n == 1).count(),
        {
            let mut v: Vec<u8> = w.iter().filter(|(_, n)| *n == 1).map(|(c, _)| *c).collect();
            v.sort_unstable();
            v
        },
        w.iter().filter(|(_, n)| *n < 5).count(),
        {
            let mut v: Vec<u8> = w.iter().filter(|(_, n)| *n < 5).map(|(c, _)| *c).collect();
            v.sort_unstable();
            v
        }
    );
    println!(
        "  empty readings that carry a confidence: {}",
        deciles(s.empty_confidences.clone())
    );
}

/// The name check: every file under a `logs` folder, or named `*.log`,
/// anywhere under `run`, searched for every name given. Counts only.
fn names_in_logs(run: &Path, names: &[String]) -> (usize, usize, usize) {
    let mut files = 0;
    let mut bytes = 0;
    let mut matches = 0;
    let mut stack = vec![run.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let in_logs = p.parent().is_some_and(|d| d.ends_with("logs"));
            let is_log = p.extension().is_some_and(|x| x == "log");
            if !(in_logs || is_log) {
                continue;
            }
            let Ok(raw) = std::fs::read(&p) else { continue };
            let text = String::from_utf8_lossy(&raw).to_lowercase();
            files += 1;
            bytes += raw.len();
            matches += names
                .iter()
                .filter(|name| text.contains(name.as_str()))
                .count();
        }
    }
    (files, bytes, matches)
}

// ------------------------------------------------------------- the archive

struct Attachment {
    kind: String,
    duplicate: bool,
    md5: String,
    words: Vec<String>,
}

fn load_words(dir: &Path) -> (Vec<Attachment>, String, String) {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("words.json")).expect("words.json"))
            .expect("words");
    let list = v["attachments"]
        .as_array()
        .expect("attachments")
        .iter()
        .map(|a| Attachment {
            kind: a["kind"].as_str().unwrap().to_owned(),
            duplicate: a["duplicate"].as_bool().unwrap_or(false),
            md5: a["md5"].as_str().unwrap().to_owned(),
            words: a["words"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| w.as_str().unwrap().to_owned())
                .collect(),
        })
        .collect();
    (
        list,
        v["common"].as_str().unwrap().to_owned(),
        v["absent"].as_str().unwrap().to_owned(),
    )
}

fn readings_for(client: &Client, ws: WorkspaceId, hashes: &[String]) -> Vec<Option<ReadingAnswer>> {
    hashes
        .iter()
        .map(|h| {
            BlobHash::parse(h)
                .ok()
                .and_then(|hash| client.reading(ws, &hash).ok().flatten())
        })
        .collect()
}

fn archive(run: &Path, dir: &Path, catalog: &str) {
    let clock = Clock(Instant::now());
    let root = run.join("archive-store");
    let samples = run.join("archive-samples.csv");
    let (attachments, common, absent) = load_words(dir);
    let enex = dir.join("generated.enex");
    println!(
        "== the generated archive: {} notes, {} bytes",
        attachments.len(),
        std::fs::metadata(&enex).map_or(0, |m| m.len())
    );
    let run_started = Instant::now();
    let mut store = Store::start(&root, catalog, &samples);
    let profile = store
        .client
        .create_profile("Big run", None, None, Some("en"))
        .expect("profile");
    let ws = store
        .client
        .create_workspace(profile.id, "Evernote")
        .expect("ws")
        .id;
    let other = store
        .client
        .create_workspace(profile.id, "Other")
        .expect("ws")
        .id;
    let search_db_empty = files_named(&root, "search.db");

    // ---- UC11: the import, through the importer.
    println!("\n== UC11: the import");
    let import_started = Instant::now();
    let by_md5 = import_archive(&clock, &store.client, profile.id, ws, &enex);
    let unique: Vec<&Attachment> = attachments.iter().filter(|a| !a.duplicate).collect();
    let hashes: Vec<String> = unique
        .iter()
        .map(|a| by_md5.get(&a.md5).cloned().unwrap_or_default())
        .collect();
    clock.say(format!(
        "attachments: {} in the archive, {} distinct, {} found by MD5 among the stored blobs",
        attachments.len(),
        unique.len(),
        hashes.iter().filter(|h| !h.is_empty()).count()
    ));

    // The second workspace: five of the same pictures, added as files.
    let mut shared = 0;
    for (a, h) in unique.iter().zip(&hashes) {
        if shared == 5 {
            break;
        }
        if a.kind == "screenshot"
            && let Ok(hash) = BlobHash::parse(h)
            && let Ok(bytes) = store.client.get_blob(ws, &hash)
            && add_file(
                &store.client,
                other,
                bytes,
                "image/png",
                &format!("Picture {shared}.png"),
            )
            .is_some()
        {
            shared += 1;
        }
    }
    clock.say(format!(
        "{shared} of the archive's pictures also added to a second workspace"
    ));

    // ---- Text first: no reader yet. What a search answer says meanwhile.
    let manifest_after_import = manifest_bytes(&store.client, ws);
    let mut seen_searches = Vec::new();
    let text_phase = drain(
        &store.client,
        Duration::from_secs(3_600),
        Duration::from_secs(4),
        |b| {
            if seen_searches.len() < 3 && queued(b) > 0 {
                seen_searches.push(search_once(&store.client, &[ws], &absent).3);
            }
        },
    );
    let b = board(&store.client);
    clock.say(format!(
        "text layers read with no reader installed: drained {:.1} s after the import ended, {:.1} s after it began \
         (reading starts while the import writes); {}",
        text_phase.as_secs_f64(),
        import_started.elapsed().as_secs_f64(),
        done_line(&b)
    ));
    let text_pages = n(&b["coordinator"]["done"]["pages"]);
    clock.say(format!(
        "  {text_pages} pages in {:.1} s: {:.0} pages per second (text layer, and the text check of scanned pages)",
        text_phase.as_secs_f64(),
        text_pages as f64 / text_phase.as_secs_f64().max(0.001)
    ));
    let (_, _, scoped_ws, _) = search_once(&store.client, &[ws], &absent);
    let (_, _, scoped_other, _) = search_once(&store.client, &[other], &absent);
    let (_, _, scoped_both, _) = search_once(&store.client, &[ws, other], &absent);
    let state = store.client.reader().expect("reader");
    clock.say(format!(
        "before the install, a search in the archive's workspace: {}",
        counts(scoped_ws)
    ));
    clock.say(format!(
        "  the same search in the second workspace: {}",
        counts(scoped_other)
    ));
    clock.say(format!("  in both: {}", counts(scoped_both)));
    clock.say(format!(
        "  the reader: installed={} offer={} declined={} machine counts {:?}",
        state.installed.is_some(),
        state.offer.is_some(),
        state.offer_declined,
        state.counts
    ));
    clock.say(format!(
        "  searches for a word that matches nothing, while text was read: {seen_searches:.0?} ms"
    ));
    let needs_reader_at_install = state.counts.needs_reader;

    // ---- Turn on, and watch the counts fall.
    println!("\n== UC11: Turn on, and the backlog");
    let took = install(&clock, &store.client);
    clock.say(format!("installed in {:.1} s", took.as_secs_f64()));
    let at_install = board(&store.client);
    let backlog_started = Instant::now();
    let rare = unique
        .iter()
        .find(|a| a.kind == "text-pdf")
        .and_then(|a| a.words.first())
        .cloned()
        .unwrap_or_default();
    // UC12 kills the Store when half of what waited is left.
    let unread_at_install = scoped_ws.map_or(0, |c| c.unread);
    let kill_at = unread_at_install / 2;
    let mut last_report = Instant::now();
    let mut watch = |client: &Client,
                     series: &mut Vec<(f64, u64)>,
                     during: &mut Vec<String>|
     -> u64 {
        let (_, _, c, ms) = search_once(client, &[ws], &absent);
        let unread = c.map_or(0, |c| c.unread);
        series.push((backlog_started.elapsed().as_secs_f64(), unread));
        if during.len() < 3
            && backlog_started.elapsed() > Duration::from_secs(20 * (during.len() as u64 + 1))
        {
            during.push(format!(
                "rare word: {} | common word: {} | a word that matches nothing: {} (the polling ask {:.0} ms)",
                timed(client, &[ws], &rare, 3),
                timed(client, &[ws], &common, 3),
                timed(client, &[ws], &absent, 3),
                ms
            ));
        }
        if last_report.elapsed() > Duration::from_secs(30) {
            clock.say(format!("  backlog: {}", counts(c)));
            last_report = Instant::now();
        }
        unread
    };
    let mut series: Vec<(f64, u64)> = Vec::new();
    let mut during: Vec<String> = Vec::new();
    while watch(&store.client, &mut series, &mut during) > kill_at {
        assert!(
            backlog_started.elapsed() < Duration::from_secs(7_200),
            "the backlog never got halfway"
        );
        std::thread::sleep(Duration::from_millis(1_000));
    }

    // ---- UC12: quit in the middle of it, and start again.
    println!("\n== UC12: the Store stopped in the middle of the backlog");
    let before_kill = board(&store.client);
    let (_, _, before_counts, _) = search_once(&store.client, &[ws], &absent);
    let kill_elapsed = backlog_started.elapsed();
    clock.say(format!(
        "killing the Store: search counts {} | {}",
        counts(before_counts),
        done_line(&before_kill)
    ));
    summarize_samples(&samples, run_started.elapsed());
    // What an update needs of the worker: that stopping the Store lets go of
    // the worker's executable, so it can be replaced. Asked by opening it for
    // writing (nothing is written): refused while a worker runs from it.
    let worker_exe = std::env::current_exe()
        .expect("this example's path")
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/examples")
        .join(format!("koru-read-worker{}", std::env::consts::EXE_SUFFIX));
    let writable = || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&worker_exe)
            .is_ok()
    };
    let before = writable();
    store.kill();
    clock.say(format!(
        "the worker's executable could be opened for writing: {} while the Store ran, {} once it was stopped",
        before,
        writable()
    ));
    let restarted = Instant::now();
    let samples_2 = run.join("archive-samples-2.csv");
    store = Store::start(&root, catalog, &samples_2);
    let first = loop {
        let (_, _, c, _) = search_once(&store.client, &[ws], &absent);
        if c.is_some_and(|c| c.total > 0) {
            break c;
        }
        assert!(
            restarted.elapsed() < Duration::from_secs(120),
            "no counts after the restart"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    clock.say(format!(
        "started again; {:.1} s later the first search answer with counts: {}",
        restarted.elapsed().as_secs_f64(),
        counts(first)
    ));
    let second_store = drain(&store.client, Duration::from_secs(7_200), SETTLED, |_| {
        watch(&store.client, &mut series, &mut during);
    });
    let backlog = backlog_started.elapsed();
    let after = board(&store.client);
    clock.say(format!(
        "drained {:.1} s after the install ({:.1} s of it on the second Store); {}",
        backlog.as_secs_f64(),
        second_store.as_secs_f64(),
        done_line(&after)
    ));
    let s1 = &before_kill["coordinator"]["done"];
    let s0 = &at_install["coordinator"]["done"];
    let s2 = &after["coordinator"]["done"];
    let d = |k: &str| n(&s1[k]).saturating_sub(n(&s0[k]));
    let s1_items = d("read") + d("empty") + d("partial") + d("failed") + d("copied");
    let s2_items = finished(&after);
    clock.say(format!(
        "UC12 counters: waiting for the reader at the install {needs_reader_at_install}; first Store after the install \
         read={} empty={} partial={} failed={} copied={} pages={}; second Store read={} empty={} partial={} failed={} copied={} pages={}; \
         items {s1_items} + {s2_items} = {}",
        d("read"), d("empty"), d("partial"), d("failed"), d("copied"), d("pages"),
        n(&s2["read"]), n(&s2["empty"]), n(&s2["partial"]), n(&s2["failed"]), n(&s2["copied"]), n(&s2["pages"]),
        s1_items + s2_items
    ));
    clock.say(format!(
        "manifest commits registering reading documents: {} on the first Store (import and backlog), {} on the second",
        n(&s1["manifest_commits"]),
        n(&s2["manifest_commits"])
    ));
    let pages_backlog = d("pages") + n(&s2["pages"]);
    clock.say(format!(
        "recognition: {pages_backlog} pages and pictures in {:.1} s of backlog ({:.1} s before the kill, {:.1} s down): {:.2} per second",
        backlog.as_secs_f64(),
        kill_elapsed.as_secs_f64(),
        restarted.elapsed().as_secs_f64() - second_store.as_secs_f64(),
        pages_backlog as f64 / backlog.as_secs_f64()
    ));
    let falling: Vec<String> = series
        .iter()
        .step_by((series.len() / 16).max(1))
        .map(|(t, u)| format!("{t:.0}s:{u}"))
        .collect();
    clock.say(format!(
        "unread in the archive's workspace as searches saw it: {}",
        falling.join(" ")
    ));
    for line in &during {
        clock.say(format!("search while reading: {line}"));
    }
    summarize_samples(&samples_2, restarted.elapsed());

    // ---- What was read, by kind of attachment.
    println!("\n== what was read correctly");
    let answers = readings_for(&store.client, ws, &hashes);
    let mut tally: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
    let mut parents = HashSet::new();
    for (a, answer) in unique.iter().zip(&answers) {
        let slot = tally.entry(a.kind.clone()).or_default();
        slot.0 += 1;
        if let Some(r) = answer {
            parents.insert(r.document);
        }
        for (i, word) in a.words.iter().enumerate() {
            slot.1 += 1;
            let page = i as u32 + 1;
            let found_on_page = answer.as_ref().is_some_and(|r| {
                r.pages.iter().any(|p| {
                    (p.page == page || r.pages.len() == 1)
                        && p.text.to_lowercase().contains(word.as_str())
                })
            });
            if found_on_page {
                slot.2 += 1;
            } else if a.kind == "long-scan-pdf" && page > koru_read::READ_PAGE_CAP {
                slot.3 += 1;
            }
        }
    }
    for (kind, (items, words, found, past_cap)) in &tally {
        println!(
            "  {kind:<14} {items:>5} attachments {words:>6} words, {found:>6} read on their page ({:.1}%){}",
            100.0 * *found as f64 / (*words).max(1) as f64,
            if *past_cap > 0 {
                format!(
                    "; {past_cap} past the {}-page limit, not read by design",
                    koru_read::READ_PAGE_CAP
                )
            } else {
                String::new()
            }
        );
    }
    let settled = settle(&answers);
    print_settled(&settled);
    let rare_found = {
        let (hits, _, _, _) = search_once(&store.client, &[ws], &rare);
        hits
    };
    // Search finds what was read: a sample of read words, each by search.
    let mut sampled = 0;
    let mut by_search = 0;
    for (a, answer) in unique.iter().zip(&answers) {
        if sampled >= 100 {
            break;
        }
        let (Some(r), Some(word)) = (answer, a.words.first()) else {
            continue;
        };
        if !r
            .pages
            .iter()
            .any(|p| p.text.to_lowercase().contains(word.as_str()))
        {
            continue;
        }
        sampled += 1;
        let answer = store
            .client
            .search(&SearchRequest {
                query: SearchQuery {
                    text: word.clone(),
                    workspaces: vec![ws],
                    ..SearchQuery::default()
                },
                include_facets: false,
                recent_queries: None,
            })
            .expect("search");
        let key = r.document.to_storage();
        if answer
            .page
            .hits
            .iter()
            .any(|h| h.document.as_deref() == Some(key.as_str()))
        {
            by_search += 1;
        }
    }
    clock.say(format!(
        "search finds what was read: {by_search} of {sampled} read words (one per attachment, in archive order) found on their document; the rare word: {rare_found} hit(s)"
    ));

    // ---- Search on the finished index.
    println!("\n== search on the finished index");
    clock.say(format!(
        "rare word: {}",
        timed(&store.client, &[ws], &rare, 5)
    ));
    clock.say(format!(
        "common word: {}",
        timed(&store.client, &[ws], &common, 5)
    ));
    clock.say(format!(
        "a word that matches nothing: {}",
        timed(&store.client, &[ws], &absent, 5)
    ));
    let (_, _, final_ws, _) = search_once(&store.client, &[ws], &absent);
    let (_, _, final_other, _) = search_once(&store.client, &[other], &absent);
    clock.say(format!(
        "counts now: archive {} | second workspace {}",
        counts(final_ws),
        counts(final_other)
    ));
    let state = store.client.reader().expect("reader");
    clock.say(format!("machine counts: {:?}", state.counts));

    // ---- Sizes, and what a phone carries.
    println!("\n== sizes, and what a device that syncs the workspace receives");
    let (manifest_now, _) = what_a_device_receives(&store.client, ws, &parents);
    clock.say(format!(
        "manifest: {} after the import, {} now (+{} from registering reading documents)",
        mib(manifest_after_import),
        mib(manifest_now),
        manifest_now as i64 - manifest_after_import as i64
    ));
    clock.say(format!(
        "on disk: readings.db {} | search.db {} (it was {} in an empty store) | the whole root {}",
        mib(files_named(&root, "readings.db")),
        mib(files_named(&root, "search.db")),
        mib(search_db_empty),
        mib(dir_size(&root))
    ));
    drop(store);
}

// -------------------------------------------------------------- the folder

const READABLE: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
];

/// Every PDF and picture under `folder`, iterated in code. Never printed.
fn readable_files(folder: &Path) -> (Vec<(PathBuf, &'static str, u64)>, u64) {
    let mut out = Vec::new();
    let mut unreadable_dirs = 0;
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            unreadable_dirs += 1;
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let p = e.path();
            if kind.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if let Some((_, mime)) = READABLE.iter().find(|(x, _)| *x == ext) {
                out.push((p, *mime, e.metadata().map_or(0, |m| m.len())));
            }
        }
    }
    (out, unreadable_dirs)
}

fn folder(run: &Path, folder: &Path, catalog: &str) {
    let clock = Clock(Instant::now());
    let root = run.join("folder-store");
    let samples = run.join("folder-samples.csv");
    let (files, unreadable_dirs) = readable_files(folder);
    let total: u64 = files.iter().map(|f| f.2).sum();
    let mut by_mime: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for (_, mime, size) in &files {
        let slot = by_mime.entry(mime).or_default();
        slot.0 += 1;
        slot.1 += size;
    }
    println!(
        "== the folder: {} PDFs and pictures, {} ({unreadable_dirs} folders could not be listed)",
        files.len(),
        mib(total)
    );
    for (mime, (count, bytes)) in &by_mime {
        println!("  {mime:<16} {count:>6} files {:>24}", mib(*bytes));
    }
    // Stop before a root would pass 40 GB.
    assert!(
        total < 40 * 1024 * 1024 * 1024,
        "the folder's PDFs and pictures exceed 40 GB"
    );

    let run_started = Instant::now();
    let store = Store::start(&root, catalog, &samples);
    let client = &store.client;
    let profile = client
        .create_profile("Folder run", None, None, Some("en"))
        .expect("profile");
    let ws = client
        .create_workspace(profile.id, "Documents")
        .expect("ws")
        .id;
    let search_db_empty = files_named(&root, "search.db");

    println!("\n== UC11: the folder added to a workspace");
    let added_at = Instant::now();
    let mut added = 0u64;
    let mut refused = 0u64;
    let mut hashes: Vec<String> = Vec::new();
    for (i, (path, mime, _)) in files.iter().enumerate() {
        let ext = READABLE
            .iter()
            .find(|(_, m)| m == mime)
            .map_or("bin", |(x, _)| *x);
        let path = path.clone();
        let stored = client.put_blob_from(
            ws,
            move || -> Box<dyn std::io::Read + Send> {
                match std::fs::File::open(&path) {
                    Ok(f) => Box::new(std::io::BufReader::new(f)),
                    Err(_) => Box::new(std::io::empty()),
                }
            },
            mime,
            None,
        );
        match stored {
            Ok(blob) if add_entry(client, ws, &blob.hash, mime, &format!("File {i}.{ext}")) => {
                added += 1;
                hashes.push(blob.hash.as_str().to_owned());
            }
            _ => refused += 1,
        }
    }
    let distinct: HashSet<&String> = hashes.iter().collect();
    clock.say(format!(
        "{added} files added ({} distinct contents), {refused} refused, in {:.1} s",
        distinct.len(),
        added_at.elapsed().as_secs_f64()
    ));
    let manifest_after_add = manifest_bytes(client, ws);

    let text_phase = drain(
        client,
        Duration::from_secs(7_200),
        Duration::from_secs(5),
        |_| {},
    );
    let b = board(client);
    let text_pages = n(&b["coordinator"]["done"]["pages"]);
    clock.say(format!(
        "with no reader installed: drained {:.1} s after the files were added; {}",
        text_phase.as_secs_f64(),
        done_line(&b)
    ));
    clock.say(format!(
        "  {text_pages} pages in {:.1} s: {:.0} pages per second (text layers, and the text check of scanned pages)",
        text_phase.as_secs_f64(),
        text_pages as f64 / text_phase.as_secs_f64().max(0.001)
    ));
    let (_, _, before, _) = search_once(client, &[ws], "zzqxjvwk");
    let state = client.reader().expect("reader");
    clock.say(format!(
        "before the install: {} | reader offer={} | machine {:?}",
        counts(before),
        state.offer.is_some(),
        state.counts
    ));
    let at_install = b;

    println!("\n== Turn on, and the backlog");
    let took = install(&clock, client);
    clock.say(format!("installed in {:.1} s", took.as_secs_f64()));
    let backlog_started = Instant::now();
    let mut last_report = Instant::now();
    let mut series = Vec::new();
    let backlog = drain(client, Duration::from_secs(14_400), SETTLED, |b| {
        if last_report.elapsed() > Duration::from_secs(60) {
            let (_, _, c, ms) = search_once(client, &[ws], "zzqxjvwk");
            series.push(format!(
                "{:.0}s:{}",
                backlog_started.elapsed().as_secs_f64(),
                c.map_or(0, |c| c.unread)
            ));
            clock.say(format!(
                "  backlog: {} | {} | a search {ms:.0} ms",
                counts(c),
                done_line(b)
            ));
            last_report = Instant::now();
        }
    });
    let after = board(client);
    let d = |k: &str| {
        n(&after["coordinator"]["done"][k]).saturating_sub(n(&at_install["coordinator"]["done"][k]))
    };
    clock.say(format!(
        "drained {:.1} s after the install; {}",
        backlog.as_secs_f64(),
        done_line(&after)
    ));
    clock.say(format!(
        "  after the install: read={} empty={} partial={} failed={} copied={} pages={}: {:.2} pages and pictures per second",
        d("read"), d("empty"), d("partial"), d("failed"), d("copied"), d("pages"),
        d("pages") as f64 / backlog.as_secs_f64().max(0.001)
    ));
    clock.say(format!("  unread as searches saw it: {}", series.join(" ")));
    clock.say(format!(
        "manifest commits registering reading documents: {}; worker respawns {} memory kills {}",
        n(&after["coordinator"]["done"]["manifest_commits"]),
        n(&after["coordinator"]["worker"]["respawns"]),
        n(&after["coordinator"]["worker"]["memory_kills"])
    ));
    let state = client.reader().expect("reader");
    clock.say(format!("machine counts: {:?}", state.counts));

    println!("\n== what was read");
    let unique: Vec<String> = distinct.iter().map(|h| (*h).clone()).collect();
    let answers = readings_for(client, ws, &unique);
    let settled = settle(&answers);
    print_settled(&settled);
    let mut parents = HashSet::new();
    for r in answers.iter().flatten() {
        parents.insert(r.document);
    }
    drop(answers);

    println!("\n== search: three function words");
    for word in ["the", "and", "2023"] {
        clock.say(format!(
            "a function word: {}",
            timed(client, &[ws], word, 5)
        ));
    }

    println!("\n== sizes, and what a device that syncs the workspace receives");
    let (manifest_now, _) = what_a_device_receives(client, ws, &parents);
    clock.say(format!(
        "manifest: {} after the files were added, {} now (+{})",
        mib(manifest_after_add),
        mib(manifest_now),
        manifest_now as i64 - manifest_after_add as i64
    ));
    clock.say(format!(
        "on disk: readings.db {} | search.db {} (it was {} in an empty store) | the blob store and everything else: root {}",
        mib(files_named(&root, "readings.db")),
        mib(files_named(&root, "search.db")),
        mib(search_db_empty),
        mib(dir_size(&root))
    ));
    let wall = run_started.elapsed();
    drop(store);
    summarize_samples(&samples, wall);

    // ---- The name check, in code: counts only.
    let mut names: HashSet<String> = HashSet::new();
    for (path, _, _) in &files {
        if let Some(name) = path.file_name() {
            names.insert(name.to_string_lossy().to_lowercase());
        }
        if let Some(stem) = path.file_stem() {
            let stem = stem.to_string_lossy().to_lowercase();
            // A stem of a few characters ("1", "scan") would match anything.
            if stem.chars().count() >= 6 {
                names.insert(stem);
            }
        }
    }
    let names: Vec<String> = names.into_iter().collect();
    let (logs, bytes, matches) = names_in_logs(run, &names);
    println!(
        "\nNAME CHECK: {} file names (with extension, and stems of 6 or more characters) looked for in {logs} log files ({bytes} bytes): {matches} matches",
        names.len()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, mode, run, source, catalog] = args.as_slice() else {
        eprintln!(
            "usage: reader_bulk_run archive|folder <run dir> <archive dir|folder> <catalog url>"
        );
        std::process::exit(2);
    };
    let run = PathBuf::from(run);
    std::fs::create_dir_all(&run).expect("the run dir");
    match mode.as_str() {
        "archive" => archive(&run, Path::new(source), catalog),
        "folder" => folder(&run, Path::new(source), catalog),
        _ => {
            eprintln!("unknown mode");
            std::process::exit(2);
        }
    }
}
