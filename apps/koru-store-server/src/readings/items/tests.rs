// SPDX-License-Identifier: MPL-2.0

//! The *could not be read* list agrees with the counts: the same references,
//! the same outcomes, the same attempts policy — and names nothing.

use super::*;
use crate::readings::mirror::AttemptCode;

fn reference(document: DocumentId, blob: &str, mime: Option<&str>) -> ReadableReference {
    ReadableReference {
        document,
        kind: "file".into(),
        blob: blob.into(),
        mime: mime.map(str::to_owned),
        trashed: false,
    }
}

fn outcome(status: &str, reason: Option<&str>) -> Outcome {
    Outcome {
        generation: i64::from(koru_read::GENERATION),
        status: Some(status.into()),
        reason: reason.map(str::to_owned),
        page_count: 1,
        source: None,
    }
}

fn attempt(code: AttemptCode, attempts: u32) -> Attempt {
    Attempt {
        code,
        attempts,
        last_at: 0,
    }
}

/// A password, damage, and a format this computer cannot open are listed with
/// their tokens; a read file, a picture with no words, a capped scan and a
/// file still waiting are not.
#[test]
fn only_what_failed_is_listed_with_its_reason() {
    let ws = WorkspaceId::new();
    let [locked, broken, heic, read, empty, capped, waiting] =
        std::array::from_fn::<DocumentId, 7, _>(|_| DocumentId::new());
    let references = vec![
        reference(locked, "b3:01", Some("application/pdf")),
        reference(broken, "b3:02", Some("application/pdf")),
        reference(heic, "b3:03", Some("image/heic")),
        reference(read, "b3:04", Some("application/pdf")),
        reference(empty, "b3:05", Some("image/png")),
        reference(capped, "b3:06", Some("application/pdf")),
        reference(waiting, "b3:07", Some("image/png")),
    ];
    let outcomes = HashMap::from([
        (
            (locked, "b3:01".to_owned()),
            outcome("failed", Some("password")),
        ),
        (
            (broken, "b3:02".to_owned()),
            outcome("failed", Some("damaged")),
        ),
        ((read, "b3:04".to_owned()), outcome("read", None)),
        ((empty, "b3:05".to_owned()), outcome("empty", None)),
        (
            (capped, "b3:06".to_owned()),
            outcome("partial", Some("page-cap")),
        ),
    ]);
    let attempts = HashMap::from([
        ("b3:03".to_owned(), attempt(AttemptCode::Unsupported, 1)),
        ("b3:07".to_owned(), attempt(AttemptCode::NeedsReader, 1)),
    ]);
    let answer = collect(vec![(ws, references, outcomes)], &attempts, 0);
    let listed: Vec<(DocumentId, &str)> = answer
        .items
        .iter()
        .map(|item| (item.document, item.reason.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            (locked, "password"),
            (broken, "damaged"),
            (heic, "unsupported")
        ]
    );
    assert!(
        answer
            .items
            .iter()
            .all(|item| item.workspace == ws && item.kind == ReaderItemKind::CouldNotBeRead)
    );
    assert!(!answer.truncated);
}

/// **The attempts policy is the coordinator's**: a crash gets one retry, so
/// one crash is not a failure yet and two are; waiting for the reader never
/// is.
#[test]
fn a_local_failure_is_listed_only_once_the_coordinator_gave_up() {
    let ws = WorkspaceId::new();
    let file = DocumentId::new();
    let refs = || vec![reference(file, "b3:aa", Some("application/pdf"))];
    let once = HashMap::from([("b3:aa".to_owned(), attempt(AttemptCode::Crashed, 1))]);
    assert!(
        collect(vec![(ws, refs(), HashMap::new())], &once, 0)
            .items
            .is_empty()
    );
    let twice = HashMap::from([("b3:aa".to_owned(), attempt(AttemptCode::Crashed, 2))]);
    let answer = collect(vec![(ws, refs(), HashMap::new())], &twice, 0);
    assert_eq!(answer.items.len(), 1);
    assert_eq!(answer.items[0].reason, "crashed");
}

/// A binned entry is not read and not listed; a media type the queue would
/// never send is not an item; two pictures of one page with the same bytes
/// are one item.
#[test]
fn trashed_unreadable_and_repeated_references_are_not_items() {
    let ws = WorkspaceId::new();
    let page = DocumentId::new();
    let binned = DocumentId::new();
    let failed = || outcome("failed", Some("damaged"));
    let mut trashed = reference(binned, "b3:bb", Some("application/pdf"));
    trashed.trashed = true;
    let references = vec![
        reference(page, "b3:cc", Some("image/png")),
        reference(page, "b3:cc", Some("image/png")),
        trashed,
        reference(page, "b3:dd", Some("application/zip")),
    ];
    let outcomes = HashMap::from([
        ((page, "b3:cc".to_owned()), failed()),
        ((binned, "b3:bb".to_owned()), failed()),
        ((page, "b3:dd".to_owned()), failed()),
    ]);
    let answer = collect(vec![(ws, references, outcomes)], &HashMap::new(), 0);
    assert_eq!(answer.items.len(), 1);
    assert_eq!(answer.items[0].hash, "b3:cc");
}

/// Past the limit the list stops and says so; the count on the row is still
/// the whole number.
#[test]
fn a_very_long_list_is_cut_and_says_so() {
    let ws = WorkspaceId::new();
    let mut references = Vec::new();
    let mut outcomes = HashMap::new();
    for i in 0..READER_ITEMS_LIMIT + 3 {
        let document = DocumentId::new();
        let hash = format!("b3:{i:04x}");
        references.push(reference(document, &hash, Some("application/pdf")));
        outcomes.insert((document, hash), outcome("failed", Some("password")));
    }
    let answer = collect(vec![(ws, references, outcomes)], &HashMap::new(), 0);
    assert_eq!(answer.items.len(), READER_ITEMS_LIMIT);
    assert!(answer.truncated);
}
