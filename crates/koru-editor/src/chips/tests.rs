// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::WorkspaceId;
use koru_core::attributes::AccentColor;
use koru_core::node::{AttrValue, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use std::collections::HashMap as StdHashMap;

fn ws() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "chips").unwrap()
}

fn read(ws: &WorkspaceDoc, id: koru_core::NodeId) -> Node {
    ws.read(id).unwrap()
}

/// The invariant the roadmap spells out: unset attributes are invisible. A
/// plain paragraph earns no chips at all.
#[test]
fn a_node_with_no_attributes_earns_no_chips() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("plain").unwrap())
        .unwrap();
    assert!(attribute_chips(&read(&ws, id)).is_empty());
}

/// Tags come first, in stable attr order, then due, then priority, then the
/// color swatch — and each carries the id / value / token, never a display
/// string.
#[test]
fn set_attributes_earn_chips_in_display_order() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("tagged").unwrap())
        .unwrap();
    let a = TagId::new();
    let b = TagId::new();
    ws.add_tag(id, a).unwrap();
    ws.add_tag(id, b).unwrap();
    let due = koru_core::time::DateOnly::parse("2026-08-31").unwrap();
    ws.set_attribute(id, koru_core::attributes::KEY_DUE, AttrValue::Date(due))
        .unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_PRIORITY,
        AttrValue::Text("high".to_owned()),
    )
    .unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_COLOR,
        AttrValue::Text(AccentColor::Teal.as_str().to_owned()),
    )
    .unwrap();

    let chips = attribute_chips(&read(&ws, id));
    assert_eq!(chips.len(), 5);
    assert!(matches!(chips[0], ChipSpec::Tag(_)));
    assert!(matches!(chips[1], ChipSpec::Tag(_)));
    assert!(
        matches!(&chips[2], ChipSpec::Due(Some(koru_core::time::Temporal::Date(d))) if *d == due)
    );
    assert!(matches!(chips[3], ChipSpec::Priority(Priority::High)));
    assert!(matches!(&chips[4], ChipSpec::Color(token) if token == "teal"));
    let tags: Vec<TagId> = chips
        .iter()
        .filter_map(|c| match c {
            ChipSpec::Tag(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert!(tags.contains(&a) && tags.contains(&b));

    // Clearing takes the chips back out — unset is invisible again.
    ws.remove_tag(id, a).unwrap();
    ws.remove_tag(id, b).unwrap();
    for key in [
        koru_core::attributes::KEY_DUE,
        koru_core::attributes::KEY_PRIORITY,
        koru_core::attributes::KEY_COLOR,
    ] {
        ws.clear_attribute(id, key).unwrap();
    }
    assert!(attribute_chips(&read(&ws, id)).is_empty());
}

/// The due chip is a neutral labelled lozenge, its label derived at draw time
/// against the pass's `today` (never cached — a chip that says "Today" must
/// stop saying it tomorrow) and no overdue tint; **the priority chip wears its
/// level's colour** (`priority_accent`, the owner's choice of 2026-11-05).
#[test]
fn the_due_chip_is_neutral_and_dated_and_the_priority_wears_its_level() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_DUE,
        AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-31").unwrap()),
    )
    .unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_PRIORITY,
        AttrValue::Text("urgent".to_owned()),
    )
    .unwrap();
    let node = read(&ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    let tags = TagLookup::default();

    // Against a Wednesday five days before: within the weekday window.
    let near = chrono::NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
    let boxes = chip_boxes_on(&row, &mut engine, &tags, &ListLookup::default(), near);
    assert_eq!(boxes.len(), 2);
    assert_eq!(
        (boxes[0].kind, boxes[0].label.as_str()),
        (ChipKind::Due, "Mon")
    );
    assert_eq!(boxes[0].accent, None, "no accent on a due chip");
    assert_eq!(
        (boxes[1].kind, boxes[1].label.as_str()),
        (ChipKind::Priority, "Urgent")
    );
    assert_eq!(
        boxes[1].accent.as_deref(),
        Some("red"),
        "Urgent is red, by `priority_accent`"
    );

    // The same spec against a distant today: the label re-derives.
    let far = chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
    let boxes = chip_boxes_on(&row, &mut engine, &tags, &ListLookup::default(), far);
    assert_eq!(boxes[0].label, "Aug 31");
}

// ------------------------------------------------- the task's due chip
//
// SPEC §4, owner correction 2026-08-31: **a task is shown by its due chip and
// there is no separate task chip.** So the assertions below are all about one
// lozenge in three states — dated, dashed, and absent — rather than about a
// new element.

/// Marks `id` a task the way `/task` does, through the canonical command.
fn mark(ws: &mut WorkspaceDoc, id: koru_core::NodeId) {
    koru_core::tasks::ensure_task(ws, id, koru_core::attributes::TaskReason::Explicit).unwrap();
}

/// The one chip a row has, or a panic naming what it had instead.
fn only_chip(ws: &WorkspaceDoc, id: koru_core::NodeId) -> ChipBox {
    let node = read(ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    let mut boxes = chip_boxes_on(
        &row,
        &mut engine,
        &TagLookup::default(),
        &ListLookup::default(),
        chrono::NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
    );
    assert_eq!(boxes.len(), 1, "expected exactly one chip: {boxes:?}");
    boxes.remove(0)
}

/// **A bare `/task` earns the due chip, dashed.** The marker is set and no
/// date is, and a task that drew nothing would be the one state the row could
/// not tell you about.
#[test]
fn a_dateless_task_shows_the_due_chip_with_a_dash() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("write the report").unwrap())
        .unwrap();
    assert!(attribute_chips(&read(&ws, id)).is_empty());

    mark(&mut ws, id);
    let chips = attribute_chips(&read(&ws, id));
    assert_eq!(chips, vec![ChipSpec::Due(None)], "the same chip, no value");

    let placed = only_chip(&ws, id);
    assert_eq!(placed.kind, ChipKind::Due, "the due chip, not a new kind");
    assert_eq!(placed.label, DUE_NONE);
    assert_eq!(placed.accent, None, "neutral, like every due chip");
}

/// **A due-bearing row that becomes a task keeps the chip it had.** The date
/// is what the chip says; the marker adds nothing beside it, which is the
/// whole of "there is no separate task chip".
#[test]
fn a_dated_task_shows_its_date_and_nothing_more() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("ship it").unwrap())
        .unwrap();
    let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
    ws.set_attribute(id, koru_core::attributes::KEY_DUE, AttrValue::Date(due))
        .unwrap();
    let before = attribute_chips(&read(&ws, id));

    mark(&mut ws, id);
    assert_eq!(
        before,
        attribute_chips(&read(&ws, id)),
        "the chip is the same"
    );
    assert!(matches!(
        &attribute_chips(&read(&ws, id))[..],
        [ChipSpec::Due(Some(koru_core::time::Temporal::Date(d)))] if *d == due
    ));
    assert_eq!(only_chip(&ws, id).label, "Fri", "the date, not the dash");

    // And the date coming off leaves the task, so the chip dashes rather than
    // disappearing — `clear_due` marks first, which is what makes this a task
    // with no date rather than a plain row.
    koru_core::tasks::clear_due(&mut ws, id).unwrap();
    assert_eq!(attribute_chips(&read(&ws, id)), vec![ChipSpec::Due(None)]);
}

/// **A row that is not a task earns no due chip**, however many other
/// attributes it carries. Unset stays invisible; the dash is a task's mark,
/// not an empty attribute's.
#[test]
fn a_row_that_is_not_a_task_earns_no_due_chip() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("just a line").unwrap())
        .unwrap();
    ws.add_tag(id, TagId::new()).unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_PRIORITY,
        AttrValue::Text("high".to_owned()),
    )
    .unwrap();
    let chips = attribute_chips(&read(&ws, id));
    assert!(
        !chips.iter().any(|c| matches!(c, ChipSpec::Due(_))),
        "priority is not a deadline and a tag is not a task: {chips:?}"
    );
}

/// **The chip is kind-agnostic.** Every task-capable kind wears the same one —
/// including the legacy [`Kind::Task`], which is a reused outline kind and
/// carries no dedicated rendering of its own (SPEC §4).
#[test]
fn the_dash_chip_is_the_same_on_every_task_capable_kind() {
    use koru_core::node::BlobHash;

    let mut ws = ws();
    let doc = ws
        .insert(Parent::Root, Node::document("A page").unwrap())
        .unwrap();
    let cases = [
        ("paragraph", Node::paragraph("a row").unwrap()),
        ("legacy task kind", Node::task("an outline task").unwrap()),
        (
            "image",
            Node::image(BlobHash("h".into()), "image/png", Some(1), Some(1)).unwrap(),
        ),
        (
            "file",
            Node::file("notes.pdf", BlobHash("f".into()), "application/pdf").unwrap(),
        ),
    ];
    for (what, node) in cases {
        let kind = node.kind;
        let id = ws.insert(Parent::Root, node).unwrap();
        assert!(
            attribute_chips(&read(&ws, id)).is_empty(),
            "{what}: nothing before it is a task"
        );
        mark(&mut ws, id);
        assert_eq!(
            attribute_chips(&read(&ws, id)),
            vec![ChipSpec::Due(None)],
            "{what} ({kind:?}) earns the one chip"
        );
    }

    // A page is promoted through its own title row, which is an ordinary row
    // carrying the document's root node.
    mark(&mut ws, doc);
    assert_eq!(attribute_chips(&read(&ws, doc)), vec![ChipSpec::Due(None)]);
}

/// **The dash chip offers its remove lane like every other chip.**
///
/// It did not, once, and the reasoning was sound while the lane meant *empty
/// this value*: a dateless task's due is already unset, so an x there would
/// have been a button that did nothing. The due chip's x now removes the
/// **task**, which a dateless task has as much of as a dated one — so the chip
/// that used to hide the lane is the one that most needs it, and `on_close` is
/// what says so to `press_chip`.
#[test]
fn the_dash_chip_offers_its_remove_lane_like_the_rest() {
    use koru_draw::chip::CHIP_CLOSE_W;

    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("write the report").unwrap())
        .unwrap();
    mark(&mut ws, id);
    let placed = only_chip(&ws, id);
    assert!(
        placed.close.is_some(),
        "a task with no date still has a task to take off"
    );

    let node = read(&ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    let tags = TagLookup::default();
    let hit = chip_at(
        &row,
        &mut engine,
        &tags,
        &ListLookup::default(),
        placed.rect.x + placed.rect.w - CHIP_CLOSE_W * 0.5,
        placed.rect.y + placed.rect.h * 0.5,
    )
    .expect("the trailing lane is still part of the chip");
    assert_eq!(hit.spec, ChipSpec::Due(None));
    assert!(
        hit.on_close,
        "the trailing lane is the remove lane, so the press asks to remove the task"
    );

    // And a *dated* task's chip is the same, which it always was.
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_DUE,
        AttrValue::Date(koru_core::time::DateOnly::parse("2026-09-04").unwrap()),
    )
    .unwrap();
    assert!(only_chip(&ws, id).close.is_some(), "dated, the same lane");
}

/// **A date pill is not echoed by a dash.** The pre-registry bare `due` role
/// is a *pill*: inline content that already prints the date on the row, and
/// [`is_task`](koru_core::tasks::is_task) counts it, so such a row is a task
/// with a deadline the chip lane must stay out of. It is the same reason
/// `attribute_chips` reads `KEY_DUE` rather than `Node::due_value` for the
/// dated chip — a chip beside the pill would print the date twice, and a dash
/// beside it would say there is no date at all.
#[test]
fn a_legacy_date_pill_earns_no_chip_at_all() {
    use koru_core::attributes::LEGACY_DUE_ROLE;
    use koru_core::node::Attr;

    let mut ws = ws();
    let id = ws
        .insert(
            Parent::Root,
            Node::paragraph("written by an older build").unwrap(),
        )
        .unwrap();
    ws.set_attr(
        id,
        &Attr::new(
            LEGACY_DUE_ROLE,
            AttrValue::Date(koru_core::time::DateOnly::parse("2026-09-11").unwrap()),
        ),
    )
    .unwrap();
    assert!(
        koru_core::tasks::is_task_node(&ws, id).unwrap(),
        "the bare role still makes it a task"
    );
    assert!(
        attribute_chips(&read(&ws, id)).is_empty(),
        "and the pill on the row is the date it shows"
    );

    // Marking it explicitly changes nothing: the date is still on the row.
    mark(&mut ws, id);
    assert!(attribute_chips(&read(&ws, id)).is_empty());
}

/// A rename touches the lookup, never the document: the same `ChipSpec`
/// resolves to the new name the moment the host replaces the table, and the
/// engine's chip cache keys on the label so the new name is a new visual.
#[test]
fn a_tag_rename_invalidates_through_the_lookup() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    let tag = TagId::new();
    ws.add_tag(id, tag).unwrap();
    let node = read(&ws, id);
    let mut row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    row.text = node.content.as_text().unwrap_or_default().to_owned();

    let mut engine = TextEngine::new();
    let mut tags = TagLookup::default();
    tags.replace([(
        tag,
        TagFace {
            name: "mvp".into(),
            color: None,
        },
    )]);
    let g = tags.generation;
    let before = chip_boxes(&row, &mut engine, &tags, &ListLookup::default());
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].label, "mvp");

    tags.replace([(
        tag,
        TagFace {
            name: "launch".into(),
            color: Some("red".into()),
        },
    )]);
    assert_ne!(tags.generation, g, "a replace is observable");
    let after = chip_boxes(&row, &mut engine, &tags, &ListLookup::default());
    assert_eq!(
        after[0].label, "launch",
        "the chip follows the table, not the doc"
    );
    assert_eq!(after[0].accent.as_deref(), Some("red"));
    assert!(
        engine
            .attr_chips
            .get(ChipKind::Tag, "launch", Some("red"), None)
            .is_some(),
        "the renamed label is its own cached visual"
    );
}

/// A chip draws the **leaf** of a hierarchical tag, never the whole path: a
/// block tagged `#fun/home` says `home`. The branch stays in the lookup for a
/// hover and for the picker, and the cache keys on what is drawn.
#[test]
fn a_hierarchical_tag_chips_its_leaf_and_caches_that_label() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    let tag = TagId::new();
    ws.add_tag(id, tag).unwrap();
    let node = read(&ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);

    let mut engine = TextEngine::new();
    let mut tags = TagLookup::default();
    let face = TagFace {
        name: "fun/home".into(),
        color: None,
    };
    assert_eq!(face.leaf(), "home");
    tags.replace([(tag, face)]);

    let boxes = chip_boxes(&row, &mut engine, &tags, &ListLookup::default());
    assert_eq!(boxes[0].label, "home", "the leaf, not `fun/home`");
    assert!(
        engine
            .attr_chips
            .get(ChipKind::Tag, "home", None, None)
            .is_some(),
        "and the cache is keyed on the drawn label"
    );
    assert!(
        engine
            .attr_chips
            .get(ChipKind::Tag, "fun/home", None, None)
            .is_none(),
        "the path is never measured"
    );
}

/// An id the table has forgotten still draws — a quiet placeholder — because
/// a peer can merge or lose a tag record concurrently (Q7's contract).
#[test]
fn a_forgotten_tag_id_still_earns_a_chip() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    ws.add_tag(id, TagId::new()).unwrap();
    let node = read(&ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    let empty = TagLookup::default();
    let boxes = chip_boxes(&row, &mut engine, &empty, &ListLookup::default());
    assert_eq!(boxes.len(), 1);
    assert_eq!(boxes[0].label, "…");
}

/// **Every kind reserves a remove lane** ([ADR-188], amended 2026-08-27), it is
/// the same width on all four, and it is always the chip's own trailing edge —
/// so the `×` a press lands on is the one the hover inked, whichever attribute
/// it belongs to. On the swatch it sits past the dot rather than over it,
/// because the dot is the half that opens the picker.
///
/// [ADR-188]: ../../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
#[test]
fn every_chip_kind_reserves_the_same_remove_lane() {
    use koru_draw::chip::{CHIP_CLOSE_W, SWATCH_D};

    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    let tag = TagId::new();
    ws.add_tag(id, tag).unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_DUE,
        AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-31").unwrap()),
    )
    .unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_PRIORITY,
        AttrValue::Text("low".to_owned()),
    )
    .unwrap();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_COLOR,
        AttrValue::Text(AccentColor::Teal.as_str().to_owned()),
    )
    .unwrap();

    let node = read(&ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    let mut tags = TagLookup::default();
    tags.replace([(
        tag,
        TagFace {
            name: "mvp".into(),
            color: None,
        },
    )]);

    let boxes = chip_boxes_on(
        &row,
        &mut engine,
        &tags,
        &ListLookup::default(),
        chrono::NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
    );
    assert_eq!(boxes.len(), 4, "tag, due, priority, swatch");
    for placed in &boxes {
        let close = placed
            .close
            .unwrap_or_else(|| panic!("{:?} reserves a remove lane", placed.kind));
        assert_eq!(close.w, CHIP_CLOSE_W, "{:?}", placed.kind);
        assert_eq!(
            close.x + close.w,
            placed.rect.x + placed.rect.w,
            "{:?}: the lane is the chip's trailing edge",
            placed.kind
        );
        assert_eq!(close.y, placed.rect.y);
        assert_eq!(close.h, placed.rect.h);
    }

    // The swatch is a dot plus a lane, and the `×` is to the *right* of it.
    let swatch = boxes.last().expect("the swatch is last");
    assert_eq!(swatch.kind, ChipKind::Swatch);
    assert_eq!(swatch.rect.w, SWATCH_D + CHIP_CLOSE_W);
    assert_eq!(swatch.close.unwrap().x, swatch.rect.x + SWATCH_D);
}

/// Chips trail one another with the shared gap, and measuring twice serves
/// the second from the cache (same widths, same visuals).
#[test]
fn chips_lay_out_left_to_right_with_gaps() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    let (a, b) = (TagId::new(), TagId::new());
    ws.add_tag(id, a).unwrap();
    ws.add_tag(id, b).unwrap();
    let node = read(&ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    let mut tags = TagLookup::default();
    tags.replace([
        (
            a,
            TagFace {
                name: "aa".into(),
                color: None,
            },
        ),
        (
            b,
            TagFace {
                name: "bb".into(),
                color: None,
            },
        ),
    ]);
    let boxes = chip_boxes(&row, &mut engine, &tags, &ListLookup::default());
    assert_eq!(boxes.len(), 2);
    assert!(boxes[1].rect.x >= boxes[0].rect.x + boxes[0].rect.w + CHIP_GAP - 0.01);
    let again = chip_boxes(&row, &mut engine, &tags, &ListLookup::default());
    assert_eq!(again[0].rect.w, boxes[0].rect.w);
}

// ---------------------------------------------------------------------------
// The list table, and the due chip that wears its colour (plan P).
// ---------------------------------------------------------------------------

/// A task in a list, ready to be measured: the node, its row, and the id.
fn task_in_a_list(ws: &mut WorkspaceDoc) -> (Paragraph, koru_core::ids::ListId) {
    let id = ws
        .insert(Parent::Root, Node::paragraph("ship it").unwrap())
        .unwrap();
    mark(ws, id);
    let list = koru_core::ids::ListId::new();
    ws.set_attribute(
        id,
        koru_core::attributes::KEY_LIST,
        AttrValue::Text(list.to_storage()),
    )
    .unwrap();
    let node = read(ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    (row, list)
}

/// **The push replaces, and an unchanged one is not a change.** `ListLookup`
/// keeps a generation for exactly this: the host pushes every turn, so what
/// says whether anything moved has to be cheap and has to be here.
#[test]
fn the_list_table_replaces_wholesale_and_counts_its_generations() {
    let a = koru_core::ids::ListId::new();
    let b = koru_core::ids::ListId::new();
    let mut lists = ListLookup::default();
    assert!(lists.is_empty());
    let start = lists.generation;

    lists.replace([(a, Some("teal".to_owned())), (b, None)]);
    assert_eq!(lists.resolve(a), Some("teal"));
    assert_eq!(lists.resolve(b), None, "a list with no colour is neutral");
    assert!(!lists.is_empty());
    assert_ne!(lists.generation, start);

    // Replace, not merge: `a` is gone because the new table does not name it.
    lists.replace([(b, Some("amber".to_owned()))]);
    assert_eq!(lists.resolve(a), None);
    assert_eq!(lists.resolve(b), Some("amber"));
}

/// **A task in a list draws its due chip in that list's tone.** Membership is
/// set state, so the chip says what is set — the rule every other chip here
/// follows — and the accent lane was already in the cache key, so nothing
/// about the recipe changes.
#[test]
fn a_task_in_a_list_wears_that_lists_colour_on_its_due_chip() {
    let mut ws = ws();
    let (row, list) = task_in_a_list(&mut ws);
    let mut engine = TextEngine::new();
    let mut lists = ListLookup::default();
    lists.replace([(list, Some(AccentColor::Teal.as_str().to_owned()))]);

    let boxes = chip_boxes(&row, &mut engine, &TagLookup::default(), &lists);
    let due = boxes
        .iter()
        .find(|b| b.kind == ChipKind::Due)
        .expect("a task earns a due chip, dated or not");
    assert_eq!(due.accent.as_deref(), Some(AccentColor::Teal.as_str()));
}

/// An id the table has forgotten resolves to `None` and the chip draws
/// neutral — the same answer a list with no colour gets, and the same answer a
/// forgotten *tag* gets. A peer can delete a list concurrently; that is an
/// ordinary state, not an error, and the chip is still the task's due chip.
#[test]
fn a_list_the_table_never_heard_of_leaves_the_chip_neutral() {
    let mut ws = ws();
    let (row, _list) = task_in_a_list(&mut ws);
    let mut engine = TextEngine::new();

    let boxes = chip_boxes(
        &row,
        &mut engine,
        &TagLookup::default(),
        &ListLookup::default(),
    );
    let due = boxes
        .iter()
        .find(|b| b.kind == ChipKind::Due)
        .expect("still a chip");
    assert_eq!(due.accent, None);
    assert_eq!(due.label, DUE_NONE, "and still says what it says");
}

/// **The hit test is given the same table the paint is.** The accent changes
/// no width, so a lookup that reached only the paint would leave the geometry
/// correct today and split one chip across two cache entries — free to diverge
/// the day an accent does affect a width. This is the guard for that.
#[test]
fn the_hit_test_and_the_paint_resolve_one_chip_the_same_way() {
    let mut ws = ws();
    let (row, list) = task_in_a_list(&mut ws);
    let mut engine = TextEngine::new();
    let mut lists = ListLookup::default();
    lists.replace([(list, Some(AccentColor::Purple.as_str().to_owned()))]);

    let placed = chip_boxes(&row, &mut engine, &TagLookup::default(), &lists)
        .into_iter()
        .find(|b| b.kind == ChipKind::Due)
        .expect("the due chip");
    let hit = chip_at(
        &row,
        &mut engine,
        &TagLookup::default(),
        &lists,
        placed.rect.x + placed.rect.w * 0.5,
        placed.rect.y + placed.rect.h * 0.5,
    )
    .expect("aimed at the middle of the chip the geometry answered");
    assert_eq!(hit.spec, ChipSpec::Due(None));

    // The table an empty lookup produces is the same *geometry*, which is what
    // makes this worth asserting: the two agree because they share one call,
    // not because the accent happens not to matter.
    let neutral = chip_boxes(
        &row,
        &mut engine,
        &TagLookup::default(),
        &ListLookup::default(),
    )
    .into_iter()
    .find(|b| b.kind == ChipKind::Due)
    .expect("the due chip");
    assert_eq!(neutral.rect.w, placed.rect.w, "the accent changes no width");
}

// ---------------------------------------------------------------------------
// The queue a due chip carries (plan C).
// ---------------------------------------------------------------------------

/// A task due `days` from `today`, laid out and measured on that day.
fn queued(ws: &mut WorkspaceDoc, days: i64, today: chrono::NaiveDate) -> ChipBox {
    let id = ws
        .insert(Parent::Root, Node::paragraph("row").unwrap())
        .unwrap();
    mark(ws, id);
    let due = koru_core::time::DateOnly::new(today + chrono::Duration::days(days));
    ws.set_attribute(id, koru_core::attributes::KEY_DUE, AttrValue::Date(due))
        .unwrap();
    let node = read(ws, id);
    let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
    let mut engine = TextEngine::new();
    chip_boxes_on(
        &row,
        &mut engine,
        &TagLookup::default(),
        &ListLookup::default(),
        today,
    )
    .into_iter()
    .find(|b| b.kind == ChipKind::Due)
    .expect("a task earns a due chip")
}

/// **The four queues, the four glyphs.** The chip and the rail draw one picture
/// per queue because they read one table — and the placement is
/// `koru_core::tasks::bucket`'s, which is the same function the projection
/// store's own placement is held equal to.
///
/// The +60 case is the one worth naming: it is *Later*, not *Upcoming*. The
/// copy of this rule that used to live in `taskcounts.rs` had drifted exactly
/// there, and a second comparison against today written inside a drawing pass
/// is how that happens again.
#[test]
fn a_dated_task_carries_its_queues_glyph() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 5).unwrap();
    for (days, bucket) in [
        (0, Bucket::Today),
        (3, Bucket::Next),
        (14, Bucket::Upcoming),
        (60, Bucket::Later),
    ] {
        let mut ws = ws();
        let placed = queued(&mut ws, days, today);
        assert_eq!(
            placed.lead,
            Some(bucket_glyph(bucket)),
            "+{days} days is {bucket:?}"
        );
    }
}

/// Overdue is *Today* (`days <= 0`), so the label moves and **the glyph does
/// not**. This is the pair that catches a glyph missing from the cache key:
/// the label is keyed and would move on its own.
#[test]
fn an_overdue_task_stays_in_today_while_its_label_moves() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 5).unwrap();
    let mut on_time = ws();
    let due_today = queued(&mut on_time, 0, today);
    let mut late = ws();
    let overdue = queued(&mut late, -3, today);

    assert_eq!(due_today.lead, Some(bucket_glyph(Bucket::Today)));
    assert_eq!(overdue.lead, Some(bucket_glyph(Bucket::Today)));
    assert_ne!(
        overdue.label, due_today.label,
        "the words differ, which is the half that is keyed"
    );
}

/// **An open dateless task carries no glyph.** Its queue is Next or Later
/// purely by the priority chip beside it, and an icon there would restate a
/// neighbour. The accent is untouched by that: membership does not depend on a
/// date.
#[test]
fn an_open_dateless_task_leads_with_nothing_and_still_takes_its_list() {
    let mut ws = ws();
    let (row, list) = task_in_a_list(&mut ws);
    let mut engine = TextEngine::new();
    let mut lists = ListLookup::default();
    lists.replace([(list, Some(AccentColor::Teal.as_str().to_owned()))]);

    let placed = chip_boxes(&row, &mut engine, &TagLookup::default(), &lists)
        .into_iter()
        .find(|b| b.kind == ChipKind::Due)
        .expect("the dash chip");
    assert_eq!(placed.lead, None);
    assert_eq!(placed.accent.as_deref(), Some(AccentColor::Teal.as_str()));
}

/// **A completed task carries the check-square whether or not it has a due**,
/// and this is the assertion that keeps §5 A's cache key honest.
///
/// `struck` is not in [`ChipKey`], so a completed dateless chip and an open one
/// would share the key `(Due, "No date", accent, None)` and differ only in the
/// strike — and the cache answers whichever was measured first. They do not
/// share it, because `bucket` returns `Completed` before it reads the due, so
/// the completed one leads with a glyph the open one has not got.
#[test]
fn a_completed_task_leads_with_the_check_square_dated_or_not() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 5).unwrap();
    let check = bucket_glyph(Bucket::Completed);

    for dated in [true, false] {
        let mut ws = ws();
        let id = ws
            .insert(Parent::Root, Node::paragraph("row").unwrap())
            .unwrap();
        mark(&mut ws, id);
        if dated {
            let due = koru_core::time::DateOnly::new(today);
            ws.set_attribute(id, koru_core::attributes::KEY_DUE, AttrValue::Date(due))
                .unwrap();
        }
        koru_core::tasks::complete_task(
            &mut ws,
            id,
            koru_core::tasks::LifecycleReason::ExplicitComplete,
            crate::tasks::today(),
        )
        .unwrap();

        let node = read(&ws, id);
        let row = Paragraph::from_node(&node, 0, Vec::new(), StdHashMap::new(), None, 0);
        let mut engine = TextEngine::new();
        let placed = chip_boxes_on(
            &row,
            &mut engine,
            &TagLookup::default(),
            &ListLookup::default(),
            today,
        )
        .into_iter()
        .find(|b| b.kind == ChipKind::Due)
        .expect("a completed task still shows its chip");
        assert_eq!(
            placed.lead,
            Some(check),
            "completed, dated: {dated} — the check-square either way"
        );
    }
}

/// **A document's title names its list; its other lines do not.** The title
/// is the line that stands for the whole document's task, so it wears the
/// list's name — ahead of the due chip, in the Tasks page's order — and a
/// body line keeps saying its list through the due chip's colour alone.
#[test]
fn the_title_wears_its_list_by_name_and_a_body_line_does_not() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Test").unwrap();
    let (mut row, list) = task_in_a_list(&mut ws);
    assert!(
        !row.chips.contains(&ChipSpec::List(list)),
        "a body line: the colour only"
    );

    row.is_title = true;
    row.wear_title_chips();
    assert_eq!(row.chips.first(), Some(&ChipSpec::List(list)), "first");
    let before = row.chips.clone();
    row.wear_title_chips();
    assert_eq!(row.chips, before, "worn once");

    let mut lists = ListLookup::default();
    lists.replace_named([(list, "Bike Project".to_owned(), Some("blue".to_owned()))]);
    let boxes = chip_boxes_on(
        &row,
        &mut TextEngine::new(),
        &TagLookup::default(),
        &lists,
        chrono::NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
    );
    assert_eq!(boxes[0].label, "Bike Project", "by name");
    assert_eq!(
        boxes[0].accent.as_deref(),
        Some("blue"),
        "in the list's colour"
    );
}
