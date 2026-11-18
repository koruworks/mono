// SPDX-License-Identifier: MPL-2.0

//! The Explore projection.
//!
//! These pin a **contract**, not an implementation: every assertion here is one
//! that must still hold when the answer comes from the store's manifest instead
//! of from the tree. Where the store's own suite pins the same fact, its test
//! name is cited, so the two can be compared rather than merely both passing.

use super::*;
use crate::WorkspaceId;
use crate::node::Node;

fn ws() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap()
}

fn doc(ws: &mut WorkspaceDoc, parent: Parent, title: &str) -> NodeId {
    ws.insert(parent, Node::document(title).unwrap()).unwrap()
}

fn titles(ws: &WorkspaceDoc) -> Vec<String> {
    ws.explore().unwrap().into_iter().map(|e| e.title).collect()
}

// ------------------------------------------------------------------ identity

/// A document gets an id of its own the moment it exists, and keeps it across
/// everything that is not a delete.
#[test]
fn a_document_id_is_minted_once_and_survives_edits() {
    let mut ws = ws();
    let d = doc(&mut ws, Parent::Root, "Roadmap");
    let id = ws.document_id(d);

    ws.set_text(d, "Plan").unwrap();
    assert_eq!(ws.document_id(d), id, "a rename does not remint");

    let other = doc(&mut ws, Parent::Root, "Other");
    ws.move_node(d, Parent::Node(other), 0).unwrap();
    assert_eq!(ws.document_id(d), id, "a move does not remint");

    assert_ne!(ws.document_id(other), id, "two documents differ");
}

/// The id is not the node's id. They are two answers to two questions, and a
/// document has both.
#[test]
fn a_document_id_is_not_its_node_id() {
    let mut ws = ws();
    let d = doc(&mut ws, Parent::Root, "Roadmap");
    assert_ne!(
        ws.document_id(d).as_uuid(),
        d.as_uuid(),
        "a minted document id is its own uuid, not a copy of the node's"
    );
}

/// Relabelling a paragraph into a document gives it an id; relabelling back and
/// forth keeps the first one.
#[test]
fn relabelling_into_a_document_mints_an_id_and_keeps_it() {
    let mut ws = ws();
    let p = ws
        .insert(Parent::Root, Node::paragraph("a line").unwrap())
        .unwrap();
    ws.set_kind(p, Kind::Document).unwrap();
    let id = ws.document_id(p);

    ws.set_kind(p, Kind::Paragraph).unwrap();
    ws.set_kind(p, Kind::Document).unwrap();
    assert_eq!(ws.document_id(p), id, "the id survives the round trip");
}

/// `node_of` is the inverse of `document_id`, and answers `None` rather than
/// panicking for an id from another workspace.
#[test]
fn node_of_finds_the_document_and_declines_a_stranger() {
    let mut ws = ws();
    let d = doc(&mut ws, Parent::Root, "Roadmap");
    assert_eq!(ws.node_of(ws.document_id(d)), Some(d));
    assert_eq!(ws.node_of(DocumentId::new()), None);
}

// ------------------------------------------------------------------ the walk

/// Tree order, with each entry naming its parent — the store's
/// `documents_come_back_in_tree_order_with_their_parents`.
#[test]
fn documents_come_back_in_tree_order_with_their_parents() {
    let mut ws = ws();
    let a = doc(&mut ws, Parent::Root, "A");
    let a1 = doc(&mut ws, Parent::Node(a), "A1");
    doc(&mut ws, Parent::Node(a1), "A1a");
    doc(&mut ws, Parent::Root, "B");

    let out = ws.explore().unwrap();
    assert_eq!(
        out.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["A", "A1", "A1a", "B"],
        "pre-order: a document, then its children, then its next sibling"
    );

    let by = |t: &str| out.iter().find(|e| e.title == t).unwrap().clone();
    assert_eq!(by("A").parent, None);
    assert_eq!(by("A1").parent, Some(by("A").id));
    assert_eq!(by("A1a").parent, Some(by("A1").id));
    assert_eq!(by("B").parent, None);
}

/// Only documents earn an entry. A workspace of prose lists nothing, which is
/// the common case on every keystroke.
#[test]
fn prose_earns_no_entry() {
    let mut ws = ws();
    let d = doc(&mut ws, Parent::Root, "Notes");
    for i in 0..20 {
        ws.insert(
            Parent::Node(d),
            Node::paragraph(format!("line {i}")).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(titles(&ws), ["Notes"], "the body is not the listing");
}

/// **A document under a task belongs to the nearest document above it.**
///
/// The projection recurses *through* anything that is not a document rather
/// than stopping, because the explore pane draws no row for the task — so a
/// document below one must still be reachable, and must not claim to be
/// top-level.
#[test]
fn a_document_under_a_task_hangs_from_the_nearest_document() {
    let mut ws = ws();
    let outer = doc(&mut ws, Parent::Root, "Outer");
    let task = ws
        .insert(Parent::Node(outer), Node::task("a task").unwrap())
        .unwrap();
    let inner = doc(&mut ws, Parent::Node(task), "Inner");

    let out = ws.explore().unwrap();
    assert_eq!(
        out.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["Outer", "Inner"]
    );
    assert_eq!(
        out[1].parent,
        Some(ws.document_id(outer)),
        "the task earns no entry, so Inner hangs from Outer"
    );
    assert_eq!(ws.document_id(inner), out[1].id);
}

/// Ordinals count entries, not tree children — the distinction that exists the
/// moment a document has a body.
#[test]
fn ordinals_count_entries_rather_than_tree_children() {
    let mut ws = ws();
    let parent = doc(&mut ws, Parent::Root, "Parent");
    // A body between the children, so the tree indices and the entry ordinals
    // cannot coincide by accident.
    ws.insert(Parent::Node(parent), Node::paragraph("body").unwrap())
        .unwrap();
    doc(&mut ws, Parent::Node(parent), "First");
    ws.insert(Parent::Node(parent), Node::paragraph("more").unwrap())
        .unwrap();
    doc(&mut ws, Parent::Node(parent), "Second");

    let out = ws.explore().unwrap();
    let kids: Vec<(&str, u32)> = out
        .iter()
        .filter(|e| e.parent == Some(ws.document_id(parent)))
        .map(|e| (e.title.as_str(), e.ordinal))
        .collect();
    assert_eq!(kids, [("First", 0), ("Second", 1)]);
}

/// The trash is not part of the listing, and it does not consume an ordinal
/// either — the one exclusion in `children(Parent::Root)` doing its job here.
#[test]
fn a_trashed_document_leaves_the_listing_and_its_siblings_close_up() {
    let mut ws = ws();
    let a = doc(&mut ws, Parent::Root, "A");
    let b = doc(&mut ws, Parent::Root, "B");
    let c = doc(&mut ws, Parent::Root, "C");
    assert_eq!(titles(&ws), ["A", "B", "C"]);

    ws.trash(b, 0, None).unwrap();

    let out = ws.explore().unwrap();
    assert_eq!(
        out.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["A", "C"],
        "the binned document is gone from Explore"
    );
    assert_eq!(
        out.iter().map(|e| e.ordinal).collect::<Vec<_>>(),
        [0, 1],
        "and the gap closes rather than leaving a hole"
    );

    ws.restore(b, None).unwrap();
    assert!(titles(&ws).contains(&"B".to_owned()), "and it comes back");
    let _ = (a, c);
}

// ------------------------------------------------------------------- moving

/// A move among siblings, addressed in the coordinates Explore shows.
#[test]
fn a_move_reorders_among_siblings() {
    let mut ws = ws();
    doc(&mut ws, Parent::Root, "A");
    let b = doc(&mut ws, Parent::Root, "B");
    doc(&mut ws, Parent::Root, "C");

    ws.move_entry(ws.document_id(b), None, 0).unwrap();
    assert_eq!(titles(&ws), ["B", "A", "C"]);

    ws.move_entry(ws.document_id(b), None, 2).unwrap();
    assert_eq!(titles(&ws), ["A", "C", "B"]);
}

/// **The translation this exists for.** Moving a document under a parent that
/// has a body must land it among that parent's *documents*, not at the tree
/// index of the same number — which would put it in the middle of the prose.
#[test]
fn an_entry_index_is_translated_past_the_body() {
    let mut ws = ws();
    let parent = doc(&mut ws, Parent::Root, "Parent");
    for i in 0..5 {
        ws.insert(
            Parent::Node(parent),
            Node::paragraph(format!("line {i}")).unwrap(),
        )
        .unwrap();
    }
    let first = doc(&mut ws, Parent::Node(parent), "First");
    let loose = doc(&mut ws, Parent::Root, "Loose");

    // Entry index 1 under Parent: after "First", which is tree index 6.
    ws.move_entry(ws.document_id(loose), Some(ws.document_id(parent)), 1)
        .unwrap();

    let out = ws.explore().unwrap();
    let kids: Vec<(&str, u32)> = out
        .iter()
        .filter(|e| e.parent == Some(ws.document_id(parent)))
        .map(|e| (e.title.as_str(), e.ordinal))
        .collect();
    assert_eq!(kids, [("First", 0), ("Loose", 1)]);

    // And the body is still whole and still first.
    let kids = ws.children(Parent::Node(parent)).unwrap();
    let prose = kids
        .iter()
        .take_while(|&&n| ws.kind_of(n) != Some(Kind::Document))
        .count();
    assert_eq!(prose, 5, "the five body lines were not split apart");
    assert_eq!(ws.node_of(ws.document_id(first)), Some(first));
}

/// Dragging a document one place down within its own parent lands it one place
/// down — the off-by-one that appears if the node being moved is still counted
/// while its destination is computed.
#[test]
fn moving_within_one_parent_does_not_land_where_it_started() {
    let mut ws = ws();
    let a = doc(&mut ws, Parent::Root, "A");
    doc(&mut ws, Parent::Root, "B");
    doc(&mut ws, Parent::Root, "C");

    ws.move_entry(ws.document_id(a), None, 1).unwrap();
    assert_eq!(titles(&ws), ["B", "A", "C"]);
}

/// Past the end means the end, rather than an error or a no-op.
#[test]
fn a_move_past_the_last_entry_lands_last() {
    let mut ws = ws();
    let a = doc(&mut ws, Parent::Root, "A");
    doc(&mut ws, Parent::Root, "B");

    ws.move_entry(ws.document_id(a), None, 99).unwrap();
    assert_eq!(titles(&ws), ["B", "A"]);
}

/// **Loro refuses a cycle and leaves the tree intact**, which is why there is no
/// guard of our own. Verified rather than assumed: the store's `move_document`
/// carries an explicit check, and copying it here without knowing this would
/// have been cargo-culting.
#[test]
fn a_document_cannot_be_moved_inside_itself() {
    let mut ws = ws();
    let parent = doc(&mut ws, Parent::Root, "Parent");
    let child = doc(&mut ws, Parent::Node(parent), "Child");
    let before = titles(&ws);

    let pid = ws.document_id(parent);
    assert!(ws.move_entry(pid, Some(ws.document_id(child)), 0).is_err());
    assert!(ws.move_entry(pid, Some(pid), 0).is_err());

    assert_eq!(titles(&ws), before, "and nothing moved");
}

/// A move naming an id this workspace does not have is an error rather than a
/// panic or a silent no-op.
#[test]
fn moving_an_unknown_document_is_refused() {
    let mut ws = ws();
    doc(&mut ws, Parent::Root, "A");
    assert!(matches!(
        ws.move_entry(DocumentId::new(), None, 0),
        Err(crate::StoreError::NoSuchDocument(_))
    ));
    assert!(matches!(
        ws.move_entry(
            ws.document_id(ws.node_of(ws.explore().unwrap()[0].id).unwrap()),
            Some(DocumentId::new()),
            0
        ),
        Err(crate::StoreError::NoSuchDocument(_))
    ));
}

// ------------------------------------------------------------------ the kind

/// The token is the store's, verbatim. It is what a renderer switches on, and
/// what makes a new sort of entry a new string rather than a schema change.
#[test]
fn a_document_lists_as_the_stores_page_token() {
    let mut ws = ws();
    doc(&mut ws, Parent::Root, "A");
    assert_eq!(ws.explore().unwrap()[0].kind, "page");
}

/// A title round-trips whatever language it is in, and the two Unicode
/// spellings of the same word stay distinct — the store's
/// `bidi_and_both_unicode_spellings_survive_byte_for_byte`.
#[test]
fn titles_survive_every_script_byte_for_byte() {
    let mut ws = ws();
    for title in [
        "Roadmap",
        "ロードマップ",
        "路线图",
        "خارطة الطريق",
        "Café",         // NFC
        "Cafe\u{0301}", // NFD — a different string, and it must stay one
    ] {
        doc(&mut ws, Parent::Root, title);
    }
    assert_eq!(
        titles(&ws),
        [
            "Roadmap",
            "ロードマップ",
            "路线图",
            "خارطة الطريق",
            "Café",
            "Cafe\u{0301}"
        ]
    );
}

/// **`listed` is an allowlist** (OCR TECHNICAL §12): a page, a file, a note
/// and a canvas, and nothing else. Every kind token the tree writes into a
/// manifest is on the side it was on when the predicate was a list of what to
/// hide — the table below is that survey, one row per token — and the only
/// tokens that moved are the ones no build of this tree writes: the reading
/// document, which is new, and a kind some future build invents, which is now
/// hidden here rather than shown as a row this build cannot open.
#[test]
fn listed_is_an_allowlist_and_every_kind_in_the_tree_keeps_its_side() {
    // (token, listed before the allowlist, why)
    let tree: [(&str, bool, &str); 10] = [
        (KIND_DOCUMENT, true, "a page"),
        (KIND_FILE, true, "a file"),
        (KIND_NOTE, true, "ADR-277: listed, hidden by presentation"),
        (KIND_CANVAS, true, "Explore gives canvases a section"),
        (KIND_SYSTEM, false, "ADR-176"),
        (KIND_TASK_LIST, false, "SPEC §8, ADR-272's amendment"),
        (
            KIND_RUN,
            false,
            "P1.0 proposal F6: reached through its pill",
        ),
        (KIND_SCHEDULE, false, "P1.0 proposal §2"),
        (KIND_SOCIAL_POST, false, "its own surface"),
        (KIND_SOCIAL_CHANNEL, false, "its own surface"),
    ];
    for (kind, before, why) in tree {
        assert_eq!(listed(kind), before, "{kind} changed side ({why})");
    }
    assert_eq!(LISTED_KINDS.len(), 4, "one row per listed kind above");

    assert!(!listed(KIND_READING), "OCR TECHNICAL D9: never a row");
    assert!(
        !listed("some-future-kind"),
        "an unknown kind is no longer listed: it would be a row nobody can open"
    );
    assert!(
        !listed(""),
        "an entry with no kind is not something to browse"
    );
}

/// **The reading token is declared twice**, as the file and note tokens are:
/// `koru-store-core` registers reading documents (`Store::ensure_reading`)
/// and cannot link this crate. A literal, so a change to either side is a
/// deliberate edit here as well as there.
#[test]
fn the_reading_token_matches_the_stores_duplicate() {
    assert_eq!(KIND_READING, "koru-reading");
}

/// **The run and schedule tokens are declared twice too**, for the same
/// reason as the file and note tokens below: `koru-delegate-document` writes
/// them into the manifest and cannot link this crate. Literal strings, so a
/// change to either side is a deliberate edit here as well as there.
#[test]
fn the_run_and_schedule_tokens_match_the_writers_duplicates() {
    assert_eq!(KIND_RUN, "koru-run");
    assert_eq!(KIND_SCHEDULE, "koru-schedule");
}

/// **These tokens are declared twice, and the two declarations must agree.**
/// `koru-store-core`'s `service.rs` holds its own `KIND_FILE` / `KIND_NOTE`
/// because the store cannot depend on koru-core (ADR-80); drift between the
/// pair would silently break notes — the manifest entry and the projection
/// would stop recognising each other. Literal strings, so a change to either
/// side is a deliberate edit here as well as there.
#[test]
fn the_file_and_note_tokens_match_the_stores_duplicates() {
    assert_eq!(KIND_FILE, "file");
    assert_eq!(KIND_NOTE, "note");
}
