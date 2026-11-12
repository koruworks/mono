// SPDX-License-Identifier: MPL-2.0

//! The *could not be read* and *partly read* lists agree with the counts: the
//! same references, the same outcomes, the same attempts policy — offer what
//! a menu would offer — and name nothing.

use koru_store_wire::readings::{ReadRequestKind, ReadingOffers};

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

fn held(
    workspace: WorkspaceId,
    references: Vec<ReadableReference>,
    outcomes: HashMap<(DocumentId, String), Outcome>,
) -> Held {
    Held {
        workspace,
        references,
        outcomes,
        requests: HashSet::new(),
    }
}

/// Every blob row is the media type the reference names, and every object is
/// here: the place a desktop holding all its files answers.
fn everywhere(_: WorkspaceId, _: &str) -> (Option<String>, bool) {
    (Some("application/pdf".into()), true)
}

fn could_not_be_read(items: &ReaderItems) -> Vec<&ReaderItem> {
    items
        .items
        .iter()
        .filter(|i| i.kind == ReaderItemKind::CouldNotBeRead)
        .collect()
}

/// A password, damage, and a format this computer cannot open are listed with
/// their tokens; a read file, a picture with no words and a file still
/// waiting are not. The capped scan is on the other list.
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
    let answer = collect(
        vec![held(ws, references, outcomes)],
        &attempts,
        0,
        true,
        &mut everywhere,
    );
    let listed: Vec<(DocumentId, &str)> = could_not_be_read(&answer)
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
    let partly: Vec<DocumentId> = answer
        .items
        .iter()
        .filter(|i| i.kind == ReaderItemKind::PartlyRead)
        .map(|i| i.document)
        .collect();
    assert_eq!(partly, [capped]);
    assert!(answer.items.iter().all(|item| item.workspace == ws));
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
        collect(
            vec![held(ws, refs(), HashMap::new())],
            &once,
            0,
            true,
            &mut everywhere
        )
        .items
        .is_empty()
    );
    let twice = HashMap::from([("b3:aa".to_owned(), attempt(AttemptCode::Crashed, 2))]);
    let answer = collect(
        vec![held(ws, refs(), HashMap::new())],
        &twice,
        0,
        true,
        &mut everywhere,
    );
    assert_eq!(answer.items.len(), 1);
    assert_eq!(answer.items[0].reason, "crashed");
    // Failed here, its bytes here: *Read again* beside *Show* (OCR Plan 3.5).
    assert_eq!(
        answer.items[0].offers,
        ReadingOffers {
            read_all: false,
            read_again: true
        }
    );
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
    let answer = collect(
        vec![held(ws, references, outcomes)],
        &HashMap::new(),
        0,
        true,
        &mut everywhere,
    );
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
    let mut asked = 0;
    let answer = collect(
        vec![held(ws, references, outcomes)],
        &HashMap::new(),
        0,
        true,
        &mut |_, _| {
            asked += 1;
            (Some("application/pdf".into()), true)
        },
    );
    assert_eq!(answer.items.len(), READER_ITEMS_LIMIT);
    assert!(answer.truncated);
    assert_eq!(
        asked, READER_ITEMS_LIMIT,
        "the disk is asked for listed items only"
    );
}

// ------------------------------------------------------ partly read (Plan 3.5)

/// **A partly read item is classified as the counts classify it**: settled
/// `partial` with a reason, how far it got as the mirror's page count, and
/// `page-cap` as its token. One still going on its own (`partial`, no reason,
/// nobody asked) is unread and on neither list.
#[test]
fn a_capped_scan_is_partly_read_with_how_far_it_got() {
    let ws = WorkspaceId::new();
    let [capped, going] = std::array::from_fn::<DocumentId, 2, _>(|_| DocumentId::new());
    let mut stopped = outcome("partial", Some("page-cap"));
    stopped.page_count = 200;
    stopped.source = Some("recognized".into());
    let mut still = outcome("partial", None);
    still.page_count = 40;
    let answer = collect(
        vec![held(
            ws,
            vec![
                reference(capped, "b3:e1", Some("application/pdf")),
                reference(going, "b3:e2", Some("application/pdf")),
            ],
            HashMap::from([
                ((capped, "b3:e1".to_owned()), stopped),
                ((going, "b3:e2".to_owned()), still),
            ]),
        )],
        &HashMap::new(),
        0,
        true,
        &mut everywhere,
    );
    assert_eq!(answer.items.len(), 1);
    let item = &answer.items[0];
    assert_eq!(
        (
            item.document,
            item.kind,
            item.reason.as_str(),
            item.pages_done
        ),
        (capped, ReaderItemKind::PartlyRead, "page-cap", Some(200))
    );
    assert!(!item.requested);
}

/// **The list offers exactly what the menu offers** (OCR Plan 3.5): each
/// item's offer is [`Facts::offers`] over the same facts, for every place
/// its bytes can be and whether the reader is ready — and each list carries
/// only its own action.
#[test]
fn the_list_offers_what_the_menu_offers() {
    let ws = WorkspaceId::new();
    let scan = DocumentId::new();
    let mut stopped = outcome("partial", Some("page-cap"));
    stopped.page_count = 200;
    stopped.source = Some("recognized".into());
    for (mime, here, ready) in [
        (Some("application/pdf"), true, true),
        (Some("application/pdf"), false, true),
        (Some("application/pdf"), true, false),
        (None, true, true),
    ] {
        let answer = collect(
            vec![held(
                ws,
                vec![reference(scan, "b3:f1", Some("application/pdf"))],
                HashMap::from([((scan, "b3:f1".to_owned()), stopped.clone())]),
            )],
            &HashMap::new(),
            0,
            ready,
            &mut |_, _| (mime.map(str::to_owned), mime.is_some() && here),
        );
        let menu = Facts {
            status: stopped.status.clone(),
            reason: stopped.reason.clone(),
            source: stopped.source.clone(),
            reader: None,
            mime: mime.map(str::to_owned),
            here: mime.is_some() && here,
            reader_ready: ready,
            pending: false,
            failed_here: false,
        };
        assert_eq!(
            answer.items[0].offers.read_all,
            menu.refusal(ReadRequestKind::ReadAll).is_none(),
            "{mime:?} here={here} ready={ready}"
        );
        assert!(!answer.items[0].offers.read_again, "not this list's action");
    }
    // A password is read again where the menu would: text, its bytes here.
    let locked = DocumentId::new();
    let answer = collect(
        vec![held(
            ws,
            vec![reference(locked, "b3:f2", Some("application/pdf"))],
            HashMap::from([(
                (locked, "b3:f2".to_owned()),
                outcome("failed", Some("password")),
            )]),
        )],
        &HashMap::new(),
        0,
        false,
        &mut everywhere,
    );
    assert_eq!(
        answer.items[0].offers,
        ReadingOffers {
            read_all: false,
            read_again: true
        }
    );
}

/// **A *Read all pages* under way keeps its row**, saying how far it has got
/// and that it was asked for, and offers nothing more; it leaves the list
/// when it ends `read`.
#[test]
fn a_request_under_way_stays_listed_until_it_finishes() {
    let ws = WorkspaceId::new();
    let scan = DocumentId::new();
    let mut going = outcome("partial", None);
    going.page_count = 230;
    going.source = Some("recognized".into());
    let asked = |outcome: Outcome| Held {
        workspace: ws,
        references: vec![reference(scan, "b3:f3", Some("application/pdf"))],
        outcomes: HashMap::from([((scan, "b3:f3".to_owned()), outcome)]),
        requests: HashSet::from(["b3:f3".to_owned()]),
    };
    let answer = collect(
        vec![asked(going)],
        &HashMap::new(),
        0,
        true,
        &mut everywhere,
    );
    assert_eq!(answer.items.len(), 1);
    let item = &answer.items[0];
    assert_eq!(item.kind, ReaderItemKind::PartlyRead);
    assert_eq!(item.pages_done, Some(230));
    assert!(item.requested);
    assert_eq!(item.offers, ReadingOffers::default(), "not offered twice");

    let mut done = outcome("read", None);
    done.page_count = 300;
    assert!(
        collect(vec![asked(done)], &HashMap::new(), 0, true, &mut everywhere)
            .items
            .is_empty(),
        "finished: off the list"
    );
}
