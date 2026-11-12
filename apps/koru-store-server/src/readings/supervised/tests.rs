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

/// **The bootstrap line has exactly the worker plan's shape**:
/// `{"bootstrap_version":1,"launch_id":…,"channel":…}`.
#[test]
fn the_bootstrap_is_the_worker_plans_shape() {
    let line = serde_json::to_value(Bootstrap {
        bootstrap_version: BOOTSTRAP_VERSION,
        launch_id: "a-launch".into(),
        channel: "nightly".into(),
    })
    .unwrap();
    assert_eq!(
        line,
        serde_json::json!({"bootstrap_version": 1, "launch_id": "a-launch", "channel": "nightly"})
    );
    assert_eq!(SERVICE, "koru-read");
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
    );
    assert_eq!(spec.program, Program::Sibling(koru_update::READ_EXE.into()));
    assert_eq!(spec.memory_limit, Some(MEMORY_LIMIT_BYTES));
    assert!(spec.graceful_stop.is_none());
}
