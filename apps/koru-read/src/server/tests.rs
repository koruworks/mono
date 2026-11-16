// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::budget::Abandoned;
use crate::fixtures::{self, Memory};
use crate::job::Engine;
use koru_read::{JobId, PROTOCOL_VERSION, Want};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the worker wrote, shared with the test.
#[derive(Clone, Default)]
struct Written(Arc<Mutex<Vec<u8>>>);

impl Write for Written {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Written {
    fn events(&self) -> Vec<Event> {
        let bytes = self.0.lock().unwrap().clone();
        let mut reader = std::io::Cursor::new(bytes);
        let mut events = Vec::new();
        while (reader.position() as usize) < reader.get_ref().len() {
            let (version, event) = koru_ipc::frame::read_frame::<_, Event>(&mut reader).unwrap();
            assert_eq!(version, FRAME_VERSION);
            events.push(event);
        }
        events
    }
}

fn frames(requests: &[Request]) -> std::io::Cursor<Vec<u8>> {
    let mut bytes = Vec::new();
    for request in requests {
        koru_ipc::frame::write_frame(&mut bytes, FRAME_VERSION, request).unwrap();
    }
    std::io::Cursor::new(bytes)
}

fn service() -> Service {
    let engine = Engine {
        files: Arc::new(Memory::default().with("objects/b3-one", fixtures::one_page())),
        ..Engine::product(Abandoned::new(64, Duration::from_secs(3600), |_| {}))
    };
    Service::new(engine, Some("launch-9".into()))
}

fn read(job: u64) -> Request {
    Request::Read {
        job: JobId(job),
        path: PathBuf::from("objects/b3-one"),
        mime: "application/pdf".into(),
        from_page: 1,
        max_text_pages: Some(koru_read::TEXT_PAGE_CAP),
        max_read_pages: Some(koru_read::READ_PAGE_CAP),
        want: Want::Text,
    }
}

/// **The handshake over frames**: the first `Status` is answered with the
/// launch id and the protocol version the Store checks.
#[test]
fn the_first_status_is_answered_with_the_launch() {
    let written = Written::default();
    converse(1, frames(&[Request::Status]), written.clone(), &service());
    let events = written.events();
    let [Event::Status(status)] = events.as_slice() else {
        panic!("one status: {events:?}");
    };
    assert_eq!(status.supervisor_launch_id.as_deref(), Some("launch-9"));
    assert_eq!(status.protocol_version, PROTOCOL_VERSION);
}

/// Anything before the handshake closes the connection, answered with nothing
/// and starting nothing.
#[test]
fn a_connection_that_does_not_open_with_status_is_closed() {
    let written = Written::default();
    let service = service();
    converse(
        1,
        frames(&[read(1), Request::Status]),
        written.clone(),
        &service,
    );
    assert!(written.events().is_empty());
    assert_eq!(service.status().job, None);
    assert_eq!(service.status().pages_read, 0);
}

#[test]
fn a_frame_at_another_version_ends_the_conversation() {
    let mut bytes = Vec::new();
    koru_ipc::frame::write_frame(&mut bytes, FRAME_VERSION + 1, &Request::Status).unwrap();
    let written = Written::default();
    converse(1, std::io::Cursor::new(bytes), written.clone(), &service());
    assert!(written.events().is_empty());
}

/// A read whose connection closes right behind it still ends with exactly
/// one terminal event — finished, or cancelled because nobody is listening.
#[test]
fn every_read_ends_exactly_once_even_when_the_connection_closes() {
    let written = Written::default();
    converse(
        1,
        frames(&[Request::Status, read(5)]),
        written.clone(),
        &service(),
    );
    let events = written.events();
    let terminal: Vec<&Event> = events.iter().filter(|event| event.is_terminal()).collect();
    assert_eq!(terminal.len(), 1, "{events:?}");
    assert_eq!(terminal[0].job(), Some(JobId(5)));
    assert!(
        events.last().unwrap().is_terminal(),
        "nothing after the end"
    );
}
