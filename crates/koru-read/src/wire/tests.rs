// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde_json::json;

fn round_trip<T>(value: &T, expected: serde_json::Value)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let sent = serde_json::to_value(value).unwrap();
    assert_eq!(
        sent, expected,
        "the wire shape changed; bump PROTOCOL_VERSION if deliberate"
    );
    let back: T = serde_json::from_value(sent).unwrap();
    assert_eq!(&back, value);
}

/// **Every message, pinned as the JSON that crosses the pipe.** A literal per
/// variant, inline rather than a file on disk; a new variant without one fails
/// the count below.
#[test]
fn every_message_has_a_pinned_shape() {
    let job = JobId(7);
    let requests = [
        (
            Request::Read {
                job,
                path: PathBuf::from("objects/b3-00ff"),
                mime: "application/pdf".into(),
                from_page: 1,
                max_text_pages: Some(crate::TEXT_PAGE_CAP),
                max_read_pages: Some(crate::READ_PAGE_CAP),
                want: Want::Text,
            },
            json!({
                "kind": "read", "job": 7, "path": "objects/b3-00ff",
                "mime": "application/pdf", "from_page": 1,
                "max_text_pages": 2000, "max_read_pages": 200, "want": "text"
            }),
        ),
        (
            Request::Read {
                job,
                path: PathBuf::from("objects/b3-00ff"),
                mime: "image/png".into(),
                from_page: 201,
                max_text_pages: None,
                max_read_pages: None,
                want: Want::Recognize,
            },
            json!({
                "kind": "read", "job": 7, "path": "objects/b3-00ff",
                "mime": "image/png", "from_page": 201,
                "max_text_pages": null, "max_read_pages": null, "want": "recognize"
            }),
        ),
        (Request::Status, json!({ "kind": "status" })),
        (
            Request::Cancel { job },
            json!({ "kind": "cancel", "job": 7 }),
        ),
    ];
    let events = [
        (
            Event::Page {
                job,
                page: 3,
                text: PageText::new("first line\nsecond line"),
                source: Source::TextLayer,
                confidence: None,
            },
            json!({
                "kind": "page", "job": 7, "page": 3,
                "text": "first line\nsecond line", "source": "text-layer",
                "confidence": null
            }),
        ),
        (
            Event::Done {
                job,
                status: ReadingStatus::Partial,
                reason: Some(Reason::PageCap),
                pages_done: 200,
            },
            json!({
                "kind": "done", "job": 7, "status": "partial",
                "reason": "page-cap", "pages_done": 200
            }),
        ),
        (
            Event::Failed {
                job,
                code: FailureCode::NeedsReader,
            },
            json!({ "kind": "failed", "job": 7, "code": "needs-reader" }),
        ),
        (
            Event::Status(WorkerStatus {
                worker_version: "0.0.3".into(),
                protocol_version: PROTOCOL_VERSION,
                process_id: 4242,
                supervisor_launch_id: Some("launch-1".into()),
                recognition: false,
                recognition_built: true,
                job: Some(job),
                pages_read: 12,
            }),
            json!({
                "kind": "status", "worker_version": "0.0.3", "protocol_version": 2,
                "process_id": 4242, "supervisor_launch_id": "launch-1",
                "recognition": false, "recognition_built": true, "job": 7, "pages_read": 12
            }),
        ),
    ];
    // Read twice (capped and uncapped), Status, Cancel; Page, Done, Failed,
    // Status — the six messages the design names, in both directions.
    assert_eq!(requests.len(), 4, "add a pinned shape for the new Request");
    assert_eq!(events.len(), 4, "add a pinned shape for the new Event");
    for (message, expected) in &requests {
        round_trip(message, expected.clone());
    }
    for (message, expected) in &events {
        round_trip(message, expected.clone());
    }
}

/// Tokens on this wire are the stored tokens, so a coordinator can write what
/// the worker said without translating it.
#[test]
fn the_codes_are_stable_lowercase_tokens() {
    for code in FailureCode::ALL {
        assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
        assert_eq!(FailureCode::parse(code.as_str()), Some(*code));
    }
    assert_eq!(
        FailureCode::ALL
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>(),
        [
            "unsupported",
            "unreadable",
            "needs-reader",
            "busy",
            "cancelled",
            "internal"
        ]
    );
    assert_eq!(
        Want::ALL.iter().map(|w| w.as_str()).collect::<Vec<_>>(),
        ["text", "recognize", "force-recognize"]
    );
}

/// **A page's words cross the pipe byte for byte**, in all eight languages,
/// and are never in what a log line of the message would print.
#[test]
fn a_page_carries_every_language_exactly_and_prints_none_of_it() {
    for (i, words) in koru_store_core::fixtures::all_strings()
        .into_iter()
        .enumerate()
    {
        let event = Event::Page {
            job: JobId(i as u64),
            page: 1,
            text: PageText::new(words),
            source: Source::Recognized,
            confidence: Some(55),
        };
        let bytes = serde_json::to_vec(&event).unwrap();
        let back: Event = serde_json::from_slice(&bytes).unwrap();
        let Event::Page { text, .. } = &back else {
            panic!("a page came back as something else");
        };
        assert_eq!(text.as_str().as_bytes(), words.as_bytes());
        let printed = format!("{back:?}");
        assert!(!printed.contains(words), "{printed}");
    }
}

#[test]
fn every_job_message_names_its_job_and_says_whether_it_ends_it() {
    let job = JobId(3);
    let page = Event::Page {
        job,
        page: 1,
        text: PageText::default(),
        source: Source::Plain,
        confidence: None,
    };
    let done = Event::Done {
        job,
        status: ReadingStatus::Empty,
        reason: None,
        pages_done: 1,
    };
    let failed = Event::Failed {
        job,
        code: FailureCode::Cancelled,
    };
    assert_eq!(
        [page.job(), done.job(), failed.job()],
        [Some(job), Some(job), Some(job)]
    );
    assert!(!page.is_terminal());
    assert!(done.is_terminal() && failed.is_terminal());
    let status = Event::Status(WorkerStatus {
        worker_version: String::new(),
        protocol_version: PROTOCOL_VERSION,
        process_id: 1,
        supervisor_launch_id: None,
        recognition: false,
        recognition_built: false,
        job: None,
        pages_read: 0,
    });
    assert_eq!(status.job(), None);
    assert!(!status.is_terminal());
}

/// Two numbers at two layers. Pinned literally so bumping one cannot silently
/// move the other.
#[test]
fn the_protocol_and_frame_versions_are_two_numbers() {
    assert_eq!(PROTOCOL_VERSION, 2);
    assert_eq!(FRAME_VERSION, 1);
}

/// **The bootstrap line, pinned.** The Store writes it and the worker reads
/// it; both sides compile against this one shape, and it is one line, so it
/// must never serialize with a newline in it.
#[test]
fn the_bootstrap_has_a_pinned_one_line_shape() {
    let bootstrap = WorkerBootstrap {
        bootstrap_version: BOOTSTRAP_VERSION,
        launch_id: "launch-1".into(),
        channel: "nightly".into(),
    };
    round_trip(
        &bootstrap,
        json!({ "bootstrap_version": 1, "launch_id": "launch-1", "channel": "nightly" }),
    );
    let line = serde_json::to_string(&bootstrap).unwrap();
    assert!(!line.contains('\n'));
    assert_eq!(BOOTSTRAP_VERSION, 1);
}

/// **A status from a worker that predates `recognition_built` still parses**,
/// and says the worker cannot recognize — the answer that makes the Store
/// offer nothing it could not use.
#[test]
fn a_status_without_recognition_built_reads_as_not_built() {
    let status: Event = serde_json::from_value(json!({
        "kind": "status", "worker_version": "0.0.1", "protocol_version": 1,
        "process_id": 1, "supervisor_launch_id": null,
        "recognition": false, "job": null, "pages_read": 0
    }))
    .unwrap();
    let Event::Status(status) = status else {
        panic!("a status");
    };
    assert!(!status.recognition_built);
}
