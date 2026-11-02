// SPDX-License-Identifier: MPL-2.0

use super::*;

const PARENT: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const KEPT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

fn edits(value: Value) -> Vec<EditOperation> {
    serde_json::from_value(value).expect("edit operations")
}

fn seed(value: Value) -> Vec<InitialOperation> {
    serde_json::from_value(value).expect("seed operations")
}

/// The batch as it would cross the wire after resolution.
fn wire<T: serde::Serialize>(operations: &[T]) -> Value {
    serde_json::to_value(operations).unwrap()
}

fn is_uuid(value: &Value) -> bool {
    value.as_str().and_then(NodeId::parse).is_some()
}

#[test]
fn an_insert_without_an_id_gets_one_keyed_by_its_index() {
    let mut operations = edits(json!([
        {"op": "replace_text", "node_id": KEPT, "expected": "a", "text": "b"},
        {"op": "insert", "parent_id": PARENT, "kind": "paragraph", "text": "new"},
    ]));
    let created = resolve_edit(&mut operations).unwrap();
    assert_eq!(created.keys().collect::<Vec<_>>(), ["1"]);
    assert!(is_uuid(&created["1"]), "{created:?}");
    let sent = wire(&operations);
    assert_eq!(sent[1]["node_id"], created["1"], "the batch carries it");
    assert_eq!(sent[0]["node_id"], KEPT);
}

#[test]
fn local_names_reach_later_parents_siblings_and_targets() {
    let mut operations = edits(json!([
        {"op": "insert", "node_id": "list", "parent_id": PARENT, "kind": "listblock", "text": ""},
        {"op": "insert", "node_id": "one", "parent_id": "list", "kind": "paragraph", "text": "One"},
        {"op": "insert", "node_id": "two", "parent_id": "list", "after_node_id": "one",
         "kind": "paragraph", "text": "Two"},
        {"op": "set_mark", "node_id": "two", "expected": "Two", "start_utf8": 0, "end_utf8": 3,
         "mark": "bold", "on": true},
        {"op": "move", "node_id": "one", "expected": "One", "parent_id": "list", "after_node_id": "two"},
    ]));
    let created = resolve_edit(&mut operations).unwrap();
    assert_eq!(created.len(), 3);
    let sent = wire(&operations);
    assert_eq!(sent[0]["node_id"], created["list"]);
    assert_eq!(sent[1]["node_id"], created["one"]);
    assert_eq!(sent[1]["parent_id"], created["list"]);
    assert_eq!(sent[2]["after_node_id"], created["one"]);
    assert_eq!(sent[3]["node_id"], created["two"]);
    assert_eq!(sent[4]["node_id"], created["one"]);
    assert_eq!(sent[4]["after_node_id"], created["two"]);
    let distinct: std::collections::HashSet<_> =
        created.values().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(distinct.len(), 3, "one UUID per name");
    assert!(created.values().all(is_uuid));
}

#[test]
fn real_uuids_pass_through_exactly_beside_names_and_omitted_ids() {
    let upper = KEPT.to_uppercase();
    let mut operations = edits(json!([
        {"op": "insert", "node_id": upper, "parent_id": PARENT, "kind": "paragraph", "text": "mine"},
        {"op": "insert", "node_id": "theirs", "parent_id": PARENT, "after_node_id": upper,
         "kind": "paragraph", "text": "named"},
        {"op": "insert", "parent_id": PARENT, "after_node_id": "theirs", "kind": "paragraph",
         "text": "unnamed"},
    ]));
    let before = wire(&operations);
    let created = resolve_edit(&mut operations).unwrap();
    let sent = wire(&operations);
    assert_eq!(
        sent[0], before[0],
        "a caller's UUID is untouched, spelling and all"
    );
    assert_eq!(sent[1]["after_node_id"], upper);
    assert_eq!(sent[2]["after_node_id"], created["theirs"]);
    assert_eq!(sent[2]["node_id"], created["2"]);
    assert_eq!(created.len(), 2, "{created:?}");

    // A batch of UUIDs only is the request it always was.
    let mut plain = edits(json!([
        {"op": "insert", "node_id": KEPT, "parent_id": PARENT, "kind": "paragraph", "text": "x"},
    ]));
    let plain_before = wire(&plain);
    assert!(resolve_edit(&mut plain).unwrap().is_empty());
    assert_eq!(wire(&plain), plain_before);
}

#[test]
fn names_that_cannot_be_resolved_are_refused_with_the_field() {
    let refused = |value: Value| {
        let mut operations = edits(value);
        resolve_edit(&mut operations).expect_err("refused")
    };

    let unknown = refused(json!([
        {"op": "insert", "parent_id": "nowhere", "kind": "paragraph", "text": "x"},
    ]));
    assert_eq!(unknown.code, "invalid_request");
    assert!(
        unknown.message.contains("operations[0].parent_id")
            && unknown.message.contains("\"nowhere\"")
            && unknown.message.contains("neither a UUID nor a local name"),
        "{}",
        unknown.message
    );
    assert_eq!(unknown.details.as_ref().unwrap()["local_name"], "nowhere");

    let twice = refused(json!([
        {"op": "insert", "node_id": "a", "parent_id": PARENT, "kind": "paragraph", "text": "x"},
        {"op": "insert", "node_id": "a", "parent_id": PARENT, "kind": "paragraph", "text": "y"},
    ]));
    assert!(
        twice.message.contains("defined twice")
            && twice.message.contains("operations[0].node_id")
            && twice.message.contains("operations[1].node_id"),
        "{}",
        twice.message
    );

    let early = refused(json!([
        {"op": "insert", "node_id": "child", "parent_id": "box", "kind": "paragraph", "text": "x"},
        {"op": "insert", "node_id": "box", "parent_id": PARENT, "kind": "callout", "text": ""},
    ]));
    assert!(
        early
            .message
            .contains("before operations[1].node_id defines it"),
        "{}",
        early.message
    );

    let own = refused(json!([
        {"op": "insert", "node_id": "self", "parent_id": "self", "kind": "paragraph", "text": "x"},
    ]));
    assert!(own.message.contains("before"), "{}", own.message);

    let digits = refused(json!([
        {"op": "insert", "node_id": "7", "parent_id": PARENT, "kind": "paragraph", "text": "x"},
    ]));
    assert!(digits.message.contains("all digits"), "{}", digits.message);

    let broken = refused(json!([
        {"op": "replace_text", "node_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa", "expected": "",
         "text": "x"},
    ]));
    assert!(
        broken.message.contains("looks like a UUID"),
        "a mistyped id is not taken for a name: {}",
        broken.message
    );

    let empty = refused(json!([
        {"op": "insert", "node_id": "", "parent_id": PARENT, "kind": "paragraph", "text": "x"},
    ]));
    assert!(empty.message.contains("is empty"), "{}", empty.message);
}

#[test]
fn an_omitted_root_is_named_root_and_a_named_root_is_its_own_name() {
    let mut root = None;
    let mut operations = seed(json!([
        {"op": "insert", "node_id": "q", "parent_id": "root", "kind": "paragraph", "text": "Why?"},
        {"op": "insert", "parent_id": "root", "after_node_id": "q", "kind": "paragraph",
         "text": "Because."},
        {"op": "set_mark", "node_id": "q", "expected": "Why?", "start_utf8": 0, "end_utf8": 3,
         "mark": "italic", "on": true},
    ]));
    let created = resolve_seed(&mut root, Some(&mut operations)).unwrap();
    let root = root.expect("minted");
    assert_eq!(created["root"], root.as_str());
    let sent = wire(&operations);
    assert_eq!(sent[0]["parent_id"], root.as_str());
    assert_eq!(sent[1]["after_node_id"], created["q"]);
    assert_eq!(sent[1]["node_id"], created["1"]);
    assert_eq!(sent[2]["node_id"], created["q"]);

    let mut named = Some("doc".to_owned());
    let mut operations = seed(json!([
        {"op": "insert", "parent_id": "doc", "kind": "paragraph", "text": "x"},
    ]));
    let created = resolve_seed(&mut named, Some(&mut operations)).unwrap();
    assert_eq!(created["doc"], named.as_deref().unwrap());
    assert!(!created.contains_key("root"));

    let mut given = Some(PARENT.to_owned());
    let mut operations = seed(json!([
        {"op": "insert", "node_id": KEPT, "parent_id": PARENT, "kind": "paragraph", "text": "x"},
    ]));
    assert!(
        resolve_seed(&mut given, Some(&mut operations))
            .unwrap()
            .is_empty()
    );
    assert_eq!(given.as_deref(), Some(PARENT));

    // No seed: a title with one blank paragraph, as it always was.
    let mut none = None;
    assert!(resolve_seed(&mut none, None).unwrap().is_empty());
    assert!(none.is_none());
}

#[test]
fn an_insert_named_root_beside_an_omitted_root_is_defined_twice() {
    let mut root = None;
    let mut operations = seed(json!([
        {"op": "insert", "node_id": "root", "parent_id": "root", "kind": "paragraph", "text": "x"},
    ]));
    let error = resolve_seed(&mut root, Some(&mut operations)).unwrap_err();
    assert!(
        error.message.contains("defined twice") && error.message.contains("left out"),
        "{}",
        error.message
    );
}

#[test]
fn an_image_id_is_minted_when_left_out_and_reported_when_named() {
    let (minted, created) = resolve_single(None).unwrap();
    assert!(NodeId::parse(&minted).is_some());
    assert!(created.is_empty(), "the answer's node_id is the report");

    let (named, created) = resolve_single(Some("photo".into())).unwrap();
    assert_eq!(created["photo"], named.as_str());

    let (kept, created) = resolve_single(Some(KEPT.into())).unwrap();
    assert_eq!(kept, KEPT);
    assert!(created.is_empty());
}
