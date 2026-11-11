// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::budget::Abandoned;
use crate::fixtures::{self, Memory, Script, stuck};
use koru_read::{ReadingStatus, Want};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

const WAIT: Duration = Duration::from_secs(20);

fn lenient() -> Arc<Abandoned> {
    Abandoned::new(64, Duration::from_secs(3600), |_| {})
}

fn real(files: Memory) -> Service {
    let engine = Engine {
        files: Arc::new(files),
        ..Engine::product(lenient())
    };
    Service::new(engine, Some("launch-42".into()))
}

fn read(job: u64, path: &str) -> Request {
    Request::Read {
        job: JobId(job),
        path: PathBuf::from(path),
        mime: "application/pdf".into(),
        from_page: 1,
        max_text_pages: Some(koru_read::TEXT_PAGE_CAP),
        max_read_pages: Some(koru_read::READ_PAGE_CAP),
        want: Want::Text,
    }
}

/// Every event until the job's terminal one, which is last.
fn until_end(events: &Receiver<Event>, job: u64) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let event = events.recv_timeout(WAIT).expect("the job ended in time");
        let end = event.job() == Some(JobId(job)) && event.is_terminal();
        seen.push(event);
        if end {
            return seen;
        }
    }
}

/// **The handshake**: the answer to the first `Status` carries the launch id
/// the Store wrote in the bootstrap, and this protocol's version.
#[test]
fn status_names_the_launch_and_starts_nothing() {
    let service = real(Memory::default());
    let (sink, events) = mpsc::channel();
    service.handle(1, Request::Status, &sink);
    let Event::Status(status) = events.recv_timeout(WAIT).unwrap() else {
        panic!("status answers with a status");
    };
    assert_eq!(status.supervisor_launch_id.as_deref(), Some("launch-42"));
    assert_eq!(status.protocol_version, PROTOCOL_VERSION);
    assert_eq!(status.process_id, std::process::id());
    assert!(!status.recognition, "this build has no recognition");
    assert_eq!(status.job, None);
    assert!(events.try_recv().is_err(), "a status starts nothing");
}

/// **A read streams its pages and ends with exactly one terminal event.**
#[test]
fn a_read_streams_pages_and_ends_once() {
    let service = real(Memory::default().with("objects/b3-two", fixtures::two_pages()));
    let (sink, events) = mpsc::channel();
    service.handle(1, read(1, "objects/b3-two"), &sink);
    let seen = until_end(&events, 1);
    let pages: Vec<u32> = seen
        .iter()
        .filter_map(|event| match event {
            Event::Page { page, .. } => Some(*page),
            _ => None,
        })
        .collect();
    assert_eq!(pages, [1, 2]);
    assert_eq!(seen.iter().filter(|event| event.is_terminal()).count(), 1);
    assert!(matches!(
        seen.last(),
        Some(Event::Done {
            status: ReadingStatus::Read,
            pages_done: 2,
            ..
        })
    ));
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
    let status = service.status();
    assert_eq!(status.job, None);
    assert_eq!(status.pages_read, 2);
}

/// **One job at a time**: a second `Read` is refused for itself, and the
/// first carries on; a cancel then ends the first.
#[test]
fn a_second_read_is_busy_and_a_cancel_ends_the_first() {
    let (_release, page) = stuck();
    let engine = fixtures::scripted_engine(vec![page], "objects/b3-s", WAIT, lenient());
    let service = Service::new(engine, None);
    let (sink, events) = mpsc::channel();
    service.handle(1, read(1, "objects/b3-s"), &sink);
    assert_eq!(service.status().job, Some(JobId(1)));
    assert_eq!(
        service.idle_for(Instant::now()),
        None,
        "a running job is not idle"
    );

    service.handle(1, read(2, "objects/b3-s"), &sink);
    assert_eq!(
        events.recv_timeout(WAIT).unwrap(),
        Event::Failed {
            job: JobId(2),
            code: FailureCode::Busy
        }
    );
    assert_eq!(service.status().job, Some(JobId(1)), "the first still runs");

    // A cancel for a job that is not running answers nothing.
    service.handle(1, Request::Cancel { job: JobId(2) }, &sink);
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());

    service.handle(1, Request::Cancel { job: JobId(1) }, &sink);
    assert_eq!(
        until_end(&events, 1),
        [Event::Failed {
            job: JobId(1),
            code: FailureCode::Cancelled
        }]
    );
    assert_eq!(service.status().job, None);
    assert!(service.idle_for(Instant::now()).is_some());
}

/// The slot is free before `Done` is sent, so the next `Read`, sent the
/// moment `Done` arrives, is taken.
#[test]
fn a_read_sent_the_moment_done_arrives_is_not_busy() {
    let service = real(Memory::default().with("objects/b3-one", fixtures::one_page()));
    let (sink, events) = mpsc::channel();
    for job in 1..=5 {
        service.handle(1, read(job, "objects/b3-one"), &sink);
        let seen = until_end(&events, job);
        assert!(
            matches!(seen.last(), Some(Event::Done { .. })),
            "job {job} ended {:?}",
            seen.last()
        );
    }
}

/// A connection that goes away cancels its own job, and only its own.
#[test]
fn a_closed_connection_cancels_the_job_it_started() {
    let (_release, page) = stuck();
    let engine = fixtures::scripted_engine(
        vec![Script::Words("first page words"), page],
        "objects/b3-s",
        WAIT,
        lenient(),
    );
    let service = Service::new(engine, None);
    let (sink, events) = mpsc::channel();
    service.handle(7, read(1, "objects/b3-s"), &sink);
    service.connection_closed(8);
    assert!(matches!(
        events.recv_timeout(WAIT).unwrap(),
        Event::Page { page: 1, .. }
    ));
    assert_eq!(service.status().job, Some(JobId(1)));
    service.connection_closed(7);
    assert_eq!(
        events.recv_timeout(WAIT).unwrap(),
        Event::Failed {
            job: JobId(1),
            code: FailureCode::Cancelled
        }
    );
}

#[test]
fn a_status_while_a_job_runs_names_the_job() {
    let (_release, page) = stuck();
    let engine = fixtures::scripted_engine(vec![page], "objects/b3-s", WAIT, lenient());
    let service = Service::new(engine, None);
    let (sink, events) = mpsc::channel();
    service.handle(1, read(3, "objects/b3-s"), &sink);
    service.handle(2, Request::Status, &sink);
    let Event::Status(status) = events.recv_timeout(WAIT).unwrap() else {
        panic!("status answers with a status");
    };
    assert_eq!(status.job, Some(JobId(3)));
    assert_eq!(status.supervisor_launch_id, None, "started by hand");
    service.handle(1, Request::Cancel { job: JobId(3) }, &sink);
    until_end(&events, 3);
}
