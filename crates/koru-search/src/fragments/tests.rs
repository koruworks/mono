// SPDX-License-Identifier: MPL-2.0

//! What a person can search for, decided here and nowhere else.

use super::*;
use koru_projection::model::{DocumentProjection, ListedDocument, ProjectedAttr, ProjectedNode};
use koru_store_core::ids::{DocumentId, WorkspaceId};

use crate::model::{AttrFilter, AttrOp, SearchQuery};
use crate::storage::SearchIndex;

fn node(id: &str, kind: &str, text: &str) -> ProjectedNode {
    ProjectedNode {
        id: id.to_owned(),
        parent: None,
        ordinal: 0,
        depth: 0,
        kind: kind.to_owned(),
        text: text.to_owned(),
        heading: None,
        list_style: None,
        done: false,
        level_name: None,
        name: None,
        icon: None,
        colour: None,
        language: None,
        blob: None,
        mime: None,
        embed: None,
        // The projection carries the document's own bin now, flagged. The
        // indexer never meets one: `Projections::read` hands over the live
        // document, which is why nothing in this file has to know about it.
        in_trash: false,
        runs: Vec::new(),
        attrs: Vec::new(),
    }
}

fn projection(nodes: Vec<ProjectedNode>) -> DocumentProjection {
    DocumentProjection {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        source_revision: b"r".to_vec(),
        nodes,
    }
}

fn texts(fragments: &[Fragment]) -> Vec<(&str, &str)> {
    fragments
        .iter()
        .map(|f| (f.field.as_str(), f.text.as_str()))
        .collect()
}

/// **A document with no content at all is still findable**, because its title
/// is written on the listing path rather than by the walk. That is also why the
/// walk itself contributes nothing here.
#[test]
fn a_document_with_no_nodes_contributes_nothing_and_is_titled_elsewhere() {
    assert!(fragments(&projection(Vec::new()), "Roadmap", None).is_empty());
    let title = title_fragment("w", "d", "Roadmap").expect("a title is matchable");
    assert_eq!(title.field, field::TITLE);
    assert_eq!(title.text, "Roadmap");
    assert_eq!(title.seq, 0, "a preview starts after the title");
    assert!(title_fragment("w", "d", "  ").is_none());
}

#[test]
fn a_trashed_task_node_has_no_search_fragment() {
    let mut task = node("task", "paragraph", "Ship café");
    task.in_trash = true;
    let projected = projection(vec![task]);
    assert!(fragments(&projected, "Roadmap", None).is_empty());
    assert!(attr_values("workspace", "document", &projected).is_empty());
}

/// The listing and the root node normally hold the same string. Indexing both
/// would double every title hit.
#[test]
fn the_root_node_does_not_repeat_the_listed_title() {
    let fragments = fragments(
        &projection(vec![
            node("root", "document", "Roadmap"),
            node("p1", "paragraph", "the first line"),
        ]),
        "Roadmap",
        None,
    );
    assert_eq!(texts(&fragments), [(field::BODY, "the first line")]);
}

/// When they have drifted apart, both are things somebody typed and both are
/// things somebody might search for.
#[test]
fn a_root_node_that_disagrees_with_the_listing_is_matchable_too() {
    let fragments = fragments(
        &projection(vec![node("root", "document", "Roadmap 2026")]),
        "Roadmap",
        None,
    );
    assert_eq!(texts(&fragments), [(field::TITLE, "Roadmap 2026")]);
}

/// **No `title_case(kind)` fallback.** The old builder gave an empty paragraph
/// the text "Paragraph", which is why searching for *table* used to match every
/// empty table in the workspace.
#[test]
fn an_empty_node_produces_nothing_rather_than_the_name_of_its_kind() {
    let fragments = fragments(
        &projection(vec![
            node("p1", "paragraph", "   "),
            node("t1", "table", ""),
            node("d1", "divider", ""),
        ]),
        "Roadmap",
        None,
    );
    assert!(texts(&fragments).is_empty());
}

/// A picture has no text. Its name is what somebody looks for.
#[test]
fn content_with_no_text_is_found_by_its_name() {
    let mut image = node("i1", "image", "");
    image.name = Some("The whiteboard".into());
    let fragments = fragments(&projection(vec![image]), "Roadmap", None);
    assert_eq!(texts(&fragments), [(field::NAME, "The whiteboard")]);
}

#[test]
fn a_heading_carries_its_level_as_a_token() {
    let mut heading = node("h1", "paragraph", "Decisions");
    heading.heading = Some(2);
    let fragments = fragments(&projection(vec![heading]), "Roadmap", None);
    assert_eq!(fragments[0].field, "heading-2");
}

#[test]
fn a_level_name_is_its_own_fragment() {
    let mut task = node("t1", "task", "Ship it");
    task.level_name = Some("Milestone".into());
    let fragments = fragments(&projection(vec![task]), "Roadmap", None);
    assert_eq!(
        texts(&fragments),
        [(field::TASK, "Ship it"), (field::LEVEL_NAME, "Milestone")]
    );
}

/// **A stable node id is routing data, not something a person read.** A
/// labelled reference stays searchable by the text they saw; an unlabelled one
/// would only ever match a uuid nobody typed.
#[test]
fn an_unlabelled_reference_is_not_searchable_text() {
    let mut task = node("t1", "task", "Ship it");
    task.attrs = vec![
        ProjectedAttr {
            id: "a1".into(),
            role: "blocks".into(),
            kind: "ref".into(),
            value: "0191d0f2-0000-7000-8000-000000000000".into(),
            zone: None,
            label: None,
        },
        ProjectedAttr {
            id: "a2".into(),
            role: "due".into(),
            kind: "date".into(),
            value: "2026-03-01".into(),
            zone: None,
            label: None,
        },
        ProjectedAttr {
            id: "a3".into(),
            role: "owner".into(),
            kind: "ref".into(),
            value: "0191d0f2-0000-7000-8000-000000000001".into(),
            zone: None,
            label: Some("Steven".into()),
        },
    ];
    let fragments = fragments(&projection(vec![task]), "Roadmap", None);
    assert_eq!(
        texts(&fragments),
        [
            (field::TASK, "Ship it"),
            ("attr:due", "2026-03-01"),
            ("attr:owner", "Steven"),
        ]
    );
}

// ------------------------------------------------------------------- tags

const TAG_A: &str = "0191d0f2-0000-7000-8000-00000000000a";
const TAG_B: &str = "0191d0f2-0000-7000-8000-00000000000b";

fn tagged(tags: &[&str]) -> ProjectedNode {
    let mut node = node("t1", "task", "Hello");
    node.attrs = tags
        .iter()
        .enumerate()
        .map(|(i, tag)| ProjectedAttr {
            id: format!("a{i}"),
            role: TAG_ROLE.into(),
            kind: "text".into(),
            value: (*tag).to_owned(),
            zone: None,
            label: None,
        })
        .collect();
    node
}

fn table(pairs: &[(&str, &str)]) -> TagTable {
    TagTable::from_pairs(
        pairs
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned())),
    )
}

/// **The decision.** A node stores a `TagId`; what a person read was the name,
/// so that is what an ordinary full-text search has to find.
#[test]
fn a_tag_is_indexed_by_its_name_not_its_id() {
    let table = table(&[(TAG_A, "amazing")]);
    let fragments = fragments(&projection(vec![tagged(&[TAG_A])]), "Roadmap", Some(&table));
    assert_eq!(
        texts(&fragments),
        [(field::TASK, "Hello"), ("attr:koru.tag", "amazing")]
    );
}

/// A rename is one record in the system document and no node write at all —
/// which is the whole reason tags are entities. The same node's fragments come
/// out spelled the new way.
#[test]
fn a_renamed_tag_rebuilds_as_the_new_name() {
    let node = tagged(&[TAG_A]);
    let before = table(&[(TAG_A, "amazing")]);
    let after = table(&[(TAG_A, "astonishing")]);
    assert_eq!(
        texts(&fragments(
            &projection(vec![node.clone()]),
            "Roadmap",
            Some(&before)
        ))[1],
        ("attr:koru.tag", "amazing")
    );
    assert_eq!(
        texts(&fragments(&projection(vec![node]), "Roadmap", Some(&after)))[1],
        ("attr:koru.tag", "astonishing")
    );
}

/// **Merged away and forgotten degrades quietly.** No panic, no blank fragment,
/// and no uuid pretending to be a word — the unlabelled-reference rule, applied
/// to the same situation.
#[test]
fn a_tag_id_the_table_does_not_know_produces_no_fragment() {
    let table = table(&[(TAG_A, "amazing")]);
    let one_known = fragments(
        &projection(vec![tagged(&[TAG_A, TAG_B])]),
        "Roadmap",
        Some(&table),
    );
    assert_eq!(
        texts(&one_known),
        [(field::TASK, "Hello"), ("attr:koru.tag", "amazing")]
    );

    // And an empty table is the same case for every id, not a crash.
    let none_known = fragments(
        &projection(vec![tagged(&[TAG_A])]),
        "Roadmap",
        Some(&TagTable::default()),
    );
    assert_eq!(texts(&none_known), [(field::TASK, "Hello")]);
}

/// **No table at all is not the same as a table that says no.** A host wiring
/// no source, or a workspace whose system document has not been read yet, keeps
/// what the index always held rather than making tags vanish while it waits.
#[test]
fn with_no_table_a_tag_is_still_indexed_as_the_id_it_was() {
    let fragments = fragments(&projection(vec![tagged(&[TAG_A])]), "Roadmap", None);
    assert_eq!(
        texts(&fragments),
        [(field::TASK, "Hello"), ("attr:koru.tag", TAG_A)]
    );
}

/// Only `koru.tag` is resolved. Another role's value is its own text and must
/// not be looked up in a table it has nothing to do with.
#[test]
fn the_table_is_only_consulted_for_the_tag_role() {
    let mut node = tagged(&[TAG_A]);
    node.attrs.push(ProjectedAttr {
        id: "a9".into(),
        role: "owner".into(),
        kind: "text".into(),
        value: TAG_B.into(),
        zone: None,
        label: Some("Steven".into()),
    });
    let table = table(&[(TAG_A, "amazing")]);
    let fragments = fragments(&projection(vec![node]), "Roadmap", Some(&table));
    assert_eq!(
        texts(&fragments),
        [
            (field::TASK, "Hello"),
            ("attr:koru.tag", "amazing"),
            ("attr:owner", "Steven"),
        ]
    );
}

/// **A `NodeId` means nothing outside the document that minted it.** Two
/// documents that happened to mint the same node id must not share a key.
#[test]
fn every_key_names_its_document() {
    let one = projection(vec![node("n1", "paragraph", "text")]);
    let two = projection(vec![node("n1", "paragraph", "text")]);
    let (a, b) = (fragments(&one, "A", None), fragments(&two, "B", None));
    assert!(a.iter().all(|f| f.key.contains(&one.document.to_storage())));
    assert!(
        a.iter()
            .all(|left| !b.iter().any(|right| right.key == left.key))
    );
}

#[test]
fn a_workspace_contributes_its_own_name_and_belongs_to_no_document() {
    let fragment = workspace_fragment("w", "Acme").expect("a name is matchable");
    assert_eq!(fragment.document, None);
    assert_eq!(fragment.field, field::WORKSPACE);
    assert!(workspace_fragment("w", "  ").is_none());
}

// ------------------------------------------------------- typed attributes

fn an_attr(role: &str, kind: &str, value: &str) -> ProjectedAttr {
    ProjectedAttr {
        id: format!("{role}-1"),
        role: role.to_owned(),
        kind: kind.to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    }
}

fn with_attrs(attrs: Vec<ProjectedAttr>) -> DocumentProjection {
    let mut n = node("n1", "paragraph", "Ship it");
    n.attrs = attrs;
    projection(vec![n])
}

/// **Every attribute produces a typed row, whatever its kind** — the walk has
/// no per-role branch, which is what makes a workspace's own attribute work on
/// the day it is defined rather than on the day somebody adds a case for it.
#[test]
fn every_attribute_keeps_its_declared_type() {
    let rows = attr_values(
        "w",
        "d",
        &with_attrs(vec![
            an_attr("koru.due", "date", "2026-09-10"),
            an_attr("koru.priority", "text", "high"),
            an_attr("user.effort", "number", "3.5"),
            an_attr("user.blocked", "bool", "true"),
        ]),
    );
    assert_eq!(rows.len(), 4);
    let by_role = |role: &str| rows.iter().find(|r| r.role == role).unwrap().clone();
    assert_eq!(by_role("koru.due").kind, "date");
    assert_eq!(by_role("koru.due").value, "2026-09-10");
    assert_eq!(by_role("user.effort").num, Some(3.5));
    assert_eq!(by_role("koru.priority").num, None, "text has no ordering");
    assert_eq!(by_role("user.blocked").value, "true");
}

/// **Only the two kinds that *are* numbers get one.** A `date` keeps its ISO
/// text and no number at all, because a number is the column a `date` and a
/// `datetime_zoned` could be compared through, and comparing them is the thing
/// the three temporal types exist to prevent (ADR-189).
#[test]
fn only_numbers_and_instants_are_comparable_as_numbers() {
    let rows = attr_values(
        "w",
        "d",
        &with_attrs(vec![
            an_attr("a", "date", "2026-09-10"),
            an_attr("b", "datetime_floating", "2026-09-10T17:00:00"),
            an_attr("c", "datetime_zoned", "1789000000000"),
            an_attr("d", "number", "42"),
        ]),
    );
    let num = |role: &str| rows.iter().find(|r| r.role == role).unwrap().num;
    assert_eq!(num("a"), None, "a calendar day is not an instant");
    assert_eq!(num("b"), None, "and neither is a floating wall clock");
    assert_eq!(num("c"), Some(1_789_000_000_000.0));
    assert_eq!(num("d"), Some(42.0));
}

/// A value that will not parse answers `None`, **never zero** — zero is 1970,
/// and a malformed instant that quietly sorts as the epoch is worse than one
/// no range matches.
#[test]
fn an_unparsable_number_is_absent_rather_than_zero() {
    let rows = attr_values("w", "d", &with_attrs(vec![an_attr("x", "number", "many")]));
    assert_eq!(rows.len(), 1, "the row is still written");
    assert_eq!(rows[0].num, None);
    assert_eq!(rows[0].value, "many", "and the text it had is kept");
}

/// The zone rides along for display and takes no part in ordering: two
/// instants an hour apart are ordered by the instant, whatever they were
/// written in.
#[test]
fn a_zoned_instant_carries_its_zone_beside_the_number() {
    let mut attr = an_attr("koru.due", "datetime_zoned", "1789000000000");
    attr.zone = Some("Europe/Zurich".into());
    let rows = attr_values("w", "d", &with_attrs(vec![attr]));
    assert_eq!(rows[0].zone.as_deref(), Some("Europe/Zurich"));
    assert_eq!(rows[0].num, Some(1_789_000_000_000.0));
}

/// **A tag keeps its id here**, where its fragment keeps its name: the two
/// answer different questions, and this row is what makes "carries any tag"
/// answerable without the table.
#[test]
fn a_tag_row_keeps_the_id_the_node_stores() {
    let rows = attr_values(
        "w",
        "d",
        &with_attrs(vec![an_attr(
            TAG_ROLE,
            "ref",
            "0191d0f2-0000-7000-8000-00000000000a",
        )]),
    );
    assert_eq!(rows[0].value, "0191d0f2-0000-7000-8000-00000000000a");
}

/// An empty value is not a value. It would match an `exists` filter while
/// being nothing a person set.
#[test]
fn an_empty_value_writes_no_row() {
    let rows = attr_values("w", "d", &with_attrs(vec![an_attr("x", "text", "   ")]));
    assert!(rows.is_empty());
}

// ------------------------------------------------------------------ a task

/// Both halves of this file's output, put into a real index: the fragments the
/// walk wrote and the typed rows beside them, under the document's own listing.
fn indexed(projection: &DocumentProjection, title: &str) -> SearchIndex {
    let workspace = projection.workspace.to_storage();
    let document = projection.document.to_storage();
    let mut index = SearchIndex::in_memory().unwrap();
    index
        .set_listing(
            &workspace,
            "Acme",
            b"d1",
            &[ListedDocument {
                document: projection.document,
                parent: None,
                ordinal: 0,
                title: title.to_owned(),
                kind: "page".to_owned(),
                mime: None,
                trashed_at: None,
                trashed_by: None,
            }],
        )
        .unwrap();
    // The title is not among them: `set_listing` above is its writer, and the
    // index enforces that with a unique key rather than trusting the caller.
    index
        .replace_document(
            &workspace,
            &document,
            &fragments(projection, title, None),
            &attr_values(&workspace, &document, projection),
            b"r1",
            10,
        )
        .unwrap();
    index
}

/// Every hit as `(node, field)`, in the order the index answered.
fn found(index: &SearchIndex, text: &str, attributes: Vec<AttrFilter>) -> Vec<(String, String)> {
    index
        .query(
            &SearchQuery {
                text: text.to_owned(),
                attributes,
                ..SearchQuery::default()
            },
            &[],
        )
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| (hit.node.unwrap_or_default(), hit.field))
        .collect()
}

/// A filter on the marker. `koru.task` is a `Select`, and a select is **stored
/// as text** — so `text` is the kind a filter has to name, not `select`
/// (`AttrValue::key`, and `walk.rs` copies that token into the projection).
fn marker(op: AttrOp, values: &[&str]) -> AttrFilter {
    AttrFilter {
        key: "koru.task".to_owned(),
        op,
        values: values.iter().map(|v| (*v).to_owned()).collect(),
        numeric_values: Vec::new(),
        kinds: vec!["text".to_owned()],
    }
}

/// **A task is findable, and nothing here knows what a task is.** The marker is
/// an attribute like any other: its text goes down the walk's ordinary path and
/// its typed row down `attr_values`, so the line is found by what somebody typed
/// *and* narrowed by the marker — with no per-role branch in this file, which is
/// what makes the next attribute work on the day it is defined.
///
/// The two halves are separate claims. The text half proves the task's own line
/// is indexed under [`field::TASK`]; the filter half proves
/// `attributes: [koru.task]` narrows to the marked node and leaves the prose
/// beside it out, which is the channel a tasks surface asks its questions
/// through.
#[test]
fn a_marked_task_is_found_by_its_text_and_narrowed_by_its_marker() {
    let mut task = node("t1", "task", "Ship the beta");
    task.attrs = vec![
        an_attr("koru.task", "text", "explicit"),
        an_attr("koru.due", "date", "2026-09-10"),
    ];
    // A line that reads the same way and is not a task, so the filter has
    // something to narrow away from rather than an empty room to be right in.
    let prose = node("p1", "paragraph", "Ship the beta announcement");
    let projection = projection(vec![node("root", "document", "Roadmap"), task, prose]);
    let index = indexed(&projection, "Roadmap");

    assert_eq!(
        found(&index, "beta", Vec::new()),
        [
            ("t1".to_owned(), field::TASK.to_owned()),
            ("p1".to_owned(), field::BODY.to_owned()),
        ],
        "the task's own text is indexed, beside the prose that shares the word"
    );

    assert_eq!(
        found(&index, "beta", vec![marker(AttrOp::Exists, &[])]),
        [("t1".to_owned(), field::TASK.to_owned())],
        "and the marker narrows to the node carrying it"
    );

    assert_eq!(
        found(&index, "beta", vec![marker(AttrOp::Is, &["explicit"])]),
        [("t1".to_owned(), field::TASK.to_owned())],
        "the reason it became a task is a value, not a flag"
    );
    assert!(
        found(&index, "beta", vec![marker(AttrOp::Is, &["tasks_view"])]).is_empty(),
        "and another reason is a different question"
    );

    assert_eq!(
        found(&index, "beta", vec![marker(AttrOp::Missing, &[])]),
        [("p1".to_owned(), field::BODY.to_owned())],
        "the complement is the same filter, negated"
    );

    // The due it carries is searchable text as well as a typed row — the walk
    // has no exception for it either.
    assert_eq!(
        found(&index, "2026-09-10", Vec::new()),
        [("t1".to_owned(), "attr:koru.due".to_owned())]
    );
}
