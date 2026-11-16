// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::WorkspaceId;
use crate::node::Node;
use crate::schema::Parent;
use crate::time::{DateOnly, FloatingDateTime, Temporal, TimeZoneName, ZonedDateTime};

fn one_node() -> (WorkspaceDoc, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "attrs").unwrap();
    let node = ws
        .insert(Parent::Root, Node::task("Reward flow").unwrap())
        .unwrap();
    (ws, node)
}

fn date(s: &str) -> AttrValue {
    AttrValue::Date(DateOnly::parse(s).unwrap())
}

fn on(s: &str) -> Temporal {
    Temporal::Date(DateOnly::parse(s).unwrap())
}

fn zoned(instant_ms: i64, zone: &str) -> AttrValue {
    AttrValue::ZonedDateTime(ZonedDateTime::new(
        instant_ms,
        TimeZoneName::parse(zone).unwrap(),
    ))
}

fn floating(wall: &str) -> AttrValue {
    AttrValue::FloatingDateTime(FloatingDateTime::parse(wall).unwrap())
}

// ------------------------------------------------------------ the registry

#[test]
fn every_system_key_resolves_and_is_its_own_id() {
    for key in [
        KEY_TAG,
        KEY_LIST,
        KEY_COLOR,
        KEY_DUE,
        KEY_PRIORITY,
        KEY_PLAN,
    ] {
        let def = system_definition(key).unwrap_or_else(|| panic!("{key} is registered"));
        assert_eq!(def.id, key, "a system attribute's id is its key");
        assert!(def.system);
    }
    assert_eq!(
        system_definition("koru.status"),
        None,
        "reserved, not yet registered"
    );
    assert_eq!(system_definition("customer"), None);
}

#[test]
fn plan_is_personal_and_the_rest_are_shared() {
    for def in system_definitions() {
        let expected = if def.key == KEY_PLAN {
            AttributeScope::Personal
        } else {
            AttributeScope::Shared
        };
        assert_eq!(def.scope, expected, "{}", def.key);
    }
}

#[test]
fn priority_tokens_round_trip_and_compare_semantically() {
    for p in Priority::ALL {
        assert_eq!(Priority::parse(p.as_str()), Some(p));
    }
    assert!(Priority::Urgent > Priority::High);
    assert!(Priority::High > Priority::Medium);
    assert!(Priority::Medium > Priority::Low);
    // Unset is absence, never a fifth token.
    assert_eq!(Priority::parse("none"), None);
    assert_eq!(Priority::parse("unset"), None);
}

#[test]
fn accent_tokens_round_trip() {
    for c in AccentColor::ALL {
        assert_eq!(AccentColor::parse(c.as_str()), Some(c));
    }
    assert_eq!(AccentColor::parse("#ff0000"), None, "hex is not a token");
}

#[test]
fn value_types_round_trip_and_unknown_parses_as_none() {
    for t in ValueType::ALL {
        assert_eq!(ValueType::parse(t.as_str()), Some(t));
    }
    // Reserved by the roadmap, deliberately not variants yet.
    assert_eq!(ValueType::parse("person"), None);
    assert_eq!(ValueType::parse("relation"), None);
}

#[test]
fn a_custom_definition_mints_a_uuid_id_unrelated_to_its_key() {
    let a = AttributeDefinition::custom("customer", "Customer", ValueType::Text);
    let b = AttributeDefinition::custom("customer", "Customer", ValueType::Text);
    assert_ne!(a.id, b.id, "never derived from the key");
    assert!(uuid::Uuid::parse_str(&a.id).is_ok());
}

// ------------------------------------------------------------ validation

#[test]
fn validation_checks_the_type() {
    let due = system_definition(KEY_DUE).unwrap();
    assert!(validate(&due, &date("2026-08-30")).is_ok());
    assert!(
        validate(&due, &zoned(1_790_000_000_000, "America/New_York")).is_ok(),
        "a shared timed attribute takes a date or a zoned instant"
    );
    assert!(validate(&due, &AttrValue::Text("friday".into())).is_err());

    let number = AttributeDefinition::custom("confidence", "Confidence", ValueType::Number);
    assert!(validate(&number, &AttrValue::Number(0.8)).is_ok());
    assert!(validate(&number, &AttrValue::Bool(true)).is_err());

    let only_date = AttributeDefinition::custom("anniversary", "Anniversary", ValueType::Date);
    assert!(validate(&only_date, &date("2026-01-01")).is_ok());
    assert!(
        validate(&only_date, &floating("2026-01-01T00:00:00")).is_err(),
        "a date-typed attribute stays a calendar date"
    );
    assert!(validate(&only_date, &zoned(0, "UTC")).is_err());
}

/// **The three temporal types do not substitute for one another**, and a
/// definition says which one its key takes ([ADR-183], rule 1 and rule 5).
/// This is where "no implicit conversion" stops being a convention: the wrong
/// shape does not land, so nothing downstream ever has to guess.
#[test]
fn a_shared_key_refuses_a_floating_value_and_a_personal_key_refuses_a_zoned_one() {
    let due = system_definition(KEY_DUE).unwrap();
    let plan = system_definition(KEY_PLAN).unwrap();

    // Shared: a deadline everyone must agree on cannot mean 17:00 wherever
    // each collaborator happens to be sitting.
    assert!(validate(&due, &zoned(0, "Europe/Zurich")).is_ok());
    assert!(validate(&due, &floating("2026-08-30T17:00:00")).is_err());

    // Personal: a plan follows the person, so it must not be pinned to the
    // place they were when they typed it.
    assert!(validate(&plan, &floating("2026-08-30T09:00:00")).is_ok());
    assert!(validate(&plan, &zoned(0, "Europe/Zurich")).is_err());

    // Both still take a plain calendar day.
    assert!(validate(&due, &date("2026-08-30")).is_ok());
    assert!(validate(&plan, &date("2026-08-30")).is_ok());
}

/// The A1 gate item: a `user.effort: select` definition validates its tokens.
#[test]
fn a_custom_select_validates_its_tokens() {
    let effort = AttributeDefinition::custom("user.effort", "Effort", ValueType::Select)
        .with_options(vec!["small".into(), "medium".into(), "large".into()]);
    assert!(validate(&effort, &AttrValue::Text("medium".into())).is_ok());
    assert!(validate(&effort, &AttrValue::Text("enormous".into())).is_err());
}

#[test]
fn system_selects_validate_their_tokens() {
    let priority = system_definition(KEY_PRIORITY).unwrap();
    assert!(validate(&priority, &AttrValue::Text("high".into())).is_ok());
    assert!(validate(&priority, &AttrValue::Text("critical".into())).is_err());

    let color = system_definition(KEY_COLOR).unwrap();
    assert!(validate(&color, &AttrValue::Text("teal".into())).is_ok());
    assert!(validate(&color, &AttrValue::Text("#00ffcc".into())).is_err());
}

#[test]
fn a_tag_value_is_an_id_not_a_name() {
    let tags = system_definition(KEY_TAG).unwrap();
    assert!(validate(&tags, &AttrValue::Text(TagId::new().to_storage())).is_ok());
    assert!(validate(&tags, &AttrValue::Text("mvp".into())).is_err());
}

/// An offset is not a timezone, and that is now enforced by the **type**
/// rather than by a validator somebody could forget to call: there is no way
/// to build a `TimeZoneName` from one, so there is no way to build a
/// `ZonedDateTime` carrying one.
#[test]
fn a_timezone_is_an_iana_name_never_an_offset() {
    for bad in ["+02:00", "-0400", "UTC-4"] {
        assert!(
            TimeZoneName::parse(bad).is_none(),
            "{bad} is an offset, and DST changes what a future offset will be"
        );
    }
}

// ------------------------------------------------------------ the ops

#[test]
fn set_and_clear_a_system_attribute() {
    let (mut ws, node) = one_node();
    ws.set_attribute(node, KEY_DUE, date("2026-08-30")).unwrap();
    assert_eq!(ws.read(node).unwrap().due(), Some(on("2026-08-30")));

    assert!(ws.clear_attribute(node, KEY_DUE).unwrap());
    assert_eq!(ws.read(node).unwrap().due(), None);
}

#[test]
fn an_unknown_key_is_refused_not_stored() {
    let (mut ws, node) = one_node();
    let err = ws
        .set_attribute(node, "deadline", date("2026-08-30"))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownAttribute(k) if k == "deadline"));
    assert!(ws.read(node).unwrap().content.attrs().is_empty());
}

#[test]
fn a_wrong_value_is_refused_not_stored() {
    let (mut ws, node) = one_node();
    let err = ws
        .set_attribute(node, KEY_PRIORITY, AttrValue::Text("critical".into()))
        .unwrap_err();
    assert!(matches!(err, StoreError::AttributeRejected { key, .. } if key == KEY_PRIORITY));
    assert!(ws.read(node).unwrap().content.attrs().is_empty());
}

/// Personal values live per profile (D4); the shared node refuses them.
#[test]
fn a_personal_attribute_never_lands_on_a_shared_node() {
    let (mut ws, node) = one_node();
    let err = ws
        .set_attribute(node, KEY_PLAN, floating("2026-08-30T09:00:00"))
        .unwrap_err();
    assert!(matches!(err, StoreError::AttributeRejected { key, .. } if key == KEY_PLAN));
    assert!(ws.read(node).unwrap().content.attrs().is_empty());
}

/// A cardinality-one key keeps one record: setting again updates in place,
/// under the same `AttrId`.
#[test]
fn setting_a_one_cardinality_key_twice_updates_one_record() {
    let (mut ws, node) = one_node();
    let first = ws.set_attribute(node, KEY_DUE, date("2026-08-30")).unwrap();
    let second = ws.set_attribute(node, KEY_DUE, date("2026-09-15")).unwrap();
    assert_eq!(first, second, "the record survives; the value moves");
    let read = ws.read(node).unwrap();
    assert_eq!(read.due(), Some(on("2026-09-15")));
    assert_eq!(read.content.attrs().len(), 1);
}

/// Convergence duplicates — two peers first-setting one key — fold into the
/// lowest id on the next set, the `ensure_trash_root` idiom at attr level.
#[test]
fn a_set_folds_convergence_duplicates_into_the_lowest_id() {
    let (mut ws, node) = one_node();
    // Two records under one role, as a merge of two first-sets leaves them.
    let a = Attr::new(KEY_DUE, date("2026-08-30"));
    let b = Attr::new(KEY_DUE, date("2026-08-31"));
    ws.set_attr(node, &a).unwrap();
    ws.set_attr(node, &b).unwrap();
    assert_eq!(ws.read(node).unwrap().content.attrs().len(), 2);

    let kept = ws.set_attribute(node, KEY_DUE, date("2026-09-01")).unwrap();
    assert_eq!(kept, a.id.min(b.id), "every peer picks the same survivor");
    let read = ws.read(node).unwrap();
    assert_eq!(read.content.attrs().len(), 1);
    assert_eq!(read.due(), Some(on("2026-09-01")));
}

#[test]
fn setting_the_same_value_again_is_a_no_op() {
    let (mut ws, node) = one_node();
    ws.enable_undo();
    ws.set_attribute(node, KEY_PRIORITY, AttrValue::Text("high".into()))
        .unwrap();
    ws.set_attribute(node, KEY_PRIORITY, AttrValue::Text("high".into()))
        .unwrap();
    assert!(ws.undo().unwrap(), "the one real change undoes");
    assert_eq!(ws.read(node).unwrap().priority(), None);
    assert!(
        !ws.can_undo(),
        "the idempotent repeat left no second step behind"
    );
}

/// Clearing what is not there must not leave an undo step — a delete of an
/// absent key is still an op, and therefore a phantom Ctrl+Z.
#[test]
fn clearing_an_absent_key_is_a_guarded_no_op() {
    let (mut ws, node) = one_node();
    ws.enable_undo();
    assert!(!ws.clear_attribute(node, KEY_DUE).unwrap());
    assert!(!ws.can_undo(), "no op, no phantom undo step");

    ws.remove_tag(node, TagId::new()).unwrap();
    assert!(!ws.can_undo(), "removing an absent tag is the same rule");
}

#[test]
fn each_attribute_command_is_exactly_one_undo_step() {
    let (mut ws, node) = one_node();
    ws.enable_undo();
    ws.set_attribute(node, KEY_DUE, date("2026-08-30")).unwrap();
    ws.set_attribute(node, KEY_PRIORITY, AttrValue::Text("urgent".into()))
        .unwrap();

    assert!(ws.undo().unwrap());
    let read = ws.read(node).unwrap();
    assert_eq!(
        read.priority(),
        None,
        "one undo took back only the priority"
    );
    assert_eq!(read.due(), Some(on("2026-08-30")));

    assert!(ws.undo().unwrap());
    assert_eq!(ws.read(node).unwrap().due(), None);
}

// ------------------------------------------------------------ tags

#[test]
fn tags_accumulate_and_remove_individually() {
    let (mut ws, node) = one_node();
    let mvp = TagId::new();
    let research = TagId::new();
    ws.add_tag(node, mvp).unwrap();
    ws.add_tag(node, research).unwrap();
    let mut tags: Vec<TagId> = ws.read(node).unwrap().tag_ids().collect();
    tags.sort_unstable();
    let mut expected = vec![mvp, research];
    expected.sort_unstable();
    assert_eq!(tags, expected);

    ws.remove_tag(node, mvp).unwrap();
    let tags: Vec<TagId> = ws.read(node).unwrap().tag_ids().collect();
    assert_eq!(tags, vec![research]);
}

#[test]
fn adding_a_tag_twice_stores_it_once() {
    let (mut ws, node) = one_node();
    let tag = TagId::new();
    ws.add_tag(node, tag).unwrap();
    ws.add_tag(node, tag).unwrap();
    assert_eq!(ws.read(node).unwrap().tag_ids().count(), 1);
}

// ------------------------------------------------------------ lists

/// Membership is one value, so putting a task in another list replaces the
/// first rather than accumulating — the cardinality-one half of SPEC §8.
#[test]
fn a_task_is_in_one_list_and_moving_it_replaces_the_value() {
    let (mut ws, node) = one_node();
    let wedding = ListId::new();
    let bike = ListId::new();

    ws.set_list(node, wedding).unwrap();
    assert_eq!(ws.read(node).unwrap().list_id(), Some(wedding));

    ws.set_list(node, bike).unwrap();
    let read = ws.read(node).unwrap();
    assert_eq!(read.list_id(), Some(bike));
    assert_eq!(
        read.content
            .attrs()
            .iter()
            .filter(|a| a.role == KEY_LIST)
            .count(),
        1,
        "one membership, replaced in place"
    );

    assert!(ws.clear_list(node).unwrap());
    assert_eq!(ws.read(node).unwrap().list_id(), None);
    assert!(!ws.clear_list(node).unwrap(), "clearing twice is a no-op");
}

/// A membership is a `ListId`, never a name: the list table owns names, and a
/// stored name would have to be rewritten by every rename.
#[test]
fn a_list_value_must_be_a_list_id() {
    let (mut ws, node) = one_node();
    let refused = ws.set_attribute(node, KEY_LIST, AttrValue::Text("Wedding".into()));
    assert!(matches!(
        refused,
        Err(StoreError::AttributeRejected { ref key, .. }) if key == KEY_LIST
    ));
    assert_eq!(ws.read(node).unwrap().list_id(), None);

    ws.set_attribute(node, KEY_LIST, AttrValue::Text(ListId::new().to_storage()))
        .unwrap();
    assert!(ws.read(node).unwrap().list_id().is_some());
}

/// **A list change routes like a tag change** (SPEC §9): the ordinary
/// attribute door, no command, and therefore no task activity this release.
#[test]
fn a_list_change_takes_the_ordinary_door() {
    let (ws, node) = one_node();
    assert!(!is_command_only_key(KEY_LIST));
    assert!(!COMMAND_ONLY_KEYS.contains(&KEY_LIST));
    assert_eq!(
        crate::tasks::route_attribute(&ws, node, KEY_LIST).unwrap(),
        crate::tasks::AttributeRoute::Ordinary,
    );
}

/// **Deleting a list clears memberships and keeps every task** (SPEC §8). The
/// registry tombstones the row; this sweep is the document's half, and it is
/// one undo step over however many tasks were in the list.
#[test]
fn deleting_a_list_clears_memberships_and_keeps_the_tasks() {
    use crate::system::SystemDoc;

    let mut sys = SystemDoc::create().unwrap();
    let wedding = sys
        .create_list_under(None, "Wedding", Some("pink"), 1)
        .unwrap()
        .id();
    let bike = sys
        .create_list_under(None, "Bike Project", Some("blue"), 2)
        .unwrap()
        .id();

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "lists").unwrap();
    let cake = ws
        .insert(Parent::Root, Node::task("Order the cake").unwrap())
        .unwrap();
    let venue = ws
        .insert(Parent::Root, Node::task("Book the venue").unwrap())
        .unwrap();
    let groupset = ws
        .insert(Parent::Root, Node::task("Order the groupset").unwrap())
        .unwrap();
    ws.set_list(cake, wedding).unwrap();
    ws.set_list(venue, wedding).unwrap();
    ws.set_list(groupset, bike).unwrap();
    ws.enable_undo();

    assert!(sys.delete_list(wedding, 3).unwrap());
    let listed: Vec<ListId> = sys.lists().into_iter().map(|l| l.id).collect();
    assert_eq!(listed, vec![bike], "the row leaves the rail at once");
    assert!(
        sys.list_entry(wedding).unwrap().deleted_ms.is_some(),
        "and says it was deleted rather than never existing"
    );

    assert_eq!(ws.clear_list_membership(wedding).unwrap(), 2);
    for node in [cake, venue] {
        let read = ws.read(node).unwrap();
        assert_eq!(read.list_id(), None, "the membership went");
        assert!(
            read.content.as_text().is_some_and(|t| !t.is_empty()),
            "and the task did not"
        );
    }
    assert_eq!(
        ws.read(groupset).unwrap().list_id(),
        Some(bike),
        "another list's memberships are untouched"
    );
    assert!(ws.undo().unwrap());
    assert_eq!(
        ws.read(cake).unwrap().list_id(),
        Some(wedding),
        "the whole sweep was one undo step"
    );
}

/// A document holding none of the deleted list's memberships writes nothing —
/// the sweep runs per open document and must be free where it has no work.
#[test]
fn a_sweep_with_nothing_to_clear_leaves_no_undo_step() {
    let (mut ws, node) = one_node();
    ws.set_list(node, ListId::new()).unwrap();
    ws.enable_undo();
    assert_eq!(ws.clear_list_membership(ListId::new()).unwrap(), 0);
    assert!(!ws.can_undo(), "no op, no phantom undo step");
}

// ------------------------------------------------------------ node readers

#[test]
fn readers_answer_from_attrs_without_touching_text() {
    let (mut ws, node) = one_node();
    let tag = TagId::new();
    ws.add_tag(node, tag).unwrap();
    ws.set_attribute(node, KEY_COLOR, AttrValue::Text("teal".into()))
        .unwrap();
    ws.set_attribute(node, KEY_PRIORITY, AttrValue::Text("high".into()))
        .unwrap();

    let read = ws.read(node).unwrap();
    assert_eq!(read.tag_ids().collect::<Vec<_>>(), vec![tag]);
    assert_eq!(read.color_token(), Some("teal"));
    assert_eq!(read.priority(), Some(Priority::High));
    assert_eq!(read.content.as_text(), Some("Reward flow"));
}

/// `due()` prefers the registry key and still answers documents written with
/// the pre-registry bare `"due"` role.
#[test]
fn due_reads_the_registry_key_and_the_legacy_role() {
    let (mut ws, node) = one_node();
    ws.set_attr(node, &Attr::new("due", date("2026-08-30")))
        .unwrap();
    assert_eq!(
        ws.read(node).unwrap().due(),
        Some(on("2026-08-30")),
        "legacy role still answers"
    );

    ws.set_attribute(node, KEY_DUE, date("2026-09-15")).unwrap();
    assert_eq!(
        ws.read(node).unwrap().due(),
        Some(on("2026-09-15")),
        "the registry key wins when both exist"
    );
}

/// **The Q5 sharp edge, resolved.** A timed due used to answer `None` from
/// `due()` — indistinguishable from *no due at all*, so a caller that wanted a
/// day silently dropped it. Now `due()` answers the value, and the refusal to
/// name a calendar day has its own name and its own method: an instant's day
/// depends on where it is read, and this crate carries no timezone database.
#[test]
fn a_timed_due_is_reachable_and_never_flattened_to_a_utc_day() {
    let (mut ws, node) = one_node();
    let value = zoned(1_790_000_000_000, "America/New_York");
    ws.set_attribute(node, KEY_DUE, value.clone()).unwrap();
    let read = ws.read(node).unwrap();

    assert_eq!(
        read.due(),
        value.temporal(),
        "the due is the value, not a day it might be somewhere"
    );
    assert!(read.due().is_some(), "and it is emphatically not absent");
    assert_eq!(
        read.due_date_only(),
        None,
        "which day this is depends on where you read it"
    );

    // A node with no due at all is the *other* `None`, and the two are now
    // distinguishable — which is the whole point.
    let (bare_ws, bare) = one_node();
    assert_eq!(bare_ws.read(bare).unwrap().due(), None);
}

// ------------------------------------------------------------ multi-node

fn many_nodes(n: usize) -> (WorkspaceDoc, Vec<NodeId>) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "attrs").unwrap();
    let nodes = (0..n)
        .map(|i| {
            ws.insert(Parent::Root, Node::task(&format!("row {i}")).unwrap())
                .unwrap()
        })
        .collect();
    (ws, nodes)
}

/// The multi-select apply: one gesture, one undo step, however many nodes.
#[test]
fn a_multi_node_tag_apply_is_one_undo_step() {
    let (mut ws, nodes) = many_nodes(3);
    ws.enable_undo();
    let tag = TagId::new();
    ws.checkpoint();
    assert!(ws.add_tag_to_all(&nodes, tag).unwrap());
    for &n in &nodes {
        assert!(ws.read(n).unwrap().tag_ids().any(|t| t == tag));
    }
    assert!(ws.undo().unwrap());
    for &n in &nodes {
        assert!(
            !ws.read(n).unwrap().tag_ids().any(|t| t == tag),
            "one Ctrl+Z takes the whole apply back"
        );
    }
}

/// Nodes already carrying the tag are skipped; all of them carrying it is a
/// guarded no-op — no op, no phantom undo step.
#[test]
fn a_multi_node_apply_skips_what_is_already_there() {
    let (mut ws, nodes) = many_nodes(2);
    let tag = TagId::new();
    ws.add_tag(nodes[0], tag).unwrap();
    assert!(ws.add_tag_to_all(&nodes, tag).unwrap());
    assert_eq!(
        ws.read(nodes[0])
            .unwrap()
            .tag_ids()
            .filter(|t| *t == tag)
            .count(),
        1,
        "no duplicate on the node that had it"
    );
    assert!(!ws.add_tag_to_all(&nodes, tag).unwrap(), "guarded no-op");
    assert!(ws.remove_tag_from_all(&nodes, tag).unwrap());
    assert!(!ws.remove_tag_from_all(&nodes, tag).unwrap());
}

#[test]
fn a_multi_node_color_set_and_clear_are_single_steps() {
    let (mut ws, nodes) = many_nodes(3);
    ws.enable_undo();
    ws.checkpoint();
    assert!(
        ws.set_attribute_on_all(&nodes, KEY_COLOR, AttrValue::Text("teal".into()))
            .unwrap()
    );
    let color_of = |ws: &WorkspaceDoc, n: NodeId| {
        ws.read(n).unwrap().content.attrs().iter().find_map(|a| {
            (a.role == KEY_COLOR)
                .then(|| a.as_text().map(str::to_owned))
                .flatten()
        })
    };
    for &n in &nodes {
        assert_eq!(color_of(&ws, n).as_deref(), Some("teal"));
    }
    // Same value again: nothing to do.
    assert!(
        !ws.set_attribute_on_all(&nodes, KEY_COLOR, AttrValue::Text("teal".into()))
            .unwrap()
    );
    assert!(ws.undo().unwrap());
    for &n in &nodes {
        assert_eq!(color_of(&ws, n), None, "one step for all three");
    }
    // A validated failure still validates: an unknown token is refused.
    assert!(
        ws.set_attribute_on_all(&nodes, KEY_COLOR, AttrValue::Text("chartreuse".into()))
            .is_err()
    );
    assert!(
        !ws.clear_attribute_on_all(&nodes, KEY_COLOR).unwrap(),
        "clearing an absent key across nodes is the guarded no-op"
    );
}

/// `edit_group` bundles a text edit and an attribute op into one undo step —
/// what the editor does with the `/tag` trigger text.
#[test]
fn edit_group_makes_a_span_delete_and_an_apply_one_step() {
    let (mut ws, node) = one_node();
    ws.enable_undo();
    let tag = TagId::new();
    let len = ws.text_len(node).unwrap();
    ws.checkpoint();
    ws.edit_group(|ws| {
        ws.delete_text(node, 0..len)?;
        ws.add_tag_to_all(&[node], tag)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(ws.text_len(node).unwrap(), 0);
    assert!(ws.read(node).unwrap().tag_ids().any(|t| t == tag));
    assert!(ws.undo().unwrap());
    assert_eq!(ws.text_len(node).unwrap(), len, "the text came back");
    assert!(
        !ws.read(node).unwrap().tag_ids().any(|t| t == tag),
        "and the tag went with it — one step"
    );
}
