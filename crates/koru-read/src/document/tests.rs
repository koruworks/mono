// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::reading::GENERATION;

/// A blob hash in the store's own form.
const H: &str = "b3:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const H2: &str = "b3:ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

#[test]
fn reading_diagnostics_fresh_read_removes_old_words_but_preserves_newer_generations() {
    let doc = device(1);
    doc.begin(H, &ReaderId::text(), GENERATION).unwrap();
    doc.put_pages(H, GENERATION, &[(1, &text("stale words"))], Some(1))
        .unwrap();
    doc.begin_fresh(H, &ReaderId::text(), GENERATION).unwrap();
    assert!(doc.reading(H).unwrap().pages.is_empty());
    assert_eq!(doc.reading(H).unwrap().pages_done, None);
    doc.begin(H, &ReaderId::text(), GENERATION + 1).unwrap();
    doc.put_pages(H, GENERATION + 1, &[(1, &text("newer words"))], Some(1))
        .unwrap();
    assert!(matches!(
        doc.begin_fresh(H, &ReaderId::text(), GENERATION).unwrap(),
        Write::Superseded { .. }
    ));
    assert_eq!(doc.reading(H).unwrap().pages[&1].as_str(), "newer words");
}

/// A replica with a fixed peer id, so last-writer-wins resolves the same way on
/// every run and a failure reproduces.
fn device(peer: u64) -> ReadingDoc {
    let doc = ReadingDoc::new();
    doc.doc().set_peer_id(peer).unwrap();
    doc
}

fn everything(doc: &ReadingDoc) -> Vec<u8> {
    doc.export_updates_since(&loro::VersionVector::default())
        .unwrap()
}

/// Both directions, as two devices that meet do.
fn sync(a: &ReadingDoc, b: &ReadingDoc) {
    let from_a = everything(a);
    let from_b = everything(b);
    b.import(&from_a).unwrap();
    a.import(&from_b).unwrap();
}

fn text(s: &str) -> PageText {
    PageText::new(s)
}

/// Begin, write `pages` and optionally finish, as one device's whole job.
fn read(doc: &ReadingDoc, generation: u32, pages: &[u32], tag: &str, finish: Option<Outcome>) {
    assert_eq!(
        doc.begin(H, &ReaderId::text(), generation).unwrap(),
        Write::Applied
    );
    let words: Vec<(u32, PageText)> = pages
        .iter()
        .map(|p| (*p, text(&format!("g{generation} p{p} {tag}"))))
        .collect();
    let refs: Vec<(u32, &PageText)> = words.iter().map(|(p, t)| (*p, t)).collect();
    assert_eq!(
        doc.put_pages(H, generation, &refs, None).unwrap(),
        Write::Applied
    );
    if let Some(outcome) = finish {
        assert_eq!(doc.finish(H, generation, &outcome).unwrap(), Write::Applied);
    }
}

fn read_outcome() -> Outcome {
    Outcome {
        status: ReadingStatus::Read,
        reason: None,
        source: Some(Source::TextLayer),
        confidence: None,
        pages_done: None,
    }
}

/// **No mixture of generations, and no page twice**: every page a reader is
/// handed was written at the entry's own generation.
fn assert_one_generation(doc: &ReadingDoc) -> Reading {
    let reading = doc.reading(H).expect("an entry");
    let prefix = format!("g{} ", reading.generation);
    for (page, words) in &reading.pages {
        assert!(
            words.as_str().starts_with(&prefix),
            "page {page} of a generation-{} reading came from another generation",
            reading.generation
        );
        assert!(
            words.as_str().starts_with(&format!("{prefix}p{page} ")),
            "page {page} holds another page's words"
        );
    }
    reading
}

/// The raw pages map of one generation, for looking past what a reader sees.
fn raw_pages(doc: &ReadingDoc, generation: u32) -> Vec<String> {
    let entry = child_map(&doc.doc().get_map(ROOT), H).expect("entry");
    match child_map(&entry, &pages_key(generation)) {
        Some(map) => {
            let mut keys: Vec<String> = map.keys().map(|k| k.to_string()).collect();
            keys.sort();
            keys
        }
        None => Vec::new(),
    }
}

/// Every order of applying `updates` to a fresh replica.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for rest in permutations(n - 1) {
        for at in 0..=rest.len() {
            let mut order = rest.clone();
            order.insert(at, n - 1);
            out.push(order);
        }
    }
    out
}

fn converge_in_every_order(updates: &[Vec<u8>]) -> ReadingDoc {
    let mut first: Option<(loro::LoroValue, ReadingDoc)> = None;
    for order in permutations(updates.len()) {
        let doc = device(1_000);
        for i in &order {
            doc.import(&updates[*i]).unwrap();
        }
        let state = doc.doc().get_deep_value();
        match &first {
            None => first = Some((state, doc)),
            Some((expected, _)) => {
                assert_eq!(&state, expected, "the order {order:?} converged elsewhere")
            }
        }
    }
    first.expect("at least one order").1
}

// ------------------------------------------------------------ convergence

/// **Two devices that each create the entry for one blob before they sync
/// hold the same container**, and both writes survive. With an ordinary child
/// container one device's whole entry would lose to the other's.
#[test]
fn two_devices_that_create_one_entry_apart_merge_into_it() {
    let a = device(1);
    let b = device(2);
    read(&a, 1, &[1], "A", None);
    read(&b, 1, &[2], "B", None);

    sync(&a, &b);

    assert_eq!(a.doc().get_deep_value(), b.doc().get_deep_value());
    assert_eq!(a.hashes(), vec![H.to_string()]);
    let reading = assert_one_generation(&a);
    assert_eq!(
        reading.pages.keys().copied().collect::<Vec<_>>(),
        vec![1, 2],
        "one device's pages were lost"
    );
}

/// **The same page written twice is one value**, whichever device wrote it
/// and whatever it said.
#[test]
fn the_same_page_from_two_devices_is_held_once() {
    let a = device(1);
    let b = device(2);
    read(&a, 1, &[1, 2, 3], "A", Some(read_outcome()));
    read(&b, 1, &[2, 3, 4], "B", None);

    sync(&a, &b);

    assert_eq!(a.doc().get_deep_value(), b.doc().get_deep_value());
    let reading = assert_one_generation(&a);
    assert_eq!(
        reading.pages.keys().copied().collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(raw_pages(&a, 1), vec!["1", "2", "3", "4"]);
}

/// **Every order, one generation and two.** Three devices read one file while
/// apart — two at generation 1, one at generation 2 — and their changes reach
/// a fourth replica in each of the six possible orders. All six hold the same
/// state, and the reading they show takes its pages from one generation.
#[test]
fn three_devices_at_two_generations_converge_in_every_order() {
    let a = device(1);
    let b = device(2);
    let c = device(3);
    read(&a, 1, &[1, 2, 3], "A", Some(read_outcome()));
    read(&b, 1, &[2, 3, 4], "B", None);
    read(&c, 2, &[1, 2], "C", Some(read_outcome()));

    let merged = converge_in_every_order(&[everything(&a), everything(&b), everything(&c)]);

    let reading = assert_one_generation(&merged);
    let expected: Vec<u32> = if reading.generation == 1 {
        vec![1, 2, 3, 4]
    } else {
        vec![1, 2]
    };
    assert_eq!(reading.pages.keys().copied().collect::<Vec<_>>(), expected);
}

/// **The device that raises the generation empties the older map**, and a
/// straggler still writing the old generation afterwards is inert: it lands in
/// a map nobody reads.
#[test]
fn raising_the_generation_empties_the_older_pages_and_a_straggler_is_inert() {
    let a = device(1);
    let c = device(3);
    read(&a, 1, &[1, 2, 3], "A", None);
    sync(&a, &c);

    // `c` has seen generation 1 and raises it.
    read(&c, 2, &[1], "C", Some(read_outcome()));
    assert!(raw_pages(&c, 1).is_empty(), "the older map was not emptied");

    // `a` has not heard, and goes on writing generation 1.
    let late = text("g1 p4 A");
    assert_eq!(
        a.put_pages(H, 1, &[(4, &late)], None).unwrap(),
        Write::Applied
    );

    let merged = converge_in_every_order(&[everything(&a), everything(&c)]);
    let reading = assert_one_generation(&merged);
    assert_eq!(reading.generation, 2);
    assert_eq!(reading.pages.keys().copied().collect::<Vec<_>>(), vec![1]);
    assert_eq!(
        raw_pages(&merged, 1),
        vec!["4"],
        "the straggler's page should sit, inert, in the old map"
    );

    // And once `a` hears, it refuses to write the old generation again.
    a.import(&everything(&c)).unwrap();
    assert_eq!(
        a.put_pages(H, 1, &[(5, &late)], None).unwrap(),
        Write::Superseded { generation: 2 }
    );
}

// ------------------------------------------------------------ generations

/// A writer behind the entry's generation writes nothing, at any step.
#[test]
fn a_writer_below_the_entrys_generation_is_superseded() {
    let doc = device(1);
    read(&doc, 2, &[1], "new", Some(read_outcome()));
    let before = doc.doc().get_deep_value();

    let superseded = Write::Superseded { generation: 2 };
    assert_eq!(doc.begin(H, &ReaderId::text(), 1).unwrap(), superseded);
    let page = text("g1 p1 old");
    assert_eq!(
        doc.put_pages(H, 1, &[(1, &page)], None).unwrap(),
        superseded
    );
    assert_eq!(doc.finish(H, 1, &read_outcome()).unwrap(), superseded);
    let old = Reading {
        reader: Some(ReaderId::text()),
        generation: 1,
        status: Some(ReadingStatus::Read),
        ..Reading::default()
    };
    assert_eq!(doc.write(H, &old).unwrap(), superseded);

    assert_eq!(
        doc.doc().get_deep_value(),
        before,
        "a superseded write wrote"
    );
}

/// *Read again* at the same generation keeps the pages, which are about to be
/// written again under the same keys, and resets the status.
#[test]
fn reading_again_at_the_same_generation_resets_the_status_and_keeps_the_pages() {
    let doc = device(1);
    read(&doc, 1, &[1, 2], "first", None);
    doc.finish(
        H,
        1,
        &Outcome {
            status: ReadingStatus::Partial,
            reason: Some(Reason::PageCap),
            source: Some(Source::TextLayer),
            confidence: None,
            pages_done: Some(2),
        },
    )
    .unwrap();

    doc.begin(H, &ReaderId::text(), 1).unwrap();

    let reading = doc.reading(H).unwrap();
    assert_eq!(reading.status, Some(ReadingStatus::Partial));
    assert_eq!(
        reading.reason, None,
        "the cap is a finished state; begin lifts it"
    );
    assert_eq!(
        reading.pages_done,
        Some(2),
        "a resumed reading knows where it was"
    );
    assert_eq!(reading.pages.len(), 2);
    assert!(!reading.is_finished());
}

/// A raised generation starts clean: no pages, no progress, no source carried
/// over from the reading it replaces.
#[test]
fn a_raised_generation_starts_from_nothing() {
    let doc = device(1);
    read(&doc, 1, &[1, 2], "old", None);
    doc.finish(
        H,
        1,
        &Outcome {
            status: ReadingStatus::Partial,
            reason: Some(Reason::PageCap),
            source: Some(Source::Recognized),
            confidence: Some(80),
            pages_done: Some(2),
        },
    )
    .unwrap();

    doc.begin(H, &ReaderId::text(), 2).unwrap();

    let reading = doc.reading(H).unwrap();
    assert_eq!(reading.generation, 2);
    assert!(reading.pages.is_empty());
    assert_eq!(
        (reading.pages_done, reading.confidence, reading.source),
        (None, None, None)
    );
    assert_eq!(doc.generation(H), Some(2));
}

/// An entry a newer build wrote with a status this build has never heard of
/// still says its generation, so this build still knows not to write over it.
#[test]
fn an_entry_with_an_unknown_status_still_guards_its_generation() {
    let doc = device(1);
    read(&doc, GENERATION + 1, &[1], "newer", None);
    let entry = child_map(&doc.doc().get_map(ROOT), H).unwrap();
    entry.insert(key::STATUS, "skipped").unwrap();
    doc.doc().commit();

    let reading = doc.reading(H).unwrap();
    assert_eq!(reading.status, None);
    assert!(reading.is_settled_for_this_build());
    assert_eq!(
        doc.begin(H, &ReaderId::text(), GENERATION).unwrap(),
        Write::Superseded {
            generation: GENERATION + 1
        }
    );
}

// ------------------------------------------------------------ writes

/// Each `None` in an outcome removes its key, so an entry says exactly what
/// its last finish said.
#[test]
fn a_finish_says_exactly_what_it_was_given() {
    let doc = device(1);
    read(&doc, 1, &[1], "x", None);
    doc.finish(
        H,
        1,
        &Outcome {
            status: ReadingStatus::Partial,
            reason: Some(Reason::PageCap),
            source: Some(Source::Recognized),
            confidence: Some(72),
            pages_done: Some(200),
        },
    )
    .unwrap();
    let capped = doc.reading(H).unwrap();
    assert_eq!(
        (capped.reason, capped.confidence, capped.pages_done),
        (Some(Reason::PageCap), Some(72), Some(200))
    );

    doc.finish(H, 1, &read_outcome()).unwrap();
    let read = doc.reading(H).unwrap();
    assert_eq!(read.status, Some(ReadingStatus::Read));
    assert_eq!(
        (read.reason, read.confidence, read.pages_done),
        (None, None, None)
    );
    assert_eq!(read.source, Some(Source::TextLayer));
}

#[test]
fn a_finish_refuses_what_the_design_does_not_pair() {
    let doc = device(1);
    read(&doc, 1, &[1], "x", None);
    let with = |status, reason, confidence| Outcome {
        status,
        reason,
        source: None,
        confidence,
        pages_done: None,
    };
    for outcome in [
        with(ReadingStatus::Failed, None, None),
        with(ReadingStatus::Read, Some(Reason::PageCap), None),
        with(ReadingStatus::Partial, Some(Reason::Password), None),
        with(ReadingStatus::Empty, Some(Reason::Damaged), None),
    ] {
        assert!(
            matches!(doc.finish(H, 1, &outcome), Err(Error::Inconsistent)),
            "{outcome:?} was accepted"
        );
    }
    assert!(matches!(
        doc.finish(H, 1, &with(ReadingStatus::Read, None, Some(101))),
        Err(Error::InvalidConfidence)
    ));
    assert!(
        doc.finish(
            H,
            1,
            &with(ReadingStatus::Failed, Some(Reason::Password), None)
        )
        .is_ok()
    );
}

#[test]
fn pages_and_outcomes_need_a_begun_reading() {
    let doc = device(1);
    let page = text("p");
    assert!(matches!(
        doc.put_pages(H, 1, &[(1, &page)], None),
        Err(Error::NotBegun)
    ));
    assert!(matches!(
        doc.finish(H, 1, &read_outcome()),
        Err(Error::NotBegun)
    ));
    doc.begin(H, &ReaderId::text(), 1).unwrap();
    assert!(
        matches!(
            doc.put_pages(H, 2, &[(1, &page)], None),
            Err(Error::NotBegun)
        ),
        "a writer ahead of the entry never began at its own generation"
    );
    assert!(matches!(
        doc.put_pages(H, 1, &[(0, &page)], None),
        Err(Error::InvalidPage)
    ));
}

/// An empty page is not a key: the index emits nothing for one, and `""`
/// would be a page that says nothing on every device.
#[test]
fn an_empty_page_is_not_written() {
    let doc = device(1);
    doc.begin(H, &ReaderId::text(), 1).unwrap();
    let empty = text("");
    let words = text("g1 p2 x");
    doc.put_pages(H, 1, &[(1, &empty), (2, &words)], Some(2))
        .unwrap();
    assert_eq!(raw_pages(&doc, 1), vec!["2"]);
    assert_eq!(doc.reading(H).unwrap().pages_done, Some(2));
}

/// **The only root is `readings`**, and an entry is keyed by the hash.
#[test]
fn the_document_has_one_root_and_entries_keyed_by_hash() {
    let doc = device(1);
    read(&doc, 1, &[1], "x", Some(read_outcome()));
    let copy = doc.reading(H).unwrap();
    doc.write(H2, &copy).unwrap();

    let loro::LoroValue::Map(root) = doc.doc().get_deep_value() else {
        panic!("a document's value is a map");
    };
    assert_eq!(root.keys().collect::<Vec<_>>(), vec![ROOT]);
    assert_eq!(doc.hashes(), vec![H.to_string(), H2.to_string()]);
    assert_eq!(doc.readings().len(), 2);
    assert_eq!(pages_key(3), "pages/3");
}

/// **Copying a reading by hash** writes the same reading, page for page.
#[test]
fn a_whole_reading_written_at_once_reads_back_as_itself() {
    let doc = device(1);
    let reading = Reading {
        reader: Some(ReaderId::parse("ocr-x").unwrap()),
        corrections: Default::default(),
        generation: 1,
        status: Some(ReadingStatus::Partial),
        reason: Some(Reason::PageCap),
        source: Some(Source::Mixed),
        confidence: Some(64),
        pages_done: Some(200),
        pages: [(1, text("one")), (3, text("three"))].into_iter().collect(),
    };
    assert_eq!(doc.write(H, &reading).unwrap(), Write::Applied);
    assert_eq!(doc.reading(H).unwrap(), reading);

    let incomplete = Reading {
        status: None,
        ..reading.clone()
    };
    assert!(matches!(
        doc.write(H2, &incomplete),
        Err(Error::Inconsistent)
    ));
}

/// **An entry round-trips the eight languages byte for byte**: through the
/// document, through a snapshot, and through an update to another replica.
/// Mixed direction, explicit bidi controls, composed and decomposed forms and
/// half-width kana included — the text is never normalised.
#[test]
fn an_entry_round_trips_all_eight_languages_byte_exactly() {
    let strings = koru_store_core::fixtures::all_strings();
    assert!(strings.len() >= 16, "the fixture lost a language");
    let mut pages: BTreeMap<u32, PageText> = strings
        .iter()
        .enumerate()
        .map(|(i, s)| (i as u32 + 1, text(s)))
        .collect();
    // And all of them as one page's lines, which is how a page is stored.
    pages.insert(strings.len() as u32 + 1, text(&strings.join("\n")));
    let reading = Reading {
        reader: Some(ReaderId::text()),
        corrections: Default::default(),
        generation: GENERATION,
        status: Some(ReadingStatus::Read),
        reason: None,
        source: Some(Source::Plain),
        confidence: None,
        pages_done: None,
        pages,
    };

    let doc = device(1);
    doc.write(H, &reading).unwrap();
    let from_snapshot = ReadingDoc::from_bytes(&doc.export_snapshot().unwrap()).unwrap();
    let other = device(2);
    other.import(&everything(&doc)).unwrap();

    for replica in [&doc, &from_snapshot, &other] {
        let back = replica.reading(H).unwrap();
        assert_eq!(back, reading);
        for (page, words) in &reading.pages {
            assert_eq!(
                back.pages[page].as_str().as_bytes(),
                words.as_str().as_bytes(),
                "page {page} changed bytes"
            );
        }
    }
}

// ------------------------------------------------------------ hashes

/// **A key is a hash the store would mint, and nothing else.** Held against
/// the real `BlobHash` rather than a second opinion: a string is accepted here
/// exactly when the store parses it to itself.
#[test]
fn a_blob_key_is_exactly_what_the_store_mints() {
    use koru_store_core::model::BlobHash;
    let minted = BlobHash::from_digest("b3", &"AB".repeat(32)).unwrap();
    assert!(is_blob_hash(minted.as_str()), "{minted}");

    for candidate in [
        H,
        "b3:abc",
        "sha256:0f",
        "b3:ABCDEF",
        "b3:",
        ":abcd",
        "b3abcd",
        "b3:ab cd",
        "b-3:abcd",
        "b3:abcg",
        "",
    ] {
        let store_says = BlobHash::parse(candidate).is_ok_and(|h| h.as_str() == candidate);
        assert_eq!(
            is_blob_hash(candidate),
            store_says,
            "{candidate:?}: this crate and the store disagree"
        );
    }

    let doc = device(1);
    assert!(matches!(
        doc.begin("b3:ABCDEF", &ReaderId::text(), 1),
        Err(Error::InvalidHash)
    ));
    assert!(matches!(
        doc.begin("not a hash", &ReaderId::text(), 1),
        Err(Error::InvalidHash)
    ));
    assert!(doc.hashes().is_empty(), "a refused key was written");
}

/// A forgotten entry is gone on every replica, its neighbour stays, and the
/// same bytes read again later are whole again.
#[test]
fn a_forgotten_reading_is_gone_everywhere_and_a_later_reading_is_whole() {
    let a = device(1);
    read(&a, GENERATION, &[1], "kept", Some(read_outcome()));
    a.write(H2, &a.reading(H).unwrap()).unwrap();
    let b = device(2);
    sync(&a, &b);

    assert!(a.forget(H).unwrap());
    assert!(!a.forget(H).unwrap(), "a second forget finds nothing");
    sync(&a, &b);
    for doc in [&a, &b] {
        assert_eq!(doc.hashes(), vec![H2.to_owned()]);
        assert!(doc.reading(H).is_none());
    }

    read(&b, GENERATION, &[1], "again", Some(read_outcome()));
    sync(&a, &b);
    assert_eq!(
        a.reading(H).unwrap().pages[&1].as_str(),
        format!("g{GENERATION} p1 again")
    );
}
