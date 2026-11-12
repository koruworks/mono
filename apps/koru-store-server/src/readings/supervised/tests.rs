// SPDX-License-Identifier: MPL-2.0

//! The conversation with the worker, over bytes in memory: no process, no
//! pipe. The spawn itself is `koru-agent-worker`'s and is tested there.

use std::io::Cursor;

use super::*;
use koru_read::{FailureCode, JobId, PageText, ReadingStatus, Source, Want};

/// What the worker would have written, and what the coordinator wrote to it.
struct Duplex {
    from_worker: Cursor<Vec<u8>>,
    to_worker: Vec<u8>,
}

impl Duplex {
    fn answering(events: &[Event]) -> Duplex {
        let mut bytes = Vec::new();
        for event in events {
            koru_ipc::frame::write_frame(&mut bytes, FRAME_VERSION, event).unwrap();
        }
        Duplex {
            from_worker: Cursor::new(bytes),
            to_worker: Vec::new(),
        }
    }

    fn sent(&self) -> Vec<Request> {
        let mut reader = Cursor::new(self.to_worker.clone());
        let mut out = Vec::new();
        while let Ok((_, request)) = koru_ipc::frame::read_frame::<_, Request>(&mut reader) {
            out.push(request);
        }
        out
    }
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.from_worker.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.to_worker.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn status(launch: Option<&str>, protocol: u32) -> Event {
    Event::Status(WorkerStatus {
        worker_version: "test".into(),
        protocol_version: protocol,
        process_id: 1,
        supervisor_launch_id: launch.map(str::to_owned),
        recognition: false,
        recognition_built: false,
        job: None,
        pages_read: 0,
    })
}

fn read(job: u64) -> Request {
    Request::Read {
        job: JobId(job),
        path: "objects/b3-aa".into(),
        mime: "application/pdf".into(),
        from_page: 1,
        max_text_pages: Some(koru_read::TEXT_PAGE_CAP),
        max_read_pages: Some(koru_read::READ_PAGE_CAP),
        want: Want::Text,
    }
}

/// **`Status` first, and the answer must be this launch's, at this
/// protocol** — or no file is ever sent.
#[test]
fn the_handshake_checks_the_launch_and_the_protocol() {
    let mut ours = Duplex::answering(&[status(Some("launch-1"), PROTOCOL_VERSION)]);
    assert!(handshake(&mut ours, "launch-1").is_ok());
    assert_eq!(
        ours.sent(),
        vec![Request::Status],
        "the first frame is Status"
    );

    let mut theirs = Duplex::answering(&[status(Some("launch-0"), PROTOCOL_VERSION)]);
    assert!(matches!(
        handshake(&mut theirs, "launch-1"),
        Err(Refusal::Wrong(_))
    ));
    let mut by_hand = Duplex::answering(&[status(None, PROTOCOL_VERSION)]);
    assert!(matches!(
        handshake(&mut by_hand, "launch-1"),
        Err(Refusal::Wrong(_))
    ));
    let mut newer = Duplex::answering(&[status(Some("launch-1"), PROTOCOL_VERSION + 1)]);
    assert!(matches!(
        handshake(&mut newer, "launch-1"),
        Err(Refusal::Wrong(_))
    ));
    let mut gone = Duplex::answering(&[]);
    assert_eq!(
        handshake(&mut gone, "launch-1"),
        Err(Refusal::Closed { hung: false })
    );
}

/// A job's events reach the coordinator in order up to its terminal one; a
/// late event for another job is dropped by comparison; a worker that goes
/// away mid-job is a closed connection, not an answer.
#[test]
fn a_job_is_its_events_up_to_the_terminal_one() {
    let page = |job, n| Event::Page {
        job: JobId(job),
        page: n,
        text: PageText::new("words"),
        source: Source::TextLayer,
        confidence: None,
    };
    let mut worker = Duplex::answering(&[
        Event::Failed {
            job: JobId(6),
            code: FailureCode::Cancelled,
        },
        page(7, 1),
        page(7, 2),
        Event::Done {
            job: JobId(7),
            status: ReadingStatus::Read,
            reason: None,
            pages_done: 2,
        },
        page(7, 3),
    ]);
    let mut seen = Vec::new();
    converse(&mut worker, &read(7), &mut |event| seen.push(event)).unwrap();
    assert_eq!(seen.len(), 3, "two pages and the done, nothing after");
    assert!(seen[2].is_terminal());
    assert_eq!(worker.sent(), vec![read(7)]);

    let mut dying = Duplex::answering(&[page(8, 1)]);
    let mut seen = Vec::new();
    assert_eq!(
        converse(&mut dying, &read(8), &mut |event| seen.push(event)),
        Err(Refusal::Closed { hung: false })
    );
    assert_eq!(seen.len(), 1, "the page that arrived was delivered");
}

/// **What a launch hands the worker is what the worker reads**: the command
/// line `--root <root> --supervised`, and one line of the contract's
/// `WorkerBootstrap` at its version, on this channel, carrying the launch id
/// the readiness probe and every job's handshake then demand back.
#[test]
fn a_launch_hands_the_worker_the_contracts_bootstrap() {
    let endpoint = koru_ipc::Endpoint::for_service("koru-read-test", "nightly").unwrap();
    let mut spec = spec(
        PathBuf::from("root"),
        "nightly".into(),
        endpoint,
        Arc::new(Observed::default()),
        None,
    );
    let plan = (spec.launch)(Generation(1)).unwrap();
    assert_eq!(
        plan.args,
        vec![
            std::ffi::OsString::from("--root"),
            "root".into(),
            "--supervised".into()
        ]
    );
    let line: WorkerBootstrap = serde_json::from_slice(&plan.bootstrap).unwrap();
    assert_eq!(line.bootstrap_version, BOOTSTRAP_VERSION);
    assert_eq!(line.channel, "nightly");
    assert_eq!(line.launch_id, plan.payload.launch_id);
    assert!(
        !plan.bootstrap.contains(&b'\n'),
        "one line; the supervisor ends it"
    );
    let again = (spec.launch)(Generation(2)).unwrap();
    assert_ne!(
        again.payload.launch_id, plan.payload.launch_id,
        "every launch is told apart"
    );
}

/// The constants the coordinator's patience is built from.
#[test]
fn the_limits_sit_where_the_measurements_put_them() {
    assert_eq!(MEMORY_LIMIT_BYTES, 1 << 30, "Plan 0.1's measured ceiling");
    assert!(
        LINGER < Duration::from_secs(60),
        "shorter than the worker's idle exit"
    );
    assert!(
        JOB_PATIENCE > Duration::from_secs(30),
        "past the per-page budget"
    );
}

/// A worker that says what it was given, and then is alive and says nothing
/// — the shape a Windows named pipe cannot time out. Its reads block until
/// the test lets go (the worker "is ended"), then report the end.
struct GoesSilent {
    said: Cursor<Vec<u8>>,
    released: std::sync::mpsc::Receiver<()>,
}

impl GoesSilent {
    fn after(events: &[Event]) -> (GoesSilent, std::sync::mpsc::Sender<()>) {
        let (release, released) = std::sync::mpsc::channel();
        let said = Duplex::answering(events).from_worker;
        (GoesSilent { said, released }, release)
    }
}

impl Read for GoesSilent {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.said.read(buf)?;
        if n > 0 {
            return Ok(n);
        }
        let _ = self.released.recv();
        Ok(0)
    }
}

impl Write for GoesSilent {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const SHORT: Duration = Duration::from_millis(100);

/// A worker that answers is heard to the end, events in order, on the calling
/// thread.
#[test]
fn a_patient_conversation_hears_the_whole_job() {
    let worker = Duplex::answering(&[
        status(Some("launch-1"), PROTOCOL_VERSION),
        Event::Page {
            job: JobId(3),
            page: 1,
            text: PageText::new("words"),
            source: Source::TextLayer,
            confidence: None,
        },
        Event::Done {
            job: JobId(3),
            status: ReadingStatus::Read,
            reason: None,
            pages_done: 1,
        },
    ]);
    let mut seen = Vec::new();
    let ended = patiently(
        worker,
        "launch-1".into(),
        read(3),
        SHORT,
        SHORT,
        &mut |event| seen.push(event),
    );
    assert_eq!(ended, Ok(()));
    assert_eq!(seen.len(), 2);
    assert!(seen[1].is_terminal());
}

/// **A worker that accepted and never answered `Status` is given up on at
/// the handshake's patience** — not waited on for ever, which is what a
/// socket timeout on a Windows named pipe amounted to.
#[test]
fn a_worker_silent_at_the_handshake_is_given_up_on() {
    let (worker, release) = GoesSilent::after(&[]);
    let started = Instant::now();
    let ended = patiently(
        worker,
        "launch-1".into(),
        read(4),
        SHORT,
        SHORT,
        &mut |_| {},
    );
    assert_eq!(ended, Err(Broke::Handshake(Refusal::Closed { hung: true })));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    drop(release);
}

/// A worker that goes silent mid-job is hung at the job's patience; what it
/// said before is still delivered, and a wrong launch is refused outright.
#[test]
fn a_worker_silent_mid_job_is_hung_and_what_it_said_arrives() {
    let page = Event::Page {
        job: JobId(5),
        page: 1,
        text: PageText::new("words"),
        source: Source::TextLayer,
        confidence: None,
    };
    let (worker, release) = GoesSilent::after(&[status(Some("launch-1"), PROTOCOL_VERSION), page]);
    let mut seen = Vec::new();
    let ended = patiently(
        worker,
        "launch-1".into(),
        read(5),
        SHORT,
        SHORT,
        &mut |event| seen.push(event),
    );
    assert_eq!(ended, Err(Broke::Job(Refusal::Closed { hung: true })));
    assert_eq!(seen.len(), 1, "the page that arrived was delivered");
    drop(release);

    let theirs = Duplex::answering(&[status(Some("launch-0"), PROTOCOL_VERSION)]);
    assert!(matches!(
        patiently(
            theirs,
            "launch-1".into(),
            read(6),
            SHORT,
            SHORT,
            &mut |_| {}
        ),
        Err(Broke::Handshake(Refusal::Wrong(_)))
    ));
}

/// The launcher's spec names the sibling and its ceiling — and nothing is
/// spawned to find that out.
#[test]
fn the_spec_names_the_sibling_and_its_ceiling() {
    let endpoint = koru_ipc::Endpoint::for_service("koru-read-test", "nightly").unwrap();
    let spec = spec(
        PathBuf::from("root"),
        "nightly".into(),
        endpoint,
        Arc::new(Observed::default()),
        None,
    );
    assert_eq!(spec.program, Program::Sibling(koru_update::READ_EXE.into()));
    assert_eq!(spec.memory_limit, Some(MEMORY_LIMIT_BYTES));
    assert!(spec.graceful_stop.is_none());
}

/// **`--model` is passed when a reader is installed, and only then** (OCR
/// Plan 2.4): without it the worker reads text and never loads a model.
#[test]
fn the_model_is_a_launch_argument_only_when_installed() {
    let root = PathBuf::from("root");
    let model = PathBuf::from("models").join("pp-ocrv6-small");
    assert_eq!(
        launch_args(&root, None),
        ["--root", "root", "--supervised"].map(std::ffi::OsString::from)
    );
    let with = launch_args(&root, Some(&model));
    assert_eq!(&with[..3], &launch_args(&root, None)[..]);
    assert_eq!(with[3], std::ffi::OsString::from("--model"));
    assert_eq!(with[4], model.into_os_string());
}

/// A changed model lets the running supervision go, so the next worker
/// starts with the new argument; the same model keeps it. Nothing is spawned:
/// no supervision was running.
#[test]
fn setting_the_model_forgets_the_last_status_only_when_it_changes() {
    let mut reader = SupervisedReader::new(Path::new("root"), "nightly");
    reader.last_status = Some(match status(Some("l"), PROTOCOL_VERSION) {
        Event::Status(status) => status,
        _ => unreachable!(),
    });
    reader.set_model(None);
    assert!(reader.status().is_some(), "unchanged: the worker is kept");
    reader.set_model(Some(PathBuf::from("model")));
    assert!(reader.status().is_none(), "changed: the worker is let go");
    assert_eq!(reader.model, Some(PathBuf::from("model")));
}
