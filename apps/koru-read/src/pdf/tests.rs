// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::accept::{Verdict, judge};
use crate::fixtures;

fn layers(bytes: Vec<u8>) -> Vec<(u32, Layer)> {
    let document = open(bytes).unwrap_or_else(|reason| panic!("did not open: {reason:?}"));
    let mut out = Vec::new();
    document.read_pages(1, document.page_count(), &mut |number, layer| {
        out.push((number, layer));
        true
    });
    out
}

fn texts(bytes: Vec<u8>) -> Vec<String> {
    layers(bytes)
        .into_iter()
        .map(|(_, layer)| layer.text)
        .collect()
}

/// The worker hands a document to page threads, and an abandoned one keeps
/// it; that is only sound if the library's document may cross threads.
#[test]
fn a_document_may_cross_threads() {
    fn shareable<T: Send + Sync>() {}
    shareable::<PdfDocument>();
}

/// **Rendered text, in reading order**, with kerned letters kept together and
/// words that were positioned apart given their space.
#[test]
fn a_text_page_reads_in_order() {
    let read = layers(fixtures::one_page());
    assert_eq!(read.len(), 1);
    let (number, layer) = &read[0];
    assert_eq!(*number, 1);
    assert_eq!(layer.text, fixtures::ONE_PAGE_TEXT);
    assert_eq!(layer.unmapped, 0);
    assert_eq!(judge(layer), Verdict::Text);
}

#[test]
fn two_pages_read_as_two_pages() {
    assert_eq!(
        texts(fixtures::two_pages()),
        ["alpha bravo charlie", "delta echo foxtrot"]
    );
}

/// **A user password is reported, distinctly**, by the library itself.
#[test]
fn a_password_protected_file_is_reported_as_one() {
    assert!(matches!(open(fixtures::encrypted()), Err(Reason::Password)));
}

/// Encryption with an empty user password is a permissions flag, not a lock:
/// any reader opens it, and so do we.
#[test]
fn a_file_encrypted_with_permissions_only_reads() {
    assert_eq!(
        texts(fixtures::permissions_only()),
        ["unlocked words appear here"]
    );
}

#[test]
fn a_damaged_file_is_reported_as_damaged() {
    assert!(matches!(open(fixtures::damaged()), Err(Reason::Damaged)));
    assert!(matches!(open(Vec::new()), Err(Reason::Damaged)));
    assert!(matches!(
        open(b"plain words, no PDF".to_vec()),
        Err(Reason::Damaged)
    ));
}

/// A file that opens with no pages is damaged, not empty (Plan 0.1 met a
/// truncated one that does this).
#[test]
fn a_file_with_no_pages_is_damaged() {
    assert!(matches!(open(fixtures::no_pages()), Err(Reason::Damaged)));
}

/// **Hostile shapes end, and keep their real words.** A page tree that
/// contains itself, and a form that draws itself, are what the library's own
/// README warns about. `hayro` 0.9.0 breaks both cycles: the tree yields its
/// one real page, the form stops recursing, and the words are read.
#[test]
fn hostile_page_trees_and_forms_end_with_their_words() {
    assert_eq!(
        texts(fixtures::page_tree_cycle()),
        ["a page inside a cycle of pages"]
    );
    assert_eq!(
        texts(fixtures::form_recursion()),
        ["words beside a form that draws itself"]
    );
}

/// **What the library does with a file cut short**: it rebuilds what it can
/// from the objects that survived, so the words on the surviving page are
/// still read. Recorded, because the design asked (TECHNICAL §8).
#[test]
fn a_file_cut_short_is_repaired_from_what_survived() {
    let mut bytes = fixtures::one_page();
    let table = bytes
        .windows(4)
        .position(|window| window == b"xref")
        .expect("the fixture has a table");
    bytes.truncate(table);
    let read = texts(bytes);
    assert_eq!(read, [fixtures::ONE_PAGE_TEXT]);
}

#[test]
fn a_scan_has_no_text_layer_and_is_left_for_the_reader() {
    let read = layers(fixtures::scan());
    let (_, layer) = &read[0];
    assert_eq!(layer.chars, 0);
    assert!(layer.picture_share > 0.99, "{}", layer.picture_share);
    assert_eq!(judge(layer), Verdict::NeedsReader);
}

#[test]
fn a_font_that_maps_nothing_is_left_for_the_reader() {
    let read = layers(fixtures::unmapped());
    let (_, layer) = &read[0];
    assert_eq!(layer.text, "");
    assert_eq!(layer.unmapped, 12);
    assert_eq!(judge(layer), Verdict::NeedsReader);
}

#[test]
fn a_blank_page_holds_nothing_and_is_not_a_scan() {
    let read = layers(fixtures::with_blank_page());
    let verdicts: Vec<Verdict> = read.iter().map(|(_, layer)| judge(layer)).collect();
    assert_eq!(verdicts, [Verdict::Text, Verdict::Nothing, Verdict::Text]);
}

/// Reading can start part-way, for a job that resumes, and stops when told.
#[test]
fn pages_can_be_read_from_the_middle_and_stopped() {
    let document = open(fixtures::many_pages(5)).unwrap();
    assert_eq!(document.page_count(), 5);
    let mut seen = Vec::new();
    document.read_pages(3, 5, &mut |number, layer| {
        seen.push((number, layer.text));
        number < 4
    });
    assert_eq!(
        seen,
        [
            (3, "page number 3".to_string()),
            (4, "page number 4".to_string())
        ]
    );
    // Past the end is nothing, not a panic.
    let mut past = 0;
    document.read_pages(6, 9, &mut |_, _| {
        past += 1;
        true
    });
    assert_eq!(past, 0);
}
