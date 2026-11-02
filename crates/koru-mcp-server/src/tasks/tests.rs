// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;

use super::*;
use crate::{AttrContext, EditOperation, InlineContext, InsertKind, apply_ops, attr_json};
use koru_core::attributes::{KEY_DUE, KEY_PRIORITY, KEY_TASK, KEY_TASK_STATE};
use koru_core::tasks::golden::{self, Fixture};
use koru_core::{Attr, AttrValue, ListStyle, Node, NodeId, Parent, WorkspaceDoc, WorkspaceId};

// ------------------------------------------------------------ the golden run

/// The whole table, through **this host's wrapper**.
///
/// [`golden::Command::apply_to`] is the closure body the wrapper takes, so this
/// exercises the wrapper for attribution and scheduling and cannot quietly run
/// something else. Everything the wrapper adds is invisible to
/// [`golden::verify`] by design: none of it may change a domain answer.
#[test]
fn the_golden_cases_agree_through_this_hosts_wrapper() {
    for case in golden::CASES {
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let mut wrapper = Wrapper::new();
        let got = wrapper.run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target));
        if let Err(complaint) = golden::verify(case, &fx, &got) {
            panic!("{complaint}");
        }
    }
}

/// Every attempt is the outcome's own intent, in order, attributed to the
/// agent and carrying the **command's** causation id — the wrapper mints none
/// of its own, which is what makes a cascade group identically here and on the
/// desktop.
#[test]
fn an_attempt_is_the_outcomes_intent_attributed_to_the_agent() {
    for case in golden::CASES {
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let mut wrapper = Wrapper::new();
        let Ok(Some(outcome)) =
            wrapper.run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target))
        else {
            continue;
        };
        assert_eq!(
            wrapper.attempts().len(),
            outcome.activity.len(),
            "{}: one attempt per intent, no more and no fewer",
            case.id
        );
        for (attempt, intent) in wrapper.attempts().iter().zip(&outcome.activity) {
            assert_eq!(attempt.event, intent.event(), "{}", case.id);
            assert_eq!(
                attempt.subject,
                TaskSubject::of(intent.subject()),
                "{}",
                case.id
            );
            assert_eq!(attempt.actor, ACTOR, "{}", case.id);
            assert_eq!(
                attempt.correlation,
                Some(outcome.causation.0),
                "{}",
                case.id
            );
        }
    }
}

/// The attrs of every row ADR-273's table declares, keyed by event name. A row
/// whose keys drifted from the registry fails here rather than at the journal.
const ROW_ATTRS: &[(&str, &[&str])] = &[
    ("task.created", &["reason", "node_kind"]),
    ("task.removed", &[]),
    ("task.completed", &["reason"]),
    ("task.reopened", &["reason"]),
    ("task.due.changed", &["change", "temporal_kind"]),
    // `priority` is optional: absence means removed, never `low`.
    ("task.priority.changed", &["change"]),
    ("task.child.added", &["child", "child_class", "reopened"]),
    ("task.child.moved", &["child", "child_class", "reopened"]),
    ("task.child.removed", &["child"]),
    ("task.child.completed", &["child", "child_is_task"]),
    (
        "task.child.reopened",
        &["child", "child_is_task", "reopened_parent"],
    ),
];

#[test]
fn every_attempt_carries_its_registry_rows_attrs() {
    for case in golden::CASES {
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let mut wrapper = Wrapper::new();
        let _ = wrapper.run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target));
        for attempt in wrapper.attempts() {
            let (_, required) = ROW_ATTRS
                .iter()
                .find(|(event, _)| *event == attempt.event)
                .unwrap_or_else(|| panic!("{}: {} is not a declared row", case.id, attempt.event));
            let keys: Vec<&str> = attempt.attrs.iter().map(|(key, _)| *key).collect();
            for key in *required {
                assert!(
                    keys.contains(key),
                    "{}: {} must carry {key}, carried {keys:?}",
                    case.id,
                    attempt.event
                );
            }
        }
    }
}

/// One row spelled out, so the tokens are pinned rather than merely present.
#[test]
fn a_due_that_creates_a_task_maps_onto_two_rows_and_names_no_value() {
    let case = golden::case("set_due.creates_the_task").expect("the case is in the table");
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let mut wrapper = Wrapper::new();
    wrapper
        .run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target))
        .expect("a due on a plain row applies");

    let attempts = wrapper.attempts();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].event, "task.created");
    assert_eq!(
        attempts[0].attrs,
        vec![
            ("reason", AttrToken::Token("due")),
            ("node_kind", AttrToken::Token("paragraph")),
        ]
    );
    assert_eq!(attempts[1].event, "task.due.changed");
    assert_eq!(
        attempts[1].attrs,
        vec![
            ("change", AttrToken::Token("set")),
            // The *shape* the deadline now has, never the deadline.
            ("temporal_kind", AttrToken::Token("date")),
        ]
    );
    assert_eq!(attempts[1].subject.node, target.as_uuid());
}

// ------------------------------------------------------- the `apply_ops` arms

fn context() -> InlineContext {
    InlineContext {
        profile: koru_core::ProfileId::new(),
        workspace: WorkspaceId::new(),
        documents: HashMap::new(),
        files: HashMap::new(),
        attrs: AttrContext::default(),
    }
}

/// A value in the wire spelling a client sends, built from the one a read
/// reports — the contract's own promise that a value taken off one node can be
/// written onto another.
fn wire(value: &AttrValue) -> crate::AttributeValue {
    let json = attr_json(&Attr::new(KEY_DUE.to_owned(), value.clone()));
    serde_json::from_value(json["value"].clone()).expect("a read value is a writable value")
}

fn text_of(ws: &WorkspaceDoc, node: NodeId) -> String {
    ws.read(node)
        .expect("the node reads")
        .content
        .as_text()
        .unwrap_or_default()
        .to_owned()
}

fn set_attribute(
    ws: &WorkspaceDoc,
    node: NodeId,
    key: &str,
    value: crate::AttributeValue,
) -> EditOperation {
    EditOperation::SetAttribute {
        node_id: node.to_storage(),
        expected: text_of(ws, node),
        key: key.to_owned(),
        value,
    }
}

fn clear_attribute(ws: &WorkspaceDoc, node: NodeId, key: &str) -> EditOperation {
    EditOperation::ClearAttribute {
        node_id: node.to_storage(),
        expected: text_of(ws, node),
        key: key.to_owned(),
    }
}

fn set_done(ws: &WorkspaceDoc, node: NodeId, done: bool) -> EditOperation {
    EditOperation::SetDone {
        node_id: node.to_storage(),
        expected: text_of(ws, node),
        done,
    }
}

/// **Route-through.** The wire contract is unchanged — a `set_attribute` of
/// `koru.due` — and the routing is internal, so the op has to reach the golden
/// case's answer: the marker normalized in, the outcome, and both intents.
#[test]
fn a_koru_due_set_attribute_reaches_the_set_due_cases_answer() {
    for id in [
        "set_due.creates_the_task",
        "set_due.moves_an_existing_deadline",
        "set_due.zoned_is_carried_as_its_shape",
    ] {
        let case = golden::case(id).expect("the case is in the table");
        let golden::Command::SetDue { due, .. } = case.command else {
            panic!("{id} is not a set_due case");
        };
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let op = set_attribute(
            &fx.ws,
            target,
            KEY_DUE,
            wire(&AttrValue::from(due.temporal())),
        );

        let wrapper = apply_ops(&mut fx.ws, &[op], &context()).expect("the due applies");
        let got = Ok(wrapper.outcomes().first().cloned());
        if let Err(complaint) = golden::verify(case, &fx, &got) {
            panic!("{complaint}");
        }
    }
}

/// The same for clearing: `clear_due` marks first, so the op must not be able
/// to take the task away with the deadline.
#[test]
fn a_koru_due_clear_attribute_reaches_the_clear_due_cases_answer() {
    for id in ["clear_due.keeps_the_task", "clear_due.no_due_is_a_no_op"] {
        let case = golden::case(id).expect("the case is in the table");
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let op = clear_attribute(&fx.ws, target, KEY_DUE);

        let wrapper = apply_ops(&mut fx.ws, &[op], &context()).expect("the clear applies");
        let got = Ok(wrapper.outcomes().first().cloned());
        if let Err(complaint) = golden::verify(case, &fx, &got) {
            panic!("{complaint}");
        }
    }
}

/// **Row 9 of ADR-272's reconciliation table.** `set_done` on a task is the
/// lifecycle, children or not: the same command, the same cascade, the same
/// mirror as an explicit Complete — only the attribution differs.
#[test]
fn set_done_on_a_task_is_the_lifecycle_command_cascade_and_all() {
    for id in [
        "complete.childless",
        "complete.cascades_over_checkables_and_nested_tasks",
    ] {
        let case = golden::case(id).expect("the case is in the table");
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let op = set_done(&fx.ws, target, true);

        let wrapper = apply_ops(&mut fx.ws, &[op], &context()).expect("the completion applies");
        let got = Ok(wrapper.outcomes().first().cloned());
        if let Err(complaint) = golden::verify(case, &fx, &got) {
            panic!("{complaint}");
        }
    }
}

#[test]
fn set_done_false_on_a_task_reopens_it_and_never_cascades_downward() {
    let case = golden::case("reopen.never_cascades_downward").expect("the case is in the table");
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let op = set_done(&fx.ws, target, false);

    let wrapper = apply_ops(&mut fx.ws, &[op], &context()).expect("the reopen applies");
    let got = Ok(wrapper.outcomes().first().cloned());
    if let Err(complaint) = golden::verify(case, &fx, &got) {
        panic!("{complaint}");
    }
}

/// **Row 10.** A plain checklist item is the ordinary checkbox door: the box
/// moves and the node gains no task, no marker and no lifecycle.
#[test]
fn set_done_on_a_plain_checklist_item_is_unchanged_and_task_free() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("List").expect("a document"))
        .expect("the root inserts");
    let list = ws
        .insert(Parent::Node(root), Node::list_block().expect("a list"))
        .expect("the list inserts");
    ws.set_list_style(list, Some(ListStyle::Checkbox))
        .expect("a checkbox list");
    let item = ws
        .insert(
            Parent::Node(list),
            Node::paragraph("Milk").expect("an item"),
        )
        .expect("the item inserts");

    let ops = [set_done(&ws, item, true)];
    let wrapper = apply_ops(&mut ws, &ops, &context()).expect("checking a checklist item applies");

    let read = ws.read(item).expect("the item reads");
    assert!(read.meta.done, "the box moved");
    assert!(
        read.content.attrs().is_empty(),
        "a checklist item gained an attribute: {:?}",
        read.content.attrs()
    );
    assert!(
        !koru_core::tasks::is_task_node(&ws, item).expect("the predicate reads"),
        "the plain checkbox path must never create a task",
    );
    assert!(
        wrapper.is_empty(),
        "a plain checkbox produced task activity: {:?}",
        wrapper.attempts()
    );
}

/// SPEC §4's exclusion, surfaced as an MCP error with the refusal **token** —
/// `task_rejected` / `kind_not_capable`, not a sentence a client has to parse.
#[test]
fn a_due_on_an_excluded_kind_is_refused_as_task_rejected() {
    let case = golden::case("set_due.excluded_kind_is_refused").expect("the case is in the table");
    let golden::Command::SetDue { due, .. } = case.command else {
        panic!("the case is a set_due");
    };
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let before = fx.fingerprint();

    let ops = [set_attribute(
        &fx.ws,
        target,
        KEY_DUE,
        wire(&AttrValue::from(due.temporal())),
    )];
    let error =
        apply_ops(&mut fx.ws, &ops, &context()).expect_err("a code block cannot carry a deadline");

    assert_eq!(error.code, "task_rejected");
    assert_eq!(
        error.details.as_ref().expect("details")["why"],
        "kind_not_capable"
    );
    assert_eq!(fx.fingerprint(), before, "a refusal wrote something");
}

/// Clearing is guarded by the same rule as setting: one kind cannot carry a
/// deadline by whichever door was not looked at.
#[test]
fn clearing_a_due_on_an_excluded_kind_is_refused_too() {
    let case = golden::case("set_due.excluded_kind_is_refused").expect("the case is in the table");
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let before = fx.fingerprint();

    let ops = [clear_attribute(&fx.ws, target, KEY_DUE)];
    let error =
        apply_ops(&mut fx.ws, &ops, &context()).expect_err("a code block cannot carry a deadline");

    assert_eq!(error.code, "task_rejected");
    assert_eq!(
        error.details.as_ref().expect("details")["why"],
        "kind_not_capable"
    );
    assert_eq!(fx.fingerprint(), before, "a refusal wrote something");
}

/// The two command-only keys are refused by the **model**, at the generic
/// door, and this layer does not repeat the refusal — it lets the model's own
/// words reach the wire. Pinned so a future routing change cannot open a
/// second authority on task state.
#[test]
fn the_command_only_keys_are_refused_at_the_generic_doors() {
    for key in [KEY_TASK, KEY_TASK_STATE] {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
        let root = ws
            .insert(Parent::Root, Node::document("Doc").expect("a document"))
            .expect("the root inserts");
        let row = ws
            .insert(
                Parent::Node(root),
                Node::paragraph("Ship it").expect("a row"),
            )
            .expect("the row inserts");

        let ops = [set_attribute(
            &ws,
            row,
            key,
            crate::AttributeValue::Text {
                value: "explicit".to_owned(),
            },
        )];
        let set = apply_ops(&mut ws, &ops, &context())
            .expect_err("a command-only key is not a generic door's to write");
        assert_eq!(set.code, "attribute_rejected");
        assert_eq!(set.details.as_ref().expect("details")["key"], key);

        let ops = [clear_attribute(&ws, row, key)];
        let cleared = apply_ops(&mut ws, &ops, &context()).expect_err("nor to delete");
        assert_eq!(cleared.code, "attribute_rejected");
        assert_eq!(cleared.details.as_ref().expect("details")["key"], key);

        assert!(
            ws.read(row)
                .expect("the row reads")
                .content
                .attrs()
                .is_empty(),
            "a refused key wrote a value",
        );
    }
}

/// An ordinary attribute is untouched by any of this: it takes the same door
/// it always did and produces no task activity.
#[test]
fn an_ordinary_attribute_still_takes_the_ordinary_door() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("Doc").expect("a document"))
        .expect("the root inserts");
    let row = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Ship it").expect("a row"),
        )
        .expect("the row inserts");

    let ops = [set_attribute(
        &ws,
        row,
        KEY_PRIORITY,
        crate::AttributeValue::Text {
            value: "high".to_owned(),
        },
    )];
    let wrapper = apply_ops(&mut ws, &ops, &context()).expect("a priority applies");

    assert_eq!(
        ws.read(row).expect("the row reads").content.attrs().len(),
        1,
        "the priority landed",
    );
    assert!(
        !koru_core::tasks::is_task_node(&ws, row).expect("the predicate reads"),
        "setting a priority never creates a task (SPEC §1)",
    );
    assert!(
        wrapper.is_empty(),
        "an ordinary attribute is not task activity"
    );
}

/// A key nothing defines stays a misspelling — the definition is resolved
/// before anything is routed, so the routing cannot turn an unknown key into
/// something it recognizes.
#[test]
fn an_unknown_key_is_still_a_misspelling() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("Doc").expect("a document"))
        .expect("the root inserts");
    let row = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Ship it").expect("a row"),
        )
        .expect("the row inserts");

    let ops = [set_attribute(
        &ws,
        row,
        "due",
        crate::AttributeValue::Date {
            value: "2026-08-30".to_owned(),
        },
    )];
    let error = apply_ops(&mut ws, &ops, &context())
        .expect_err("the legacy bare role is not a key a client may write");
    assert_eq!(error.code, "unknown_attribute");
}

// ------------------------------------------------------------------ the sink

#[derive(Default)]
struct Recording {
    events: Vec<&'static str>,
    finished: usize,
}

impl ActivitySink for Recording {
    fn append(&mut self, attempt: &ActivityAttempt) {
        self.events.push(attempt.event);
    }

    fn finish(&mut self, appended: usize) {
        self.finished += 1;
        assert_eq!(appended, self.events.len());
    }
}

/// Delivery is **per call**, not per attempt: MCP's burst is the batch, and a
/// sink is told once however many tasks the batch touched.
#[test]
fn a_batch_delivers_once_and_carries_every_attempt() {
    let case = golden::case("complete.cascades_over_checkables_and_nested_tasks")
        .expect("the case is in the table");
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let ops = [set_done(&fx.ws, target, true)];
    let wrapper = apply_ops(&mut fx.ws, &ops, &context()).expect("the completion applies");

    let mut sink = Recording::default();
    wrapper.deliver(&mut sink);

    assert_eq!(sink.events, vec!["task.completed", "task.completed"]);
    assert_eq!(sink.finished, 1);
}

#[test]
fn every_attempt_is_attributed_to_the_agent_and_to_nothing_else() {
    assert_eq!(ACTOR, "mcp");
}

// ====================================================== T-2 · emission coverage
//
// `ROW_ATTRS` above says which keys a row must *carry*. What follows is the
// whole of ADR-273's table — every key, its shape, and its closed vocabulary —
// and the assertions are exact rather than "contains": a row that gained a key,
// lost one, or spelled a token differently fails here.
//
// This crate links no events crate ([ADR-286] does not let it acquire one to
// say something was ticked), so the registry cannot be iterated from inside it.
// The table below is therefore hand-transcribed and its *twelfth* row is the
// tell: `koru-workspace-host`'s `every_registry_row_has_a_producer_and_every
// _producer_a_row` walks the real registry, so a thirteenth event breaks that
// test and a mapping deleted here breaks this one.

/// One attr as ADR-273 declares it.
struct RowAttr {
    key: &'static str,
    shape: Shape,
    /// The closed vocabulary, for a `Token`. Empty for the other two shapes.
    vocabulary: &'static [&'static str],
    /// `priority` is the only one, and absence means removed, never `low`.
    optional: bool,
}

#[derive(PartialEq, Eq, Debug)]
enum Shape {
    Token,
    Id,
    Flag,
}

const fn tok(key: &'static str, vocabulary: &'static [&'static str]) -> RowAttr {
    RowAttr {
        key,
        shape: Shape::Token,
        vocabulary,
        optional: false,
    }
}

const fn id(key: &'static str) -> RowAttr {
    RowAttr {
        key,
        shape: Shape::Id,
        vocabulary: &[],
        optional: false,
    }
}

const fn flag(key: &'static str) -> RowAttr {
    RowAttr {
        key,
        shape: Shape::Flag,
        vocabulary: &[],
        optional: false,
    }
}

const CHILD_CLASS: &[&str] = &["unfinished_structural", "finished_structural", "passive"];
const CHANGE: &[&str] = &["set", "changed", "cleared"];

/// **ADR-273's twelve**, in the order the ADR prints them.
const ROWS: &[(&str, &[RowAttr])] = &[
    (
        "task.created",
        &[
            tok("reason", &["explicit", "due", "tasks_view"]),
            tok(
                "node_kind",
                &["paragraph", "task", "document", "image", "file"],
            ),
        ],
    ),
    ("task.removed", &[]),
    (
        "task.completed",
        &[tok(
            "reason",
            &[
                "explicit_complete",
                "checkbox_completed",
                "cascade_from_ancestor",
            ],
        )],
    ),
    (
        "task.reopened",
        &[tok(
            "reason",
            &[
                "explicit_reopen",
                "checkbox_reopened",
                "unfinished_child_added",
            ],
        )],
    ),
    (
        "task.due.changed",
        &[
            tok("change", CHANGE),
            // The *shape* the deadline now has, never its value — and never
            // `datetime_floating`, which `set_due` refuses.
            tok("temporal_kind", &["date", "datetime_zoned", "none"]),
        ],
    ),
    (
        "task.priority.changed",
        &[
            tok("change", CHANGE),
            RowAttr {
                optional: true,
                ..tok("priority", &["low", "medium", "high", "urgent"])
            },
        ],
    ),
    (
        "task.child.added",
        &[
            id("child"),
            tok("child_class", CHILD_CLASS),
            flag("reopened"),
        ],
    ),
    (
        "task.child.moved",
        &[
            id("child"),
            tok("child_class", CHILD_CLASS),
            flag("reopened"),
        ],
    ),
    ("task.child.removed", &[id("child")]),
    (
        "task.child.completed",
        &[id("child"), flag("child_is_task")],
    ),
    (
        "task.child.reopened",
        &[id("child"), flag("child_is_task"), flag("reopened_parent")],
    ),
    // No attrs at all — the text is what it must never carry.
    ("task.content.changed", &[]),
];

fn row(event: &str) -> &'static [RowAttr] {
    ROWS.iter()
        .find(|(name, _)| *name == event)
        .unwrap_or_else(|| panic!("{event} is not one of ADR-273's twelve"))
        .1
}

/// **ADR-273's table, spelled from the domain's own enums.**
///
/// The golden case says what a command owes in labels and closed tokens; this
/// turns one of those into the attrs an attempt must carry. It is deliberately
/// a second source: [`attrs_of`] renders the same fact out of the wrapper's
/// constants, so a token that drifts on one side and not the other is a red
/// test rather than a renamed value in the journal.
fn render(
    activity: &golden::Activity,
    fx: &Fixture,
) -> (&'static str, Vec<(&'static str, AttrToken)>) {
    use golden::Detail;
    let child_of = |label: &str| AttrToken::Id(fx.node(label).as_uuid());
    let attrs = match activity.detail {
        Detail::Created { reason, node_kind } => vec![
            ("reason", AttrToken::Token(reason.as_str())),
            ("node_kind", AttrToken::Token(node_kind.as_str())),
        ],
        Detail::Removed => Vec::new(),
        Detail::Completed { reason } | Detail::Reopened { reason } => {
            vec![("reason", AttrToken::Token(reason.as_str()))]
        }
        Detail::DueChanged {
            change,
            temporal_kind,
        } => vec![
            ("change", AttrToken::Token(change.as_str())),
            (
                "temporal_kind",
                AttrToken::Token(temporal_kind.map_or("none", TemporalKind::token)),
            ),
        ],
        Detail::PriorityChanged { change, priority } => {
            let mut attrs = vec![("change", AttrToken::Token(change.as_str()))];
            if let Some(priority) = priority {
                attrs.push(("priority", AttrToken::Token(priority.as_str())));
            }
            attrs
        }
        Detail::ChildAdded {
            child,
            class,
            reopened,
        }
        | Detail::ChildMoved {
            child,
            class,
            reopened,
        } => vec![
            ("child", child_of(child)),
            ("child_class", AttrToken::Token(class.as_str())),
            ("reopened", AttrToken::Flag(reopened)),
        ],
        Detail::ChildRemoved { child } => vec![("child", child_of(child))],
        Detail::ChildCompleted {
            child,
            child_is_task,
        } => vec![
            ("child", child_of(child)),
            ("child_is_task", AttrToken::Flag(child_is_task)),
        ],
        Detail::ChildReopened {
            child,
            child_is_task,
            reopened_parent,
        } => vec![
            ("child", child_of(child)),
            ("child_is_task", AttrToken::Flag(child_is_task)),
            ("reopened_parent", AttrToken::Flag(reopened_parent)),
        ],
    };
    (activity.event, attrs)
}

/// One golden case, driven through this host's wrapper, with everything it
/// would append kept.
fn attempted(case: &golden::Case) -> (Fixture, Vec<ActivityAttempt>) {
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let mut wrapper = Wrapper::new();
    let _ = wrapper.run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target));
    let attempts = wrapper.attempts();
    (fx, attempts)
}

/// **Every command's Activity, at this host, is the row ADR-273 declares** —
/// name, keys and tokens, per golden case. `&[]` is an assertion too: a refusal
/// and a no-op append nothing at all.
#[test]
fn every_golden_case_attempts_the_rows_adr_249_declares() {
    let mut bad: Vec<String> = Vec::new();
    for case in golden::CASES {
        let (fx, attempts) = attempted(case);
        let want: Vec<(&'static str, Vec<(&'static str, AttrToken)>)> = match &case.expect {
            golden::Expect::Applied(applied) => applied
                .activity
                .iter()
                .map(|activity| render(activity, &fx))
                .collect(),
            golden::Expect::Refused(_) | golden::Expect::Nothing => Vec::new(),
        };
        let got: Vec<(&'static str, Vec<(&'static str, AttrToken)>)> = attempts
            .iter()
            .map(|attempt| (attempt.event, attempt.attrs.clone()))
            .collect();
        if got != want {
            bad.push(format!(
                "{}:\n    want {want:#?}\n    got  {got:#?}",
                case.id
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} golden cases attempt something other than ADR-273's row:\n\n{}",
        bad.len(),
        golden::CASES.len(),
        bad.join("\n\n")
    );
}

/// The same output, checked against the table's *shapes and vocabularies*
/// rather than against a case: no undeclared key, every required key present,
/// in declaration order, and every token from its closed set.
#[test]
fn every_attempted_attr_is_declared_and_its_token_is_in_the_vocabulary() {
    for case in golden::CASES {
        let (_fx, attempts) = attempted(case);
        for attempt in &attempts {
            let declared = row(attempt.event);
            let keys: Vec<&str> = attempt.attrs.iter().map(|(key, _)| *key).collect();
            let expected: Vec<&str> = declared
                .iter()
                .filter(|attr| !attr.optional || keys.contains(&attr.key))
                .map(|attr| attr.key)
                .collect();
            assert_eq!(
                keys, expected,
                "{}: {} carried the wrong keys",
                case.id, attempt.event
            );
            for (key, value) in &attempt.attrs {
                let attr = declared
                    .iter()
                    .find(|attr| attr.key == *key)
                    .unwrap_or_else(|| {
                        panic!("{}: {} carries undeclared {key}", case.id, attempt.event)
                    });
                match value {
                    AttrToken::Token(got) => {
                        assert_eq!(attr.shape, Shape::Token, "{}: {key}", case.id);
                        assert!(
                            attr.vocabulary.contains(got),
                            "{}: {}.{key} = {got:?} is not in {:?}",
                            case.id,
                            attempt.event,
                            attr.vocabulary
                        );
                    }
                    AttrToken::Id(_) => assert_eq!(attr.shape, Shape::Id, "{}: {key}", case.id),
                    AttrToken::Flag(_) => assert_eq!(attr.shape, Shape::Flag, "{}: {key}", case.id),
                }
            }
        }
    }
}

// --------------------------------------------------------------- attribution

/// **Attribution is the agent, on every attempt this host can make**, and there
/// are three producers rather than one: a command's outcome, the priority door's
/// standalone intent, and the call's coalesced content tail. The desktop emits
/// `Actor::user()` for all three; that difference is the whole of the
/// difference between the hosts (ADR-273).
#[test]
fn all_three_producers_attribute_to_the_agent() {
    let case = golden::case("complete.cascades_over_checkables_and_nested_tasks")
        .expect("the case is in the table");
    let mut fx = Fixture::build(case);
    let target = fx.node(case.command.target());
    let mut wrapper = Wrapper::new();

    // 1. a command's outcome.
    let _ = wrapper.run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target));
    // 2. the priority door, which belongs to no command.
    wrapper.note_intent(ActivityIntent::PriorityChanged {
        task: koru_core::tasks::refs::task_ref(&fx.ws, target).expect("a workspace"),
        change: AttrChange::Set,
        priority: Some(Priority::High),
    });
    // 3. the call's content tail.
    wrapper
        .note_content_changed(&fx.ws, target)
        .expect("the read succeeds");

    let attempts = wrapper.attempts();
    assert!(
        attempts.len() >= 4,
        "the three producers between them attempted {}",
        attempts.len()
    );
    assert!(
        attempts.iter().any(|a| a.event == "task.content.changed"),
        "the content tail is in the batch"
    );
    assert!(
        attempts.iter().any(|a| a.event == "task.priority.changed"),
        "the door's intent is in the batch"
    );
    for attempt in &attempts {
        assert_eq!(attempt.actor, ACTOR, "{}", attempt.event);
        assert_eq!(attempt.actor, "mcp", "and never a user");
    }
}

// ----------------------------------------------------------- cascade causation

/// **A cascade is a set of attempts sharing one chain root**, here as on the
/// desktop: the wrapper mints no causation id of its own for a command, so the
/// grouping is identical at both hosts and T-1's query is one `WHERE` either
/// way.
#[test]
fn every_cascading_case_attempts_under_one_chain_root() {
    for case_id in [
        "complete.cascades_over_checkables_and_nested_tasks",
        "complete.tells_the_task_above_it",
        "reopen.reopens_every_completed_task_above_it",
        "reclassify.unfinished_child_reopens_the_task",
    ] {
        let case = golden::case(case_id).expect("the case is in the table");
        let mut fx = Fixture::build(case);
        let target = fx.node(case.command.target());
        let mut wrapper = Wrapper::new();
        let outcome = wrapper
            .run_optional(&mut fx.ws, |ws| case.command.apply_to(ws, target))
            .expect("the command applies")
            .expect("an outcome");

        let attempts = wrapper.attempts();
        assert!(
            attempts.len() >= 2,
            "{case_id} is a cascade case and attempted {}",
            attempts.len()
        );
        for attempt in &attempts {
            assert_eq!(
                attempt.correlation,
                Some(outcome.causation.0),
                "{case_id}: {} left the chain",
                attempt.event
            );
        }
    }
}

/// A *child* event and the event it companions travel under one root — the
/// parent's side and the child's side of one fact.
#[test]
fn a_child_attempt_shares_the_root_with_the_attempt_it_companions() {
    let case = golden::case("complete.tells_the_task_above_it").expect("the case is in the table");
    let (_fx, attempts) = attempted(case);
    let events: Vec<&str> = attempts.iter().map(|attempt| attempt.event).collect();
    assert_eq!(events, vec!["task.completed", "task.child.completed"]);
    assert_eq!(attempts[0].correlation, attempts[1].correlation);
    assert!(attempts[0].correlation.is_some());
    // Two different tasks, one chain: the grouping is not two copies of one row.
    assert_ne!(attempts[0].subject.node, attempts[1].subject.node);
}

// ------------------------------------------------------ the mapping drift net

/// One intent of every shape, so the mapping can be walked rather than reasoned
/// about. A thirteenth `ActivityIntent` is already a compile error in
/// [`attrs_of`]'s exhaustive `match`; this covers the other direction — a row
/// nothing produces, and a producer no row declares.
fn one_intent_of_each_shape(task: EntityRef, child: NodeId) -> Vec<ActivityIntent> {
    vec![
        ActivityIntent::Created {
            task,
            reason: TaskReason::Explicit,
            node_kind: koru_core::Kind::Paragraph,
        },
        ActivityIntent::Removed { task },
        ActivityIntent::Completed {
            task,
            reason: koru_core::tasks::LifecycleReason::ExplicitComplete,
        },
        ActivityIntent::Reopened {
            task,
            reason: koru_core::tasks::LifecycleReason::ExplicitReopen,
        },
        ActivityIntent::DueChanged {
            task,
            change: AttrChange::Set,
            temporal_kind: Some(TemporalKind::Date),
        },
        ActivityIntent::PriorityChanged {
            task,
            change: AttrChange::Set,
            // Set rather than cleared, so the one optional key is present.
            priority: Some(Priority::High),
        },
        ActivityIntent::ChildAdded {
            task,
            child,
            class: ChildClass::UnfinishedStructural,
            reopened: true,
        },
        ActivityIntent::ChildMoved {
            task,
            child,
            class: ChildClass::Passive,
            reopened: false,
        },
        ActivityIntent::ChildRemoved { task, child },
        ActivityIntent::ChildCompleted {
            task,
            child,
            child_is_task: true,
        },
        ActivityIntent::ChildReopened {
            task,
            child,
            child_is_task: false,
            reopened_parent: true,
        },
    ]
}

/// **Every row a producer, every producer a row.** Eleven come from an
/// [`ActivityIntent`]; the twelfth, `task.content.changed`, belongs to no
/// command and to **both** hosts — this wrapper mints it once per
/// `edit_document` call, the desktop's once per editing burst.
#[test]
fn every_adr_249_row_has_a_producer_at_this_host_or_the_other() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let node = ws
        .insert(Parent::Root, Node::paragraph("Ship it").expect("a row"))
        .expect("the row inserts");
    koru_core::tasks::ensure_task(&mut ws, node, TaskReason::Explicit).expect("a task");
    let task = koru_core::tasks::refs::task_ref(&ws, node).expect("a workspace");

    let mut wrapper = Wrapper::new();
    for intent in one_intent_of_each_shape(task, NodeId::new()) {
        let event = intent.event();
        wrapper.note_intent(intent);
        // Every intent's keys are the row's, in declaration order.
        let attempt = wrapper.attempts().pop().expect("an attempt");
        assert_eq!(attempt.event, event, "the wrapper renamed an intent");
        let declared: Vec<&str> = row(event).iter().map(|attr| attr.key).collect();
        let keys: Vec<&str> = attempt.attrs.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, declared, "{event}");
    }
    wrapper
        .note_content_changed(&ws, node)
        .expect("the read succeeds");

    let mut produced: Vec<&str> = wrapper
        .attempts()
        .iter()
        .map(|attempt| attempt.event)
        .collect();
    produced.sort_unstable();
    produced.dedup();
    let mut declared: Vec<&str> = ROWS.iter().map(|(event, _)| *event).collect();
    declared.sort_unstable();
    assert_eq!(
        produced, declared,
        "this host and ADR-273's table disagree about which task events exist"
    );
    assert_eq!(
        ROWS.len(),
        12,
        "ADR-273 declares twelve task events. A thirteenth needs a row here, a \
         producer, and a registry entry (pinned in koru-workspace-host)."
    );
}

/// **Never the text, structurally.** [`AttrToken`] has no free-string variant,
/// so the only thing that could carry content is a `Token` outside its closed
/// vocabulary — and no row declares a key that takes prose. This walks every
/// attr of every row and every attempt the table can produce.
#[test]
fn no_row_can_carry_a_free_string() {
    for (event, attrs) in ROWS {
        for attr in *attrs {
            match attr.shape {
                Shape::Token => assert!(
                    !attr.vocabulary.is_empty(),
                    "{event}.{} is a token with no closed vocabulary",
                    attr.key
                ),
                Shape::Id | Shape::Flag => assert!(
                    attr.vocabulary.is_empty(),
                    "{event}.{} declares a vocabulary it cannot use",
                    attr.key
                ),
            }
        }
        if *event == "task.content.changed" {
            assert!(
                attrs.is_empty(),
                "the coalesced row declares attrs; the text is the one thing it \
                 must never carry"
            );
        }
    }
}

// ---------------------------------------------------------------- negative space
//
// SPEC §9 and ADR-273: tags, color, custom attributes and list membership
// produce **no** task activity this release. Each door below is driven through
// `apply_ops` on a node that *is* a task, which is the case where a mistaken
// emission would look plausible.

/// A document with one row that is already a task, and the context to edit it.
fn a_task_row() -> (WorkspaceDoc, NodeId, InlineContext) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("Doc").expect("a document"))
        .expect("the root inserts");
    let row = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Ship it").expect("a row"),
        )
        .expect("the row inserts");
    koru_core::tasks::ensure_task(&mut ws, row, TaskReason::Explicit).expect("a task");
    (ws, row, context())
}

/// **A tag on a task is not task activity** — neither adding one nor taking it
/// away — and the task is untouched by either.
#[test]
fn tagging_and_untagging_a_task_produces_no_activity() {
    let (mut ws, row, context) = a_task_row();
    let tag = koru_core::ids::TagId::new().to_storage();

    for op in [
        EditOperation::AddTag {
            node_id: row.to_storage(),
            expected: "Ship it".into(),
            tag: None,
            tag_id: Some(tag.clone()),
        },
        EditOperation::RemoveTag {
            node_id: row.to_storage(),
            expected: "Ship it".into(),
            tag: None,
            tag_id: Some(tag.clone()),
        },
    ] {
        let wrapper = apply_ops(&mut ws, &[op], &context).expect("the tag op applies");
        assert!(
            wrapper.is_empty(),
            "a tag produced task activity: {:?}",
            wrapper.attempts()
        );
    }
    assert!(
        koru_core::tasks::is_task_node(&ws, row).expect("the predicate reads"),
        "and none of it took the task away"
    );
}

/// **A color on a task is not task activity.** Presentation, never meaning —
/// nothing may infer "red = urgent", and nothing records that somebody chose
/// one.
#[test]
fn coloring_a_task_produces_no_activity() {
    let (mut ws, row, context) = a_task_row();
    let ops = [set_attribute(
        &ws,
        row,
        koru_core::attributes::KEY_COLOR,
        crate::AttributeValue::Text {
            value: "teal".to_owned(),
        },
    )];
    let wrapper = apply_ops(&mut ws, &ops, &context).expect("the color applies");
    assert!(
        wrapper.is_empty(),
        "a color produced task activity: {:?}",
        wrapper.attempts()
    );
    assert_eq!(
        ws.read(row).expect("the row reads").content.attrs().len(),
        2,
        "the marker and the color; the write did land",
    );
}

/// **A custom attribute on a task is not task activity either.** The workspace
/// may define whatever keys it likes; none of them is a lifecycle.
#[test]
fn a_custom_attribute_on_a_task_produces_no_activity() {
    let (mut ws, row, mut context) = a_task_row();
    let definition = koru_core::attributes::AttributeDefinition::custom(
        "workspace.owner",
        "Owner",
        koru_core::attributes::ValueType::Text,
    );
    context
        .attrs
        .definitions
        .insert(definition.key.clone(), definition);

    let ops = [set_attribute(
        &ws,
        row,
        "workspace.owner",
        crate::AttributeValue::Text {
            value: "me".to_owned(),
        },
    )];
    let wrapper = apply_ops(&mut ws, &ops, &context).expect("the custom attribute applies");
    assert!(
        wrapper.is_empty(),
        "a custom attribute produced task activity: {:?}",
        wrapper.attempts()
    );
}

/// **List membership is not task activity** (D-4). A list is a filter, not a
/// place: `koru.list` is an ordinary key that takes the generic door, carries no
/// lifecycle invariant, and emits nothing — exactly like a tag.
#[test]
fn putting_a_task_in_a_list_produces_no_activity() {
    let (mut ws, row, context) = a_task_row();
    let list = koru_core::ids::ListId::new().to_storage();

    let ops = [set_attribute(
        &ws,
        row,
        koru_core::attributes::KEY_LIST,
        crate::AttributeValue::Text {
            value: list.clone(),
        },
    )];
    let wrapper = apply_ops(&mut ws, &ops, &context).expect("the membership applies");
    assert!(
        wrapper.is_empty(),
        "a list membership produced task activity: {:?}",
        wrapper.attempts()
    );

    let ops = [clear_attribute(&ws, row, koru_core::attributes::KEY_LIST)];
    let wrapper = apply_ops(&mut ws, &ops, &context).expect("the removal applies");
    assert!(
        wrapper.is_empty(),
        "leaving a list produced task activity: {:?}",
        wrapper.attempts()
    );
    assert!(
        koru_core::tasks::is_task_node(&ws, row).expect("the predicate reads"),
        "and the task survived both"
    );
}

/// All four doors in **one call**, which is the shape that would expose a
/// wrapper accumulating something per op: the batch owes nothing at all, and
/// the content tail is empty because none of them touched the row's text.
#[test]
fn a_batch_of_ordinary_attribute_doors_owes_nothing() {
    let (mut ws, row, mut context) = a_task_row();
    let definition = koru_core::attributes::AttributeDefinition::custom(
        "workspace.owner",
        "Owner",
        koru_core::attributes::ValueType::Text,
    );
    context
        .attrs
        .definitions
        .insert(definition.key.clone(), definition);

    let ops = [
        EditOperation::AddTag {
            node_id: row.to_storage(),
            expected: "Ship it".into(),
            tag: None,
            tag_id: Some(koru_core::ids::TagId::new().to_storage()),
        },
        set_attribute(
            &ws,
            row,
            koru_core::attributes::KEY_COLOR,
            crate::AttributeValue::Text {
                value: "teal".to_owned(),
            },
        ),
        set_attribute(
            &ws,
            row,
            "workspace.owner",
            crate::AttributeValue::Text {
                value: "me".to_owned(),
            },
        ),
        set_attribute(
            &ws,
            row,
            koru_core::attributes::KEY_LIST,
            crate::AttributeValue::Text {
                value: koru_core::ids::ListId::new().to_storage(),
            },
        ),
    ];
    let wrapper = apply_ops(&mut ws, &ops, &context).expect("the batch applies");
    assert!(
        wrapper.attempts().is_empty(),
        "four ordinary doors produced task activity: {:?}",
        wrapper.attempts()
    );
}

// ------------------------------------------------------- departures (T-0c)
//
// `task.child.removed` at this host: the `Remove` and `Move` arms of
// `apply_ops`, which are the only two operations that can take a node out of a
// task. The arm was mapped, shaped and unreachable until T-0c; these are the
// producer's side.

/// A document holding a task with a nested task under it.
/// `(ws, root, task, child)`.
fn a_task_with_a_subtask() -> (WorkspaceDoc, NodeId, NodeId, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("Plan").expect("a document"))
        .expect("the root inserts");
    let task = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Ship it").expect("a row"),
        )
        .expect("the task inserts");
    let child = ws
        .insert(Parent::Node(task), Node::paragraph("Draft").expect("a row"))
        .expect("the child inserts");
    koru_core::tasks::ensure_task(&mut ws, task, TaskReason::Explicit).expect("a task");
    koru_core::tasks::ensure_task(&mut ws, child, TaskReason::Explicit).expect("a subtask");
    (ws, root, task, child)
}

fn remove(node: NodeId, expected: &str) -> EditOperation {
    EditOperation::Remove {
        node_id: node.to_storage(),
        expected: expected.to_owned(),
    }
}

fn move_under(node: NodeId, expected: &str, parent: NodeId) -> EditOperation {
    EditOperation::Move {
        node_id: node.to_storage(),
        expected: expected.to_owned(),
        parent_id: parent.to_storage(),
        after_node_id: None,
    }
}

/// The one row a departure produces, or a panic naming what came instead.
fn only_departure(wrapper: &Wrapper) -> ActivityAttempt {
    let attempts = wrapper.attempts();
    assert_eq!(
        attempts.len(),
        1,
        "exactly the departure was expected: {attempts:?}"
    );
    assert_eq!(attempts[0].event, "task.child.removed");
    attempts[0].clone()
}

/// **Removing a structural child tells the task it lost one**, filed under the
/// task and carrying the child's id — the row's single attr.
#[test]
fn removing_a_structural_child_reports_the_departure() {
    let (mut ws, _root, task, child) = a_task_with_a_subtask();

    let wrapper = apply_ops(&mut ws, &[remove(child, "Draft")], &context()).expect("it removes");

    let attempt = only_departure(&wrapper);
    assert_eq!(attempt.subject.node, task.as_uuid());
    assert_eq!(attempt.attrs.len(), 1);
    assert_eq!(attempt.attrs[0].0, "child");
    assert!(
        attempt.correlation.is_some(),
        "its own chain root, like the priority door's"
    );
}

/// **Moving a structural child out of a task reports the departure**, and moving
/// it within one reports nothing: the recorded task is still an ancestor.
#[test]
fn moving_a_structural_child_out_of_a_task_reports_the_departure() {
    let (mut ws, root, task, child) = a_task_with_a_subtask();

    let stayed = apply_ops(&mut ws, &[move_under(child, "Draft", task)], &context())
        .expect("a move within the task applies");
    assert!(
        stayed.is_empty(),
        "a reorder inside one task owes nothing: {:?}",
        stayed.attempts()
    );

    let left = apply_ops(&mut ws, &[move_under(child, "Draft", root)], &context())
        .expect("a move out applies");
    let attempt = only_departure(&left);
    assert_eq!(attempt.subject.node, task.as_uuid());
}

/// **Passive content owes nothing**, at this host as at the desktop — the
/// reading T-0c took from ADR-273's row, applied through `classify_child` so
/// the arrival and departure sides cannot disagree about what a child is.
#[test]
fn removing_passive_content_from_a_task_reports_nothing() {
    let (mut ws, _root, task, _child) = a_task_with_a_subtask();
    let note = ws
        .insert(
            Parent::Node(task),
            Node::paragraph("a note").expect("a row"),
        )
        .expect("the note inserts");

    let wrapper = apply_ops(&mut ws, &[remove(note, "a note")], &context()).expect("it removes");
    assert!(
        wrapper.is_empty(),
        "a note under a task is filing, not work: {:?}",
        wrapper.attempts()
    );
}

/// **A plain row owes nothing.** No task above it, so no task lost anything —
/// and removing a whole task subtree is one departure for the task itself,
/// never one per node inside it.
#[test]
fn removing_a_row_under_no_task_reports_nothing() {
    let (mut ws, root, _task, _child) = a_task_with_a_subtask();
    let loose = ws
        .insert(Parent::Node(root), Node::paragraph("loose").expect("a row"))
        .expect("the row inserts");

    let wrapper = apply_ops(&mut ws, &[remove(loose, "loose")], &context()).expect("it removes");
    assert!(
        wrapper.is_empty(),
        "nothing above it to tell: {:?}",
        wrapper.attempts()
    );
}

/// A whole task removed from under another is **one** departure, not one per
/// descendant: a node inside the deleted subtree loses a task ancestor that is
/// going with it, and an event filed under a task that no longer exists is
/// noise rather than history.
#[test]
fn removing_a_task_subtree_is_one_departure() {
    let (mut ws, _root, task, child) = a_task_with_a_subtask();
    ws.insert(
        Parent::Node(child),
        Node::paragraph("deeper").expect("a row"),
    )
    .expect("the grandchild inserts");

    let wrapper = apply_ops(&mut ws, &[remove(child, "Draft")], &context()).expect("it removes");
    let attempt = only_departure(&wrapper);
    assert_eq!(attempt.subject.node, task.as_uuid());
}

// ------------------------------------------------------- arrivals (D-2b)
//
// The other half of the same gesture, and the half T-0c reported missing: the
// `Move` arm reported the departure and never asked `reclassify_moved`, so an
// MCP move that put unfinished work under a completed task did not reopen it
// while the identical desktop drag did. SPEC §2 says both surfaces run the same
// commands, so these mirror `editor.rs`'s drop matrix case for case.

/// A document holding a **completed** task and a loose row beside it.
/// `(ws, root, done, loose)`.
///
/// The completion is stood up through the faithful-import door rather than
/// `complete_task`, for the reason the desktop's matrix gives: completing would
/// cascade over the very children these cases are about.
fn a_completed_task_and_a_loose_row(loose: Node) -> (WorkspaceDoc, NodeId, NodeId, NodeId) {
    use koru_core::attributes::{Lifecycle, TaskReason};
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("Plan").expect("a document"))
        .expect("the root inserts");
    let done = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Ship it").expect("a row"),
        )
        .expect("the task inserts");
    koru_core::tasks::ensure_task(&mut ws, done, TaskReason::Explicit).expect("a task");
    koru_core::tasks::faithful_import::restore_task(
        &mut ws,
        done,
        &koru_core::tasks::TaskFields {
            marker: Some(TaskReason::Explicit),
            due: None,
            lifecycle: Lifecycle::Completed,
            done: false,
            pinned_today: false,
            completed_at: None,
        },
    )
    .expect("it stands completed");
    let loose = ws
        .insert(Parent::Node(root), loose)
        .expect("the loose row inserts");
    (ws, root, done, loose)
}

fn lifecycle(ws: &WorkspaceDoc, node: NodeId) -> koru_core::attributes::Lifecycle {
    koru_core::tasks::TaskFields::of(&ws.read(node).expect("the node reads")).lifecycle
}

/// **An MCP move of unfinished work under a completed task reopens it**, says
/// why, and tells it as a *move* — the same three facts the desktop's drop
/// asserts, produced by the same domain function (SPEC §2, SPEC §5).
#[test]
fn an_mcp_move_of_unfinished_work_under_a_completed_task_reopens_it() {
    use koru_core::attributes::{Lifecycle, TaskReason};
    let (mut ws, _root, done, loose) =
        a_completed_task_and_a_loose_row(Node::paragraph("draft the summary").expect("a row"));
    koru_core::tasks::ensure_task(&mut ws, loose, TaskReason::Explicit).expect("an open task");
    assert_eq!(lifecycle(&ws, done), Lifecycle::Completed);

    let wrapper = apply_ops(
        &mut ws,
        &[move_under(loose, "draft the summary", done)],
        &context(),
    )
    .expect("the move applies");

    assert!(
        matches!(ws.parent(loose), Ok(Parent::Node(p)) if p == done),
        "it landed inside the completed task"
    );
    assert_eq!(
        lifecycle(&ws, done),
        Lifecycle::Open,
        "which reopened it (SPEC §5)"
    );

    let attempts = wrapper.attempts();
    assert_eq!(
        attempts.len(),
        2,
        "the reopen and the arrival, and nothing left a task: {attempts:?}"
    );
    assert_eq!(attempts[0].event, "task.reopened");
    assert_eq!(attempts[0].subject.node, done.as_uuid());
    assert_eq!(
        attempts[0].attrs,
        vec![("reason", AttrToken::Token("unfinished_child_added"))]
    );
    assert_eq!(attempts[1].event, "task.child.moved");
    assert_eq!(attempts[1].subject.node, done.as_uuid());
    assert_eq!(
        attempts[1].attrs,
        vec![
            ("child", AttrToken::Id(loose.as_uuid())),
            ("child_class", AttrToken::Token("unfinished_structural")),
            ("reopened", AttrToken::Flag(true)),
        ]
    );
    assert_eq!(
        attempts[0].correlation, attempts[1].correlation,
        "one command, one chain root"
    );
    assert!(attempts[0].correlation.is_some());
}

/// **A move between two tasks owes both halves**, each under its own subject:
/// the arrival at the task it landed in, the departure at the one it left. Two
/// chain roots, because MCP's unit is the op and this op ran one command and
/// noted one intent that belongs to none (ADR-273).
#[test]
fn an_mcp_move_between_two_tasks_owes_the_arrival_and_the_departure() {
    use koru_core::attributes::{Lifecycle, TaskReason};
    let (mut ws, root, done, _loose) =
        a_completed_task_and_a_loose_row(Node::paragraph("spare").expect("a row"));
    let source = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Plan it").expect("a row"),
        )
        .expect("the source task inserts");
    koru_core::tasks::ensure_task(&mut ws, source, TaskReason::Explicit).expect("an open task");
    let child = ws
        .insert(
            Parent::Node(source),
            Node::paragraph("Draft").expect("a row"),
        )
        .expect("the child inserts");
    koru_core::tasks::ensure_task(&mut ws, child, TaskReason::Explicit).expect("an open subtask");

    let wrapper =
        apply_ops(&mut ws, &[move_under(child, "Draft", done)], &context()).expect("it moves");

    assert_eq!(
        lifecycle(&ws, done),
        Lifecycle::Open,
        "the arrival reopened"
    );
    let attempts = wrapper.attempts();
    let events: Vec<&str> = attempts.iter().map(|a| a.event).collect();
    assert_eq!(
        events,
        vec!["task.reopened", "task.child.moved", "task.child.removed"],
        "arrival first, then what it left: {attempts:?}"
    );
    assert_eq!(attempts[1].subject.node, done.as_uuid());
    assert_eq!(attempts[2].subject.node, source.as_uuid());
    assert_eq!(
        attempts[2].attrs,
        vec![("child", AttrToken::Id(child.as_uuid()))]
    );
    assert_ne!(
        attempts[1].correlation, attempts[2].correlation,
        "the departure belongs to no command, so it is its own chain root"
    );
}

/// **An already-completed child reopens nothing** — structural, and finished:
/// `classify_child`'s second rule, asked here through the same call the desktop
/// makes.
#[test]
fn an_mcp_move_of_a_completed_child_reopens_nothing() {
    use koru_core::attributes::{Lifecycle, TaskReason};
    let (mut ws, _root, done, loose) =
        a_completed_task_and_a_loose_row(Node::paragraph("already done").expect("a row"));
    koru_core::tasks::ensure_task(&mut ws, loose, TaskReason::Explicit).expect("a task");
    koru_core::tasks::faithful_import::restore_task(
        &mut ws,
        loose,
        &koru_core::tasks::TaskFields {
            marker: Some(TaskReason::Explicit),
            due: None,
            lifecycle: Lifecycle::Completed,
            done: false,
            pinned_today: false,
            completed_at: None,
        },
    )
    .expect("it stands completed");

    let wrapper = apply_ops(
        &mut ws,
        &[move_under(loose, "already done", done)],
        &context(),
    )
    .expect("it moves");

    assert_eq!(
        lifecycle(&ws, done),
        Lifecycle::Completed,
        "it stayed completed"
    );
    assert!(
        wrapper.is_empty(),
        "and owed nothing at all: {:?}",
        wrapper.attempts()
    );
}

/// **Passive content reopens nothing.** A note is filing, not work — the same
/// reading the departure side takes, through the same `classify_child`.
#[test]
fn an_mcp_move_of_passive_content_reopens_nothing() {
    use koru_core::attributes::Lifecycle;
    let (mut ws, _root, done, loose) =
        a_completed_task_and_a_loose_row(Node::paragraph("for the record").expect("a row"));

    let wrapper = apply_ops(
        &mut ws,
        &[move_under(loose, "for the record", done)],
        &context(),
    )
    .expect("it moves");

    assert!(matches!(ws.parent(loose), Ok(Parent::Node(p)) if p == done));
    assert_eq!(lifecycle(&ws, done), Lifecycle::Completed);
    assert!(
        wrapper.is_empty(),
        "a note under a finished task is filing: {:?}",
        wrapper.attempts()
    );
}

/// **A reorder inside one open task owes nothing**, arrival side included:
/// there is no completed ancestor, so `reclassify_moved` opens no group and
/// mints no causation. This is every ordinary move, and the case the hook has
/// to stay cheap in.
#[test]
fn an_mcp_move_within_one_open_task_owes_nothing() {
    let (mut ws, _root, task, child) = a_task_with_a_subtask();
    let sibling = ws
        .insert(
            Parent::Node(task),
            Node::paragraph("Review").expect("a row"),
        )
        .expect("the sibling inserts");

    let wrapper = apply_ops(
        &mut ws,
        &[EditOperation::Move {
            node_id: child.to_storage(),
            expected: "Draft".to_owned(),
            parent_id: task.to_storage(),
            after_node_id: Some(sibling.to_storage()),
        }],
        &context(),
    )
    .expect("it moves");

    assert!(
        wrapper.is_empty(),
        "a reorder inside one task owes nothing: {:?}",
        wrapper.attempts()
    );
}

// ------------------------------------------------------- creations (D-2c)
//
// The arm D-2b parked as "a separate question nobody has asked yet". A node
// that has just been inserted was nowhere before, so there is no departure
// half — only the arrival, and it is `task.child.added` rather than `.moved`.

/// A **completed** task holding a checkbox list whose one item is checked, plus
/// the list's own id. `(ws, task, list)`.
///
/// Completion is stood up through the faithful-import door, the same reason the
/// move fixture above gives: `complete_task` would cascade over the very child
/// this shape is about.
fn a_completed_task_over_a_finished_list() -> (WorkspaceDoc, NodeId, NodeId) {
    use koru_core::attributes::{Lifecycle, TaskReason};
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").expect("a workspace");
    let root = ws
        .insert(Parent::Root, Node::document("Plan").expect("a document"))
        .expect("the root inserts");
    let task = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Ship it").expect("a row"),
        )
        .expect("the task inserts");
    let item = ws
        .insert(
            Parent::Node(task),
            Node::paragraph("Step one").expect("a row"),
        )
        .expect("the item inserts");
    ws.wrap_in_list_block_many(&[item], koru_core::ListStyle::Checkbox)
        .expect("a checkbox list");
    let list = match ws.parent(item) {
        Ok(Parent::Node(list)) => list,
        other => panic!("the item should sit in a list block, got {other:?}"),
    };
    ws.set_done(item, true).expect("the step is finished");
    koru_core::tasks::ensure_task(&mut ws, task, TaskReason::Explicit).expect("a task");
    koru_core::tasks::faithful_import::restore_task(
        &mut ws,
        task,
        &koru_core::tasks::TaskFields {
            marker: Some(TaskReason::Explicit),
            due: None,
            lifecycle: Lifecycle::Completed,
            done: false,
            pinned_today: false,
            completed_at: None,
        },
    )
    .expect("it stands completed");
    (ws, task, list)
}

fn insert_under(node: NodeId, parent: NodeId, kind: InsertKind, text: &str) -> EditOperation {
    EditOperation::Insert {
        node_id: Some(node.to_storage()),
        parent_id: parent.to_storage(),
        after_node_id: None,
        kind,
        text: text.to_owned(),
    }
}

/// **An MCP insert of an unchecked item under a completed task reopens it**,
/// with the reason SPEC §5 names and filed as an *add*.
///
/// The same three facts the editor's Enter asserts, produced by the same domain
/// function — which is the whole of SPEC §2's same-commands invariant, and the
/// sort of gap that is invisible until the two surfaces are compared.
#[test]
fn an_mcp_insert_of_an_unchecked_item_under_a_completed_task_reopens_it() {
    use koru_core::attributes::Lifecycle;
    let (mut ws, task, list) = a_completed_task_over_a_finished_list();
    let fresh = NodeId::new();
    assert_eq!(lifecycle(&ws, task), Lifecycle::Completed);

    let wrapper = apply_ops(
        &mut ws,
        &[insert_under(fresh, list, InsertKind::Paragraph, "Step two")],
        &context(),
    )
    .expect("the insert applies");

    assert!(
        matches!(ws.parent(fresh), Ok(Parent::Node(p)) if p == list),
        "it landed in the completed task's checklist"
    );
    assert!(
        !ws.read(fresh).expect("it reads").meta.done,
        "and a fresh item is never checked"
    );
    assert_eq!(
        lifecycle(&ws, task),
        Lifecycle::Open,
        "which reopened the task (SPEC §5)"
    );

    let attempts = wrapper.attempts();
    assert_eq!(
        attempts.len(),
        2,
        "the reopen and the arrival, and nothing else: {attempts:?}"
    );
    assert_eq!(attempts[0].event, "task.reopened");
    assert_eq!(attempts[0].subject.node, task.as_uuid());
    assert_eq!(
        attempts[0].attrs,
        vec![("reason", AttrToken::Token("unfinished_child_added"))]
    );
    assert_eq!(attempts[1].event, "task.child.added");
    assert_eq!(attempts[1].subject.node, task.as_uuid());
    assert_eq!(
        attempts[1].attrs,
        vec![
            ("child", AttrToken::Id(fresh.as_uuid())),
            ("child_class", AttrToken::Token("unfinished_structural")),
            ("reopened", AttrToken::Flag(true)),
        ]
    );
}

/// **Passive content inserted into a completed task reopens nothing.** A
/// paragraph under the task itself is a note, not work — SPEC §5's fourth rule,
/// through the same `classify_child` the move side reads.
#[test]
fn an_mcp_insert_of_passive_content_reopens_nothing() {
    use koru_core::attributes::Lifecycle;
    let (mut ws, task, _list) = a_completed_task_over_a_finished_list();

    let wrapper = apply_ops(
        &mut ws,
        &[insert_under(
            NodeId::new(),
            task,
            InsertKind::Paragraph,
            "for the record",
        )],
        &context(),
    )
    .expect("the insert applies");

    assert_eq!(lifecycle(&ws, task), Lifecycle::Completed);
    assert!(
        wrapper.is_empty(),
        "a note under a finished task is filing: {:?}",
        wrapper.attempts()
    );
}

/// **An ordinary insert owes nothing at all** — no completed ancestor, so the
/// classification opens no group and mints no causation. This is every agent
/// edit, and the case the hook has to stay cheap in.
#[test]
fn an_mcp_insert_with_no_completed_task_above_it_owes_nothing() {
    let (mut ws, _root, task, _child) = a_task_with_a_subtask();

    let wrapper = apply_ops(
        &mut ws,
        &[insert_under(
            NodeId::new(),
            task,
            InsertKind::Paragraph,
            "Review",
        )],
        &context(),
    )
    .expect("the insert applies");

    assert!(
        wrapper.is_empty(),
        "an ordinary insert owes nothing: {:?}",
        wrapper.attempts()
    );
}
