// SPDX-License-Identifier: MPL-2.0

//! What the desktop wrapper owes a task gesture, and what it must never do.

use std::sync::{Arc, Mutex};

use super::*;
use koru_core::ids::{DocumentId, NodeId, WorkspaceId};
use koru_core::node::Node;
use koru_core::schema::Parent;
use koru_core::tasks;

/// A document with one plain row, and that row's id.
fn one_row() -> (WorkspaceDoc, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Tasks").unwrap();
    let row = ws
        .insert(Parent::Root, Node::paragraph("post the form").unwrap())
        .unwrap();
    (ws, row)
}

fn recorder() -> (TaskHost, Arc<Mutex<Vec<Draft>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    (TaskHost::recording(seen.clone()), seen)
}

/// The context an envelope is finished with is never the wrapper's business —
/// it belongs to whoever publishes — so the tests supply one of their own and
/// assert only on what the wrapper itself put in the draft.
fn envelopes(seen: &Arc<Mutex<Vec<Draft>>>) -> Vec<koru_events::Envelope> {
    let ctx = koru_events::EmitContext::system("test");
    seen.lock()
        .unwrap()
        .iter()
        .map(|draft| draft.clone().into_envelope(&ctx))
        .collect()
}

fn names(seen: &Arc<Mutex<Vec<Draft>>>) -> Vec<String> {
    envelopes(seen)
        .into_iter()
        .map(|envelope| envelope.name.as_str().to_owned())
        .collect()
}

/// **A task gesture performs no disk or network I/O on the event thread**, and
/// the strongest half of that is structural rather than measured: [`run`] is
/// handed a `&mut WorkspaceDoc` — the in-memory CRDT document — and never a
/// `Workspace`, a `Session` or a client, so there is no persistence call
/// available to it even by mistake. The closure it runs is `koru_core::tasks`,
/// whose whole mutation subset is `set_one_attr_raw`, `remove_attr` and
/// `set_done`.
///
/// This is the empirical half. It runs the **shipping** wrapper — the one whose
/// sink is `events_host::emit` — in a process where no store, no gateway and no
/// events thread exists, and the gesture still answers its outcome and leaves
/// the edit in memory. Anything synchronous and remote in that path would hang
/// or fail here instead.
#[test]
fn a_task_gesture_answers_with_no_store_gateway_or_events_thread() {
    let (mut ws, row) = one_row();
    let mut host = TaskHost::new();
    let outcome = host
        .run(&mut ws, |ws| {
            tasks::ensure_task(ws, row, TaskReason::Explicit)
        })
        .expect("the command applies");
    assert_eq!(outcome.effect, tasks::TaskEffect::Created);
    assert!(tasks::is_task_node(&ws, row).unwrap());
}

/// **The outcome comes before the attempt**, which is what "success shows
/// immediately, before anything is acknowledged" means at this level: the sink
/// is asked only after the edit has landed, and what it does with the draft
/// cannot reach back into the answer.
#[test]
fn the_edit_lands_before_the_activity_attempt_is_made() {
    let (mut ws, row) = one_row();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let attempts_at = Arc::new(Mutex::new(Vec::<usize>::new()));
    let mut host = {
        let seen = seen.clone();
        let attempts_at = attempts_at.clone();
        TaskHost {
            sink: Box::new(move |draft| {
                attempts_at.lock().unwrap().push(1);
                seen.lock().unwrap().push(draft);
            }),
        }
    };
    let landed = Arc::new(Mutex::new(false));
    let outcome = {
        let landed = landed.clone();
        host.run(&mut ws, move |ws| {
            let outcome = tasks::ensure_task(ws, row, TaskReason::Explicit)?;
            *landed.lock().unwrap() = true;
            Ok(outcome)
        })
        .expect("the command applies")
    };
    assert!(*landed.lock().unwrap(), "the command ran");
    assert_eq!(
        attempts_at.lock().unwrap().len(),
        1,
        "one attempt, and it happened after the command returned"
    );
    assert!(outcome.changed());
    assert_eq!(names(&seen), vec!["task.created"]);
}

/// The attempt is best effort and is never waited on: a sink that swallows
/// everything leaves the task change exactly as it was (SPEC §9 — a lost
/// diagnostic must never cost somebody their edit).
#[test]
fn a_dropped_attempt_never_costs_the_edit() {
    let (mut ws, row) = one_row();
    let mut host = TaskHost {
        sink: Box::new(|_| {}),
    };
    let outcome = host
        .run(&mut ws, |ws| {
            tasks::ensure_task(ws, row, TaskReason::Explicit)
        })
        .expect("the command applies");
    assert!(outcome.changed());
    assert!(tasks::is_task_node(&ws, row).unwrap());
}

/// A refusal produces no outcome, so there is nothing to attribute and nothing
/// to attempt — a wrapper never invents an event a command did not name.
#[test]
fn a_refusal_leaves_nothing_behind() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Tasks").unwrap();
    let quote = ws
        .insert(Parent::Root, Node::quote("not a task").unwrap())
        .unwrap();
    let (mut host, seen) = recorder();
    let refused = host.run(&mut ws, |ws| {
        tasks::ensure_task(ws, quote, TaskReason::Explicit)
    });
    assert!(refused.is_err(), "a quote cannot carry task state");
    assert!(names(&seen).is_empty());
}

/// **Every event of one command carries the command's causation id as its
/// correlation, and no `causation_id`** — a set sharing one chain root rather
/// than a causal chain among themselves. A cascade is what makes it visible:
/// several events, one id, one actor.
#[test]
fn a_cascade_groups_under_one_correlation_and_is_attributed_to_the_user() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Tasks").unwrap();
    let parent = ws
        .insert(Parent::Root, Node::paragraph("ship it").unwrap())
        .unwrap();
    let child = ws
        .insert(Parent::Node(parent), Node::paragraph("draft").unwrap())
        .unwrap();
    tasks::ensure_task(&mut ws, parent, TaskReason::Explicit).unwrap();
    tasks::ensure_task(&mut ws, child, TaskReason::Explicit).unwrap();

    let (mut host, seen) = recorder();
    let outcome = host
        .run(&mut ws, |ws| {
            tasks::complete_task(
                ws,
                parent,
                LifecycleReason::ExplicitComplete,
                koru_core::time::DateOnly::new(chrono::Local::now().date_naive()),
            )
        })
        .expect("the cascade applies");
    assert!(!outcome.cascaded.is_empty(), "the child was cascaded");

    let published = envelopes(&seen);
    assert!(published.len() >= 2, "one event per completed task");
    for envelope in &published {
        assert_eq!(
            envelope.correlation_id,
            Some(EventId(outcome.causation.0)),
            "every event of one command shares the chain root"
        );
        assert_eq!(envelope.causation_id, None);
        assert_eq!(envelope.actor.kind, koru_events::ActorKind::User);
        assert!(
            envelope.actor.token.is_none(),
            "a desktop actor is a plain user"
        );
    }
}

/// An outcome a *view* produced — a checkbox click, a `/due` — reaches the same
/// wrapper through [`TaskHost::attempt`]. The editor keeps the causation id and
/// the intents; the scheduling and the attribution are still this crate's.
#[test]
fn an_outcome_the_view_produced_is_attempted_the_same_way() {
    let (mut ws, row) = one_row();
    let outcome = tasks::ensure_task(&mut ws, row, TaskReason::Explicit).unwrap();
    let (mut host, seen) = recorder();
    host.attempt(outcome.causation, &outcome.activity);
    let published = envelopes(&seen);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].name.as_str(), "task.created");
    assert_eq!(
        published[0].correlation_id,
        Some(EventId(outcome.causation.0))
    );
}

/// Task activity is filed under the spelling task surfaces speak, in the
/// workspace it happened in.
#[test]
fn the_subject_is_the_task_spelling() {
    let (mut ws, row) = one_row();
    let (mut host, seen) = recorder();
    host.run(&mut ws, |ws| {
        tasks::ensure_task(ws, row, TaskReason::Explicit)
    })
    .unwrap();
    let published = envelopes(&seen);
    let subject = published[0].subject.as_ref().expect("a subject");
    assert!(
        subject.id.starts_with("task:"),
        "filed under the task spelling, got {:?}",
        subject.id
    );
    assert!(published[0].scope.workspace_id.is_some());
}

/// A temporal shape the event vocabulary has no token for is **dropped and
/// counted**, never guessed at — a wrapper may not invent a token any more than
/// it may invent an event.
#[test]
fn an_unnameable_shape_is_dropped_rather_than_guessed() {
    let unnameable = ActivityIntent::DueChanged {
        task: koru_core::EntityRef::task(WorkspaceId::new(), DocumentId::new(), NodeId::new()),
        change: AttrChange::Set,
        temporal_kind: Some(TemporalKind::Floating),
    };
    assert!(draft_for(CausationId::new(), &unnameable).is_none());

    let before = counters().dropped;
    let (mut host, seen) = recorder();
    host.attempt(CausationId::new(), std::slice::from_ref(&unnameable));
    assert!(names(&seen).is_empty());
    assert!(counters().dropped > before);
}

/// The counters are a plain function with no environment read of their own: the
/// debug row that gates them belongs to whoever owns `KORU_TASKS_DEBUG`, and a
/// wrapper reading it would be a second answer to that question.
#[test]
fn the_counters_are_a_line_of_tokens_and_numbers() {
    let line = counters().describe();
    assert!(line.starts_with("tasks commands="));
    assert!(line.contains("activity_attempted="));
    assert!(line.contains("activity_dropped="));
}

// ------------------------------------------------------------ the golden set
//
// The same table `koru-core` runs against the commands and `koru-editor` runs
// through its own seam, replayed through this wrapper. Everything a wrapper
// adds is invisible to `verify` **by design** — none of it may change a domain
// answer — so a red case here means this wrapper drifted, never that the row
// was fiction.

#[test]
fn the_golden_cases_survive_the_wrapper() {
    use koru_core::tasks::golden::{self, CASES, Command};

    let mut bad: Vec<String> = Vec::new();
    for case in CASES {
        let mut fx = golden::Fixture::build(case);
        let target = fx.node(case.command.target());
        let (mut host, _seen) = recorder();
        let got = match case.command {
            // The classification hook is not one of the six and may answer
            // `None`; `run`'s shape is the one ADR-273 fixed, so this arm calls
            // the domain and hands the wrapper what it produced.
            Command::Reclassify { .. } | Command::ReclassifyMoved { .. } => {
                let answer = case.command.apply_to(&mut fx.ws, target);
                if let Ok(Some(outcome)) = &answer {
                    host.attempt(outcome.causation, &outcome.activity);
                }
                answer
            }
            _ => host
                .run(&mut fx.ws, |ws| {
                    case.command
                        .apply_to(ws, target)
                        .map(|answer| answer.expect("a command always answers an outcome"))
                })
                .map(Some),
        };
        if let Err(complaint) = golden::verify(case, &fx, &got) {
            bad.push(complaint);
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} golden cases disagree through the desktop wrapper:\n\n{}",
        bad.len(),
        CASES.len(),
        bad.join("\n\n")
    );
}

// ---------------------------------------------------------- the content burst
//
// `task.content.changed` is the one event with no `ActivityIntent` behind it:
// it belongs to no command, and ADR-273 gives the coalescing to this wrapper.
// The four constraints it names are what these tests are for — never per
// keystroke, no timer of its own, no document walk, never the text.

/// A page with one task on it, and the reference a burst is keyed by.
///
/// A **page**, rather than `one_row`'s bare row, because what these tests are
/// about is which bursts belong to which document — and that is a question a
/// document with no root node cannot be asked. The reference is minted through
/// the domain's own `refs::task_ref`, never spelled here: a second spelling in a
/// test is how a burst that never closes gets asserted as correct.
fn a_task() -> (WorkspaceDoc, EntityRef) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Tasks").unwrap();
    let page = ws
        .insert(Parent::Root, Node::document("Plans").unwrap())
        .unwrap();
    let row = ws
        .insert(
            Parent::Node(page),
            Node::paragraph("post the form").unwrap(),
        )
        .unwrap();
    tasks::ensure_task(&mut ws, row, TaskReason::Explicit).unwrap();
    let task = tasks::refs::task_ref(&ws, row).unwrap();
    (ws, task)
}

/// A second task, a row under the first.
fn another_task(ws: &mut WorkspaceDoc, under: &EntityRef) -> EntityRef {
    let row = ws
        .insert(
            Parent::Node(under.node_id()),
            Node::paragraph("call the office").unwrap(),
        )
        .unwrap();
    tasks::ensure_task(ws, row, TaskReason::Explicit).unwrap();
    tasks::refs::task_ref(ws, row).unwrap()
}

/// **Never per keystroke** — the first constraint, and the one somebody would
/// break by emitting from the edit path. A hundred notes are a hundred map
/// writes and no event at all; the burst produces exactly one when it ends.
#[test]
fn a_hundred_keystrokes_are_one_event() {
    let (_ws, task) = a_task();
    let mut bursts = ContentBursts::new();
    let (mut host, seen) = recorder();
    let start = Instant::now();
    for tick in 0..100 {
        bursts.note(task, start + Duration::from_millis(tick * 10));
    }
    assert!(names(&seen).is_empty(), "noting emits nothing");
    host.content_changed(&bursts.caret_left(&task));
    assert_eq!(names(&seen), vec!["task.content.changed"]);
}

/// **The caret leaving the row** is the first boundary — a click, an arrow key,
/// Enter, a tab switch, a window blur. It does not consult the clock, and the
/// burst it ended is gone: the second boundary to arrive finds nothing, which
/// is what makes "one event per task per burst" true rather than nearly true.
#[test]
fn the_caret_leaving_the_row_ends_the_burst_once() {
    let (_ws, task) = a_task();
    let mut bursts = ContentBursts::new();
    let now = Instant::now();
    bursts.note(task, now);
    assert_eq!(bursts.caret_left(&task), vec![task]);
    assert!(bursts.is_empty());
    assert!(
        bursts.caret_left(&task).is_empty(),
        "a burst ends once, and a second boundary finds nothing"
    );
    assert!(
        bursts.quiet(now + BURST_QUIET * 2).is_empty(),
        "the tick has nothing left to find either"
    );
}

/// **The maintenance tick** is the second boundary, and this is where the "no
/// timer of its own" rule lives: `quiet` is *given* the time, so the type
/// cannot schedule anything and the host's existing throttled tick stays the
/// only clock in the feature. A keystroke inside the window pushes the deadline
/// out rather than opening a second burst.
#[test]
fn the_tick_ends_a_burst_only_after_five_quiet_seconds() {
    let (_ws, task) = a_task();
    let mut bursts = ContentBursts::new();
    let start = Instant::now();
    bursts.note(task, start);
    assert!(
        bursts
            .quiet(start + BURST_QUIET - Duration::from_millis(1))
            .is_empty(),
        "still being typed into"
    );
    // A keystroke four seconds in refreshes the deadline; one burst, not two.
    bursts.note(task, start + Duration::from_secs(4));
    assert!(
        bursts.quiet(start + BURST_QUIET).is_empty(),
        "the deadline follows the last keystroke, not the first"
    );
    assert_eq!(bursts.quiet(start + Duration::from_secs(9)), vec![task]);
    assert!(bursts.is_empty());
}

/// **A tab closing** is the third boundary. There is no row left for a caret to
/// leave and no tick will ever see it again, so what that tab held ends now —
/// and only what it held: another tab's burst is still being typed into.
///
/// The boundary is given the tasks rather than a document id — see
/// [`ContentBursts::closed`] for why that is now a choice rather than a
/// necessity. What this pins is the property that made it a necessity and no
/// longer holds: `task` and `sibling` are one page's rows and carry **one**
/// document segment between them, so `elsewhere` is another document's burst by
/// its segment as well as by its node.
#[test]
fn a_closing_tab_ends_the_bursts_it_held() {
    let (mut ws, task) = a_task();
    let sibling = another_task(&mut ws, &task);
    let elsewhere = EntityRef::task(ws.id().unwrap(), DocumentId::new(), NodeId::new());
    assert_eq!(
        task.document_id(),
        sibling.document_id(),
        "two rows of one page, one document segment"
    );
    assert_ne!(task.document_id(), elsewhere.document_id());
    let mut bursts = ContentBursts::new();
    let now = Instant::now();
    bursts.note(task, now);
    bursts.note(sibling, now);
    bursts.note(elsewhere, now);

    let ended = bursts.closed(&[task, sibling]);
    let mut expected = vec![task, sibling];
    expected.sort_by_key(|task| task.to_storage());
    assert_eq!(ended, expected, "both tasks the tab held");
    assert_eq!(bursts.len(), 1, "and nothing of the other tab");
    assert_eq!(bursts.drain(), vec![elsewhere]);
    assert!(
        bursts.closed(&[task]).is_empty(),
        "a task with no burst open owes no event"
    );
}

/// **Never the text**, and it carries no chain either: `task.content.changed`
/// belongs to no command, so neither `in_chain` nor `caused_by` is called on
/// the draft and a reader grouping by chain root sees exactly one event.
#[test]
fn the_coalesced_event_carries_nothing_but_its_subject() {
    let (_ws, task) = a_task();
    let (mut host, seen) = recorder();
    host.content_changed(&[task]);
    let published = envelopes(&seen);
    assert_eq!(published.len(), 1);
    let envelope = &published[0];
    assert_eq!(envelope.name.as_str(), "task.content.changed");
    assert!(
        envelope.attrs.is_empty(),
        "no attrs at all — the text is what it must never carry"
    );
    assert_eq!(envelope.correlation_id, None);
    assert_eq!(envelope.causation_id, None);
    assert_eq!(envelope.chain_root(), envelope.event_id);
    let subject = envelope.subject.as_ref().expect("a subject");
    assert!(subject.id.starts_with("task:"), "got {:?}", subject.id);
    assert!(envelope.scope.workspace_id.is_some());
}

/// The same rule the intent mapping follows: a reference that is not the task
/// spelling is **counted and dropped**, never published as a subject nobody can
/// query.
#[test]
fn a_reference_that_is_not_a_task_is_dropped_rather_than_published() {
    let block = EntityRef::block(WorkspaceId::new(), DocumentId::new(), NodeId::new());
    let before = counters().dropped;
    let (mut host, seen) = recorder();
    host.content_changed(&[block]);
    assert!(names(&seen).is_empty());
    assert!(counters().dropped > before);
}

/// Two tasks typed into in one burst window are two events, one each — the map
/// is keyed by task, so coalescing is the data structure rather than a rule
/// somebody applies. The order is stable, because two events published in an
/// arbitrary order are two events a reader cannot compare runs of.
#[test]
fn two_tasks_are_one_event_each_in_a_stable_order() {
    let (mut ws, first) = a_task();
    let second = another_task(&mut ws, &first);
    let mut bursts = ContentBursts::new();
    let now = Instant::now();
    bursts.note(first, now);
    bursts.note(second, now);
    bursts.note(first, now + Duration::from_millis(1));

    let ended = bursts.quiet(now + BURST_QUIET * 2);
    let mut expected = vec![first, second];
    expected.sort_by_key(|task| task.to_storage());
    assert_eq!(ended, expected);

    let (mut host, seen) = recorder();
    host.content_changed(&ended);
    assert_eq!(
        names(&seen),
        vec!["task.content.changed", "task.content.changed"]
    );
}

/// A host that never flushes must not cost anything worse than the feed it is
/// not draining. The oldest burst is the one to lose: activity is best effort
/// and is never retried or fabricated later.
#[test]
fn the_open_set_is_bounded() {
    let mut bursts = ContentBursts::new();
    let now = Instant::now();
    for tick in 0..(OPEN_LIMIT + 10) {
        let task = EntityRef::task(WorkspaceId::new(), DocumentId::new(), NodeId::new());
        bursts.note(task, now + Duration::from_millis(tick as u64));
    }
    assert_eq!(bursts.len(), OPEN_LIMIT);
}

// ------------------------------------------------------------ the priority door
//
// The other event no command produces. The view mints the intent — the editor's
// `set_attribute_routed` proves *when*, on both a task and a plain row — and
// this is the wrapper's half: that the intent becomes the registry's row, with
// the tokens ADR-273's table declares and nothing else.

/// **Priority on a task emits**, `set` the first time, and the row carries the
/// change and the priority and no value of anything else.
#[test]
fn a_priority_intent_from_the_view_becomes_the_registry_row() {
    let (_ws, task) = a_task();
    let owed = koru_editor_style_priority(task, AttrChange::Set, Some(Priority::High));
    let (mut host, seen) = recorder();
    host.attempt(owed.0, &owed.1);

    let published = envelopes(&seen);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].name.as_str(), "task.priority.changed");
    assert_eq!(published[0].actor.kind, koru_events::ActorKind::User);
    let attrs: Vec<&str> = published[0]
        .attrs
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(attrs, vec!["change", "priority"]);
}

/// **Cleared carries no `priority`** — absence means removed, never `low`, which
/// is why the registry declares the key optional.
#[test]
fn a_cleared_priority_names_no_priority() {
    let (_ws, task) = a_task();
    let owed = koru_editor_style_priority(task, AttrChange::Cleared, None);
    let (mut host, seen) = recorder();
    host.attempt(owed.0, &owed.1);

    let published = envelopes(&seen);
    let attrs: Vec<&str> = published[0]
        .attrs
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(attrs, vec!["change"]);
}

/// **A plain node's priority owes nothing**, and the wrapper is not where that
/// is decided — `route_attribute` is, which is what the door asks before it
/// mints anything. Pinning the routing here is pinning the reason this wrapper
/// never sees an intent for a plain row.
#[test]
fn priority_on_a_plain_node_never_reaches_the_wrapper() {
    let (mut ws, row) = one_row();
    assert!(!tasks::is_task_node(&ws, row).unwrap());
    assert_eq!(
        tasks::route_attribute(&ws, row, koru_core::attributes::KEY_PRIORITY).unwrap(),
        tasks::AttributeRoute::Ordinary,
        "a plain row takes the ordinary door and owes no task activity"
    );
    ws.set_attribute(
        row,
        koru_core::attributes::KEY_PRIORITY,
        koru_core::AttrValue::Text("high".into()),
    )
    .unwrap();
    assert!(
        !tasks::is_task_node(&ws, row).unwrap(),
        "and writing one created nothing"
    );
}

/// What `koru_editor::tasks::TaskActivity::alone` hands a host: one intent that
/// belongs to no command, so it mints its own chain root.
fn koru_editor_style_priority(
    task: EntityRef,
    change: AttrChange,
    priority: Option<Priority>,
) -> (CausationId, Vec<ActivityIntent>) {
    (
        CausationId::new(),
        vec![ActivityIntent::PriorityChanged {
            task,
            change,
            priority,
        }],
    )
}

// ------------------------------------------------- the one open set (T-0b)
//
// The map above, reached as a process global — which is what the two boundaries
// need, because the container's post-dispatch drain is handed a view and the
// maintenance tick is handed an `App`, and neither can see the other's fields.
// These tests are about that *location* and nothing else: the behaviour they
// exercise is `ContentBursts`', pinned above.

/// The desktop's two boundaries reach one map. A burst opened by the drain is a
/// burst the maintenance tick can end — which is the whole point of the global,
/// and the failure it prevents is a burst that no clock can ever reach.
#[test]
fn the_two_boundaries_reach_one_map() {
    let (_ws, task) = a_task();
    let start = Instant::now();

    note_content_edit(task, start);
    assert!(content_bursts_open());
    assert!(
        content_quiet(start + BURST_QUIET - Duration::from_millis(1)).is_empty(),
        "still being typed into"
    );
    assert_eq!(content_quiet(start + BURST_QUIET), vec![task]);
    assert!(
        content_quiet(start + BURST_QUIET * 2).is_empty(),
        "and the burst ended once"
    );
}

/// The caret and the closing tab reach it too, and each is a drain: whichever
/// boundary arrives first takes the burst, and the others find nothing.
#[test]
fn the_caret_and_a_close_drain_the_same_map() {
    let (mut ws, task) = a_task();
    let other = another_task(&mut ws, &task);
    let now = Instant::now();

    note_content_edit(task, now);
    note_content_edit(other, now);
    assert_eq!(content_caret_left(&task), vec![task]);
    assert!(content_caret_left(&task).is_empty());
    assert_eq!(content_closed(&[task, other]), vec![other]);
    assert!(
        content_quiet(now + BURST_QUIET * 2)
            .iter()
            .all(|flushed| *flushed != task && *flushed != other),
        "both bursts are gone before the tick sees them"
    );
}

/// **The burst path performs no disk or network I/O either**, and the same way
/// the gesture path does not: this runs the **shipping** wrapper — the one whose
/// sink is `events_host::emit` — in a process with no store, no gateway and no
/// events thread, and the flush still returns. Anything synchronous and remote
/// in it would hang or fail here.
///
/// The note and the boundary cannot reach one even in principle: neither is
/// handed a `Workspace`, a `Session` or a client, and the map's key is three
/// ids.
#[test]
fn a_flushed_burst_needs_no_store_gateway_or_events_thread() {
    let (_ws, task) = a_task();
    let now = Instant::now();
    note_content_edit(task, now);
    let flushed = content_caret_left(&task);
    assert_eq!(flushed, vec![task]);
    TaskHost::new().content_changed(&flushed);
}

// ====================================================== T-2 · emission coverage
//
// Everything above pins what this wrapper *does*. What follows pins what it
// **publishes**, against ADR-273's table of twelve — because the mapping being
// exhaustive by `match` only proves that every intent has an arm, never that
// an arm renders the row the table declares.
//
// The expectation is deliberately built from a second source. `render` below
// spells each row out of the **domain's own** enums (`TaskReason::as_str`,
// `LifecycleReason::as_str`, `Kind::as_str`, `TemporalKind::token`, …), while
// the wrapper renders it out of `koru-events`' — so a token that drifts in one
// vocabulary and not the other is a red test rather than a silently renamed
// value in the journal. Asserting the wrapper's output against the wrapper's
// own constants would only assert the arm back to itself.

use koru_core::tasks::golden;

/// One envelope, in the shape the table declares it: a name and its attrs in
/// key order.
type Row = (String, Vec<(String, koru_events::AttrValue)>);

fn rows(seen: &Arc<Mutex<Vec<Draft>>>) -> Vec<Row> {
    envelopes(seen)
        .into_iter()
        .map(|envelope| {
            (
                envelope.name.as_str().to_owned(),
                envelope.attrs.into_iter().collect(),
            )
        })
        .collect()
}

fn token(value: &str) -> koru_events::AttrValue {
    koru_events::AttrValue::Token(value.to_owned())
}

/// **ADR-273's table, spelled from the domain.**
///
/// The golden case says what a command owes in labels and closed tokens; this
/// turns one of those into the `(name, attrs)` an envelope must carry. Keys
/// come out in `BTreeMap` order because that is the order an envelope holds
/// them in — which is also alphabetical for every row, `change` before
/// `priority` and `child` before `child_class` before `reopened`.
fn render(activity: &golden::Activity, fx: &golden::Fixture) -> Row {
    use golden::Detail;
    let child_of = |label: &str| koru_events::AttrValue::Id(fx.node(label).as_uuid());
    let attrs: Vec<(String, koru_events::AttrValue)> = match activity.detail {
        Detail::Created { reason, node_kind } => vec![
            ("node_kind".to_owned(), token(node_kind.as_str())),
            ("reason".to_owned(), token(reason.as_str())),
        ],
        Detail::Removed => Vec::new(),
        Detail::Completed { reason } | Detail::Reopened { reason } => {
            vec![("reason".to_owned(), token(reason.as_str()))]
        }
        Detail::DueChanged {
            change,
            temporal_kind,
        } => vec![
            ("change".to_owned(), token(change.as_str())),
            (
                "temporal_kind".to_owned(),
                // The *shape*, never the value — and `none` once it is cleared.
                token(temporal_kind.map_or("none", TemporalKind::token)),
            ),
        ],
        Detail::PriorityChanged { change, priority } => {
            let mut attrs = vec![("change".to_owned(), token(change.as_str()))];
            // Absence means removed, never `low`.
            if let Some(priority) = priority {
                attrs.push(("priority".to_owned(), token(priority.as_str())));
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
            ("child".to_owned(), child_of(child)),
            ("child_class".to_owned(), token(class.as_str())),
            (
                "reopened".to_owned(),
                koru_events::AttrValue::Flag(reopened),
            ),
        ],
        Detail::ChildRemoved { child } => vec![("child".to_owned(), child_of(child))],
        Detail::ChildCompleted {
            child,
            child_is_task,
        } => vec![
            ("child".to_owned(), child_of(child)),
            (
                "child_is_task".to_owned(),
                koru_events::AttrValue::Flag(child_is_task),
            ),
        ],
        Detail::ChildReopened {
            child,
            child_is_task,
            reopened_parent,
        } => vec![
            ("child".to_owned(), child_of(child)),
            (
                "child_is_task".to_owned(),
                koru_events::AttrValue::Flag(child_is_task),
            ),
            (
                "reopened_parent".to_owned(),
                koru_events::AttrValue::Flag(reopened_parent),
            ),
        ],
    };
    (activity.event.to_owned(), attrs)
}

/// One golden case, driven through the wrapper exactly as the shipping paths
/// drive it, with everything it published kept.
fn published_for(case: &golden::Case) -> (golden::Fixture, Vec<koru_events::Envelope>) {
    use koru_core::tasks::golden::Command;

    let mut fx = golden::Fixture::build(case);
    let target = fx.node(case.command.target());
    let (mut host, seen) = recorder();
    match case.command {
        // The classification hook is not one of the six and may answer `None`,
        // so it takes `attempt` — the same door a view's outcome takes.
        Command::Reclassify { .. } | Command::ReclassifyMoved { .. } => {
            if let Ok(Some(outcome)) = case.command.apply_to(&mut fx.ws, target) {
                host.attempt(outcome.causation, &outcome.activity);
            }
        }
        _ => {
            let _ = host.run(&mut fx.ws, |ws| {
                case.command
                    .apply_to(ws, target)
                    .map(|answer| answer.expect("a command always answers an outcome"))
            });
        }
    }
    let published = envelopes(&seen);
    (fx, published)
}

/// **Every command's Activity, at this host, is the row ADR-273 declares.**
///
/// Names, keys and tokens, per golden case — and `&[]` is an assertion too: a
/// refusal and a no-op publish nothing at all.
#[test]
fn every_golden_case_publishes_the_rows_adr_249_declares() {
    let mut bad: Vec<String> = Vec::new();
    for case in golden::CASES {
        let mut fx = golden::Fixture::build(case);
        let target = fx.node(case.command.target());
        let (mut host, seen) = recorder();
        match case.command {
            golden::Command::Reclassify { .. } | golden::Command::ReclassifyMoved { .. } => {
                if let Ok(Some(outcome)) = case.command.apply_to(&mut fx.ws, target) {
                    host.attempt(outcome.causation, &outcome.activity);
                }
            }
            _ => {
                let _ = host.run(&mut fx.ws, |ws| {
                    case.command
                        .apply_to(ws, target)
                        .map(|answer| answer.expect("a command always answers an outcome"))
                });
            }
        }

        let want: Vec<Row> = match &case.expect {
            golden::Expect::Applied(applied) => applied
                .activity
                .iter()
                .map(|activity| render(activity, &fx))
                .collect(),
            // A refusal and an `Ok(None)` owe nothing: a wrapper never invents
            // an event a command did not name.
            golden::Expect::Refused(_) | golden::Expect::Nothing => Vec::new(),
        };
        let got = rows(&seen);
        if got != want {
            bad.push(format!(
                "{}:\n    want {want:#?}\n    got  {got:#?}",
                case.id
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} golden cases publish something other than ADR-273's row:\n\n{}",
        bad.len(),
        golden::CASES.len(),
        bad.join("\n\n")
    );
}

/// The other half of the same claim: an envelope is filed under the task the
/// table names, in the `task:` spelling, with the workspace on the scope.
#[test]
fn every_published_row_is_filed_under_the_task_the_table_names() {
    for case in golden::CASES {
        let golden::Expect::Applied(applied) = &case.expect else {
            continue;
        };
        let (fx, published) = published_for(case);
        assert_eq!(published.len(), applied.activity.len(), "{}", case.id);
        for (envelope, want) in published.iter().zip(applied.activity) {
            let subject = envelope.subject.as_ref().expect("a subject");
            let node = fx.node(want.subject).as_uuid();
            assert!(
                subject.id.starts_with("task:") && subject.id.ends_with(&node.to_string()),
                "{}: {} is filed under {:?}, not {:?}",
                case.id,
                envelope.name.as_str(),
                subject.id,
                want.subject
            );
            assert_eq!(
                subject.kind,
                koru_events::SubjectKind::Task,
                "{}: {}",
                case.id,
                envelope.name.as_str()
            );
            assert!(envelope.scope.workspace_id.is_some(), "{}", case.id);
        }
    }
}

/// **What the registry says the row may carry**, checked against what actually
/// left: no undeclared key, every required key present, and every token from
/// the declared closed vocabulary. A token the events crate spells one way and
/// the registry another is caught here rather than by a reader who cannot
/// filter on it.
#[test]
fn every_published_attr_is_declared_and_its_token_is_in_the_vocabulary() {
    for case in golden::CASES {
        let (_fx, published) = published_for(case);
        for envelope in &published {
            let name = envelope.name.as_str();
            let spec = koru_events::registry::spec(name)
                .unwrap_or_else(|| panic!("{}: {name} is not a registered event", case.id));
            for (key, value) in &envelope.attrs {
                let attr = spec
                    .attr(key)
                    .unwrap_or_else(|| panic!("{}: {name} carries undeclared {key}", case.id));
                match (attr.kind, value) {
                    (koru_events::AttrKind::Token, koru_events::AttrValue::Token(got)) => {
                        assert!(
                            attr.vocabulary.contains(&got.as_str()),
                            "{}: {name}.{key} = {got:?} is not in {:?}",
                            case.id,
                            attr.vocabulary
                        );
                    }
                    (koru_events::AttrKind::Id, koru_events::AttrValue::Id(_)) => {}
                    (koru_events::AttrKind::Flag, koru_events::AttrValue::Flag(_)) => {}
                    (kind, other) => panic!(
                        "{}: {name}.{key} is declared {} and carried {other:?}",
                        case.id,
                        kind.as_str()
                    ),
                }
            }
            for attr in spec.attrs.iter().filter(|attr| attr.required) {
                assert!(
                    envelope.attrs.contains_key(attr.key),
                    "{}: {name} is missing required {}",
                    case.id,
                    attr.key
                );
            }
        }
    }
}

// --------------------------------------------------------------- attribution

/// **Attribution is `Actor::user()`, everywhere.** The desktop half of the one
/// difference between the two hosts: the same outcome, the same causation id
/// and the same intents reach both, and only this differs (ADR-273).
#[test]
fn every_desktop_row_is_attributed_to_a_plain_user() {
    let mut published = 0;
    for case in golden::CASES {
        let (_fx, envelopes) = published_for(case);
        for envelope in &envelopes {
            published += 1;
            assert_eq!(
                envelope.actor.kind,
                koru_events::ActorKind::User,
                "{}: {}",
                case.id,
                envelope.name.as_str()
            );
            assert!(
                envelope.actor.token.is_none(),
                "{}: a desktop actor carries no agent token",
                case.id
            );
            assert!(envelope.actor.id.is_none(), "{}", case.id);
        }
    }
    assert!(published > 0, "the golden table published nothing at all");
}

/// The burst event takes the same attribution, and it is worth its own line
/// because it travels a different function — `content_changed` builds its own
/// draft rather than going through `draft_for`, so an attribution added there
/// and forgotten here would be invisible.
#[test]
fn the_coalesced_row_is_attributed_to_a_plain_user_too() {
    let (_ws, task) = a_task();
    let (mut host, seen) = recorder();
    host.content_changed(&[task]);
    let published = envelopes(&seen);
    assert_eq!(published[0].actor, Actor::user());
}

// ----------------------------------------------------------- cascade causation

/// **A cascade is a set of events sharing one chain root**, and the golden
/// table has three shapes of it: a downward completion, the child event that
/// tells the task above, and a reopen chain up the ancestors. Each is several
/// envelopes, one correlation, no `causation_id` — so T-1's grouping query is
/// one `WHERE` on the correlation whichever way the cascade ran.
#[test]
fn every_cascading_case_publishes_one_chain_root() {
    for id in [
        "complete.cascades_over_checkables_and_nested_tasks",
        "complete.tells_the_task_above_it",
        "reopen.reopens_every_completed_task_above_it",
        "reclassify.unfinished_child_reopens_the_task",
    ] {
        let case = golden::case(id).expect("the case is in the table");
        let (_fx, published) = published_for(case);
        assert!(
            published.len() >= 2,
            "{id} is a cascade case and published {} event(s)",
            published.len()
        );
        let root = published[0].chain_root();
        for envelope in &published {
            assert_eq!(
                envelope.correlation_id,
                Some(root),
                "{id}: {} left the chain",
                envelope.name.as_str()
            );
            assert_eq!(
                envelope.causation_id, None,
                "{id}: none of a command's events caused another"
            );
            assert_eq!(envelope.chain_root(), root, "{id}");
        }
        // Distinct events, one chain: the grouping is not an artefact of two
        // identical envelopes.
        let ids: std::collections::BTreeSet<_> =
            published.iter().map(|envelope| envelope.event_id).collect();
        assert_eq!(ids.len(), published.len(), "{id}");
    }
}

/// A *child* event and its companion travel together — the parent's side and
/// the child's side of one fact, under one root. `complete.tells_the_task_above
/// _it` is the shape: a `task.completed` for the child and a
/// `task.child.completed` for the task above it.
#[test]
fn a_child_event_shares_the_root_with_the_event_it_companions() {
    let case = golden::case("complete.tells_the_task_above_it").expect("the case is in the table");
    let (_fx, published) = published_for(case);
    let names: Vec<&str> = published
        .iter()
        .map(|envelope| envelope.name.as_str())
        .collect();
    assert_eq!(names, vec!["task.completed", "task.child.completed"]);
    assert_eq!(published[0].chain_root(), published[1].chain_root());
}

// ------------------------------------------------------ the registry drift net

/// Every task row in the registry, in declaration order.
fn task_rows() -> Vec<&'static koru_events::EventSpec> {
    koru_events::REGISTRY
        .iter()
        .filter(|spec| spec.name.starts_with("task."))
        .collect()
}

/// One intent of every shape, so the mapping can be walked rather than
/// reasoned about. A thirteenth `ActivityIntent` is already a compile error in
/// [`draft_for`]'s exhaustive `match`; what this list adds is the other
/// direction — a row nothing produces.
fn one_intent_of_each_shape(task: EntityRef, child: NodeId) -> Vec<ActivityIntent> {
    vec![
        ActivityIntent::Created {
            task,
            reason: TaskReason::Explicit,
            node_kind: Kind::Paragraph,
        },
        ActivityIntent::Removed { task },
        ActivityIntent::Completed {
            task,
            reason: LifecycleReason::ExplicitComplete,
        },
        ActivityIntent::Reopened {
            task,
            reason: LifecycleReason::ExplicitReopen,
        },
        ActivityIntent::DueChanged {
            task,
            change: AttrChange::Set,
            temporal_kind: Some(TemporalKind::Date),
        },
        ActivityIntent::PriorityChanged {
            task,
            change: AttrChange::Set,
            // Set rather than cleared, so the optional key is present and the
            // row's full declared set is exercised.
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

/// **The drift net.** Every registry row a producer names, and every producer a
/// registry row — so a thirteenth event, or a mapping quietly deleted, breaks a
/// test that says which.
///
/// Eleven rows come from an [`ActivityIntent`] through [`draft_for`]. The
/// twelfth, `task.content.changed`, belongs to **both** hosts and to no command:
/// this wrapper produces it from [`TaskHost::content_changed`] and MCP's
/// produces it once per `edit_document` call (pinned in that crate).
#[test]
fn every_registry_row_has_a_producer_and_every_producer_a_row() {
    let (_ws, task) = a_task();
    let child = NodeId::new();

    let mut produced: Vec<&'static str> = Vec::new();
    for intent in one_intent_of_each_shape(task, child) {
        let draft = draft_for(CausationId::new(), &intent)
            .unwrap_or_else(|| panic!("{} has no mapping in this wrapper", intent.event()));
        let envelope = draft.into_envelope(&koru_events::EmitContext::system("test"));
        assert_eq!(
            envelope.name.as_str(),
            intent.event(),
            "the wrapper renamed an intent on the way out"
        );
        produced.push(intent.event());
    }
    // The twelfth, through its own door.
    let (mut host, seen) = recorder();
    host.content_changed(&[task]);
    produced.extend(names(&seen).iter().map(|name| {
        koru_events::registry::spec(name)
            .expect("the coalesced row is registered")
            .name
    }));

    let registered: Vec<&'static str> = task_rows().iter().map(|spec| spec.name).collect();
    let mut want = registered.clone();
    want.sort_unstable();
    let mut got = produced.clone();
    got.sort_unstable();
    assert_eq!(
        got, want,
        "the registry and this wrapper disagree about which task events exist"
    );
    assert_eq!(
        registered.len(),
        12,
        "ADR-273 declares twelve task events; the registry now has {}. \
         A thirteenth needs a producer mapping in a wrapper and a row here.",
        registered.len()
    );
}

/// Each row's declared keys, against what the mapping actually emits for it.
/// The golden run covers the rows a command produces; this covers all twelve,
/// including `task.child.removed`, which belongs to no command — its producer
/// is the departure hook (T-0c), driven for real in
/// `a_deleted_structural_child_reaches_the_child_removed_row`.
#[test]
fn every_row_emits_exactly_the_keys_the_registry_declares() {
    let (_ws, task) = a_task();
    let child = NodeId::new();
    let ctx = koru_events::EmitContext::system("test");
    for intent in one_intent_of_each_shape(task, child) {
        let envelope = draft_for(CausationId::new(), &intent)
            .expect("a mapping")
            .into_envelope(&ctx);
        let spec = koru_events::registry::spec(envelope.name.as_str()).expect("a registered row");
        // Sorted on both sides: an envelope holds its attrs in a `BTreeMap`
        // and the registry declares them in reading order, so the two agree
        // about the *set* and never about the sequence.
        let got: Vec<&str> = envelope.attrs.keys().map(String::as_str).collect();
        let mut want: Vec<&str> = spec.attrs.iter().map(|attr| attr.key).collect();
        want.sort_unstable();
        assert_eq!(got, want, "{}", envelope.name.as_str());
    }
}

/// **No task row can carry content, structurally.** Every declared attr on
/// every one of the twelve is a `Token`, an `Id` or a `Flag` — the three
/// `AttrValue` shapes that cannot hold prose — and `task.content.changed`
/// declares none at all, which is what "never the text" means in the type
/// system rather than in a review.
#[test]
fn no_task_row_declares_an_attr_that_could_hold_text() {
    for spec in task_rows() {
        for attr in spec.attrs {
            assert!(
                matches!(
                    attr.kind,
                    koru_events::AttrKind::Token
                        | koru_events::AttrKind::Id
                        | koru_events::AttrKind::Flag
                ),
                "{}.{} is declared {} — a task event may only carry a closed \
                 token, a local reference or a flag",
                spec.name,
                attr.key,
                attr.kind.as_str()
            );
        }
        if spec.name == "task.content.changed" {
            assert!(
                spec.attrs.is_empty(),
                "the coalesced row declares attrs; the text is the one thing it \
                 must never carry"
            );
        }
    }
}

/// The same claim about what is *built*, not only what is declared: every way
/// this wrapper can construct the coalesced event produces an envelope whose
/// attr set is exactly empty. `content_changed` is the only construction — it
/// is the only caller of `koru_events::TaskContentChanged` in this crate — and
/// a burst of any size flushes through it.
#[test]
fn every_coalesced_envelope_this_wrapper_builds_has_no_attrs_at_all() {
    let (mut ws, first) = a_task();
    let second = another_task(&mut ws, &first);
    let (mut host, seen) = recorder();
    host.content_changed(&[first, second]);
    let published = envelopes(&seen);
    assert_eq!(published.len(), 2);
    for envelope in &published {
        assert_eq!(envelope.name.as_str(), "task.content.changed");
        assert!(envelope.attrs.is_empty(), "{:?}", envelope.attrs);
    }
}

// ---------------------------------------------------------------- negative space
//
// SPEC §9 and ADR-273: tags, color, custom attributes and list membership
// produce **no** task activity this release, and T-2 asserts the absence rather
// than trusting it. Each door below is driven on a node that *is* a task, which
// is the case where a mistaken emission would look plausible.

/// Every ordinary key takes the ordinary route on a task, so no command runs
/// and there is no [`TaskOutcome`] to take an intent from. That is the reason
/// the wrapper never sees one, and it is the load-bearing half: a key that
/// started answering `PriorityOnTask` would mint an intent at the door, above
/// this crate.
#[test]
fn tags_color_custom_keys_and_list_membership_take_the_ordinary_route() {
    let (ws, task) = a_task();
    let node = task.node_id();
    assert!(tasks::is_task_node(&ws, node).unwrap());
    for key in [
        koru_core::attributes::KEY_TAG,
        koru_core::attributes::KEY_COLOR,
        koru_core::attributes::KEY_LIST,
        "workspace.custom_field",
    ] {
        assert_eq!(
            tasks::route_attribute(&ws, node, key).unwrap(),
            tasks::AttributeRoute::Ordinary,
            "{key} owes no task activity"
        );
    }
    // And the priority key is the one that does not, which is what makes the
    // four above an assertion rather than a tautology about `route_attribute`.
    assert_eq!(
        tasks::route_attribute(&ws, node, koru_core::attributes::KEY_PRIORITY).unwrap(),
        tasks::AttributeRoute::PriorityOnTask,
    );
}

/// The four doors, driven for real on a task: a tag, a color, a custom
/// definition and a list membership all land, the node is still the same task
/// with the same lifecycle, and **nothing is published** — the wrapper is asked
/// for the empty intent list every one of them produces.
#[test]
fn tags_color_custom_attributes_and_list_membership_publish_nothing() {
    use koru_core::attributes::{AttributeDefinition, KEY_COLOR, ValueType, system_definition};
    use koru_core::ids::{ListId, TagId};

    let (mut ws, task) = a_task();
    let node = task.node_id();
    let before = tasks::TaskFields::of(&ws.read(node).unwrap());

    // The host's one multi-apply site is a tag (`app.rs`'s `add_tag_to_all`),
    // and it answers a bool: there is no outcome, so there is no intent.
    assert!(ws.add_tag_to_all(&[node], TagId::new()).unwrap());
    assert!(
        ws.set_attribute_on_all(
            &[node],
            KEY_COLOR,
            koru_core::AttrValue::Text("teal".into())
        )
        .unwrap()
    );
    let custom = AttributeDefinition::custom("workspace.owner", "Owner", ValueType::Text);
    ws.set_attribute_defined(node, &custom, koru_core::AttrValue::Text("me".into()))
        .unwrap();
    ws.set_list(node, ListId::new()).unwrap();

    // None of the four is a command, so the only thing a host could hand the
    // wrapper is an empty list — and that publishes nothing and moves no
    // counter.
    let (mut host, seen) = recorder();
    host.attempt(CausationId::new(), &[]);
    assert!(
        names(&seen).is_empty(),
        "an ordinary attribute door published task activity"
    );

    let after = tasks::TaskFields::of(&ws.read(node).unwrap());
    assert_eq!(after.marker, before.marker, "still the same task");
    assert_eq!(after.lifecycle, before.lifecycle, "and still open");
    assert!(tasks::is_task_node(&ws, node).unwrap());
    assert!(
        system_definition(koru_core::attributes::KEY_LIST).is_some(),
        "list membership is an ordinary registered key, not a command's"
    );
}

/// Removing a tag and clearing a list are the other halves of the same doors,
/// and they owe nothing either — an absence is not activity.
#[test]
fn removing_a_tag_or_a_list_membership_publishes_nothing() {
    use koru_core::ids::{ListId, TagId};

    let (mut ws, task) = a_task();
    let node = task.node_id();
    let tag = TagId::new();
    ws.add_tag_to_all(&[node], tag).unwrap();
    ws.set_list(node, ListId::new()).unwrap();

    assert!(ws.remove_tag_from_all(&[node], tag).unwrap());
    assert!(ws.clear_list(node).unwrap());

    let (mut host, seen) = recorder();
    host.attempt(CausationId::new(), &[]);
    assert!(names(&seen).is_empty());
    assert!(
        tasks::is_task_node(&ws, node).unwrap(),
        "and none of it took the task away"
    );
}

// -------------------------------------------------------- departures (T-0c)

/// A task holding a nested task, and a plain sibling. `(ws, task, child, loose)`.
fn a_task_with_a_subtask() -> (WorkspaceDoc, NodeId, NodeId, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Tasks").unwrap();
    let task = ws
        .insert(Parent::Root, Node::paragraph("ship it").unwrap())
        .unwrap();
    let child = ws
        .insert(Parent::Node(task), Node::paragraph("draft").unwrap())
        .unwrap();
    let loose = ws
        .insert(Parent::Root, Node::paragraph("elsewhere").unwrap())
        .unwrap();
    tasks::ensure_task(&mut ws, task, TaskReason::Explicit).unwrap();
    tasks::ensure_task(&mut ws, child, TaskReason::Explicit).unwrap();
    (ws, task, child, loose)
}

/// **A real departure reaches the row.** T-2 could only drive
/// `task.child.removed` through a synthetic intent, because nothing in the tree
/// minted one; T-0c's producer is `koru_core::tasks::departing`, and this runs
/// its answer through the shipping mapping.
///
/// The editor is where the *call sites* are and this crate deliberately does not
/// depend on it (see `Cargo.toml`), so what is proved here is the half that is
/// this host's: a domain-produced departure renders the registry row, filed
/// under the task, carrying the child's id and nothing else.
#[test]
fn a_deleted_structural_child_reaches_the_child_removed_row() {
    let (mut ws, task, child, _loose) = a_task_with_a_subtask();
    let departing = tasks::departing(&ws, child)
        .expect("the read succeeds")
        .expect("the child is under a task");
    ws.delete(child).expect("the delete lands");

    let (mut host, seen) = recorder();
    let causation = CausationId::new();
    host.attempt(causation, &[departing.deleted()]);

    let envelopes = envelopes(&seen);
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].name.as_str(), "task.child.removed");
    let subject = envelopes[0].subject.as_ref().expect("a subject");
    assert!(
        subject.id.starts_with("task:") && subject.id.ends_with(&task.as_uuid().to_string()),
        "filed under the task it left: {}",
        subject.id
    );
    let got: Vec<&str> = envelopes[0].attrs.keys().map(String::as_str).collect();
    assert_eq!(got, vec!["child"], "the row's one declared key");
    assert_eq!(
        envelopes[0].attrs["child"],
        koru_events::AttrValue::Id(child.as_uuid()),
        "and it is the child that left, as a reference"
    );
    assert_eq!(task, departing.task().node_id());
}

/// **A move that left the task reaches the same row; one that stayed inside it
/// reaches nothing.** The wrapper is not the thing deciding — the domain is —
/// but a host that published the departure of a node that never left would be
/// the visible failure.
#[test]
fn a_move_out_reaches_the_row_and_a_move_within_reaches_nothing() {
    let (mut ws, _task, child, loose) = a_task_with_a_subtask();
    let departing = tasks::departing(&ws, child).unwrap().unwrap();

    // Still inside: nothing to attempt at all.
    assert_eq!(departing.moved(&ws).unwrap(), None);

    ws.move_node(child, Parent::Node(loose), 0).unwrap();
    let intent = departing.moved(&ws).unwrap().expect("it left");

    let (mut host, seen) = recorder();
    host.attempt(CausationId::new(), &[intent]);
    assert_eq!(names(&seen), vec!["task.child.removed"]);
}

// ------------------------------------------------------------ membership (V-4)

/// **A membership change is a command on this channel and owes no activity.**
///
/// It travels in [`TaskCommand`] because it needs exactly what a tick needs — a
/// `&mut Workspace`, to park the task's own document and publish it — and a
/// second channel for one attribute would be a second answer to *how does a
/// Tasks surface write*. What it is **not** is a canonical task command:
/// `koru.list` takes the ordinary attribute route (SPEC §9 registers no event
/// for it), so [`serve`]'s arm runs the document door and attempts nothing.
///
/// Two halves, and the second is why this is a test rather than a comment: the
/// arm is reachable and answers a **sentence** rather than panicking when the
/// document cannot be reached, because a failed command is shown to a person in
/// a toast.
#[test]
fn a_list_change_is_a_command_that_owes_nothing() {
    use koru_core::ids::ListId;

    let route = {
        let (ws, task) = a_task();
        tasks::route_attribute(&ws, task.node_id(), koru_core::attributes::KEY_LIST).unwrap()
    };
    assert_eq!(
        route,
        tasks::AttributeRoute::Ordinary,
        "a membership is not a canonical command, so nothing is owed for one"
    );

    // A workspace with no store cannot park the task's document, and the
    // refusal comes back as something to say rather than as a panic.
    let mut workspace =
        koru_workspace::Workspace::new(WorkspaceDoc::create(WorkspaceId::new(), "w").unwrap());
    for list in [Some(ListId::new()), None] {
        let error = super::serve(
            &mut workspace,
            &TaskCommand::SetList {
                document: DocumentId::new(),
                node: NodeId::new(),
                list,
            },
        )
        .expect_err("no store, so the document cannot be parked");
        assert!(!error.is_empty(), "and the caller is given a sentence");
    }

    // …and the wrapper published nothing on the way past.
    let (mut host, seen) = recorder();
    host.attempt(CausationId::new(), &[]);
    assert!(names(&seen).is_empty());
}

// ------------------------------------------------------ managing a list (V-6)

/// **Deleting a list deletes no task**, and the sweep runs in the order that
/// keeps that true: the registry record is tombstoned first, and only then are
/// the memberships taken off the nodes.
///
/// Both halves are here because the second is what a reviewer would be tempted
/// to reorder. A workspace with no store cannot write the registry — the same
/// refusal `a_list_change_is_a_command_that_owes_nothing` relies on — and the
/// sweep must not run anyway: clearing memberships for a list still in
/// everybody's rail is a worse answer than clearing none. Then the sweep itself,
/// through the door the arm calls, on the document this window has open.
#[test]
fn deleting_a_list_sweeps_memberships_and_leaves_the_tasks() {
    use koru_core::ids::ListId;

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "w").unwrap();
    let wedding = ListId::new();
    let bike = ListId::new();
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
    let mut workspace = koru_workspace::Workspace::new(ws);

    // The registry refuses — there is no store to write it to — so nothing is
    // swept. The tidy-up never runs ahead of the deletion it tidies up after.
    assert!(!super::serve_list_edit(
        &mut workspace,
        &ListEdit::DeleteTree {
            id: wedding,
            inside: vec![bike],
        }
    ));
    assert_eq!(
        workspace.ws.read(cake).unwrap().list_id(),
        Some(wedding),
        "a refused delete sweeps nothing"
    );

    // The sweep itself, on the open document, through the call that arm makes.
    assert_eq!(workspace.ws.clear_list_membership(wedding).unwrap(), 2);
    for node in [cake, venue] {
        let read = workspace.ws.read(node).unwrap();
        assert_eq!(read.list_id(), None, "the membership went");
        assert!(
            read.content.as_text().is_some_and(|t| !t.is_empty()),
            "and the task did not"
        );
    }
    assert_eq!(
        workspace.ws.read(groupset).unwrap().list_id(),
        Some(bike),
        "another list's memberships are untouched"
    );

    // A rename and a recolour are registry-only: they answer *nothing was
    // written to the document*, which is what the caller's adopt is gated on.
    for edit in [
        ListEdit::Rename {
            id: bike,
            name: "Bike Project".into(),
        },
        ListEdit::Recolor {
            id: bike,
            color: Some("teal".into()),
        },
    ] {
        assert!(!super::serve_list_edit(&mut workspace, &edit));
    }
    assert_eq!(
        workspace.ws.read(groupset).unwrap().list_id(),
        Some(bike),
        "and neither touches a task"
    );
}

// ------------------------------------------------ the in-place writes (INPLACE C)

use koru_core::attributes::Priority;
use koru_core::time::{DateOnly, Temporal};

/// A workspace holding the document as its active one, with no store — the
/// road `serve` takes for a document this window has open.
fn holding(ws: WorkspaceDoc) -> (koru_workspace::Workspace, DocumentId) {
    let id = DocumentId::new();
    (koru_workspace::Workspace::holding_for_tests(ws, id), id)
}

fn due_of(ws: &WorkspaceDoc, node: NodeId) -> Option<Temporal> {
    tasks::TaskFields::of(&ws.read(node).unwrap()).due
}

fn on(s: &str) -> Temporal {
    Temporal::Date(DateOnly::parse(s).unwrap())
}

/// **A due set from the page writes through the canonical command and owes
/// its activity**, with a receipt naming the document.
#[test]
fn a_due_set_and_cleared_from_the_page_takes_the_command_door() {
    let (ws, task) = a_task();
    let node = task.node_id();
    let (mut w, id) = holding(ws);
    let (mut host, seen) = recorder();

    let landed = super::serve_with(
        &mut w,
        &TaskCommand::SetDue {
            document: id,
            node,
            due: Some(on("2026-09-04")),
        },
        &mut host,
    )
    .expect("a task takes a due");
    assert_eq!(landed.document, id);
    assert_eq!(due_of(&w.ws, node), Some(on("2026-09-04")));
    assert_eq!(
        names(&seen),
        vec!["task.due.changed"],
        "the due change was attempted"
    );

    super::serve_with(
        &mut w,
        &TaskCommand::SetDue {
            document: id,
            node,
            due: None,
        },
        &mut host,
    )
    .expect("and a task's due can be cleared");
    assert_eq!(due_of(&w.ws, node), None);
    assert!(
        tasks::is_task_node(&w.ws, node).unwrap(),
        "clearing a due marks first: the task survives"
    );
    assert_eq!(names(&seen).len(), 2);
}

/// **A row that stopped being a task refuses**, with the one sentence, and
/// writes nothing — separately for a node that lost the marker and a node
/// that is gone. `set_due` would have recreated the first; a page of tasks
/// must not.
#[test]
fn a_row_that_is_no_longer_a_task_refuses_with_the_one_sentence() {
    let (mut ws, task) = a_task();
    let node = task.node_id();
    // Removed *after* the page was built and the picker opened: the row is
    // still on screen, the marker is gone.
    tasks::remove_from_tasks(&mut ws, node).unwrap();
    assert!(!tasks::is_task_node(&ws, node).unwrap());
    let (mut w, id) = holding(ws);
    let version = w.ws.version();
    let (mut host, seen) = recorder();

    for command in [
        TaskCommand::SetDue {
            document: id,
            node,
            due: Some(on("2026-09-04")),
        },
        TaskCommand::SetPriority {
            document: id,
            node,
            priority: Some(Priority::High),
        },
        TaskCommand::SetDue {
            document: id,
            node: NodeId::new(),
            due: Some(on("2026-09-04")),
        },
        TaskCommand::SetPriority {
            document: id,
            node: NodeId::new(),
            priority: None,
        },
    ] {
        let error = super::serve_with(&mut w, &command, &mut host).expect_err("refused");
        assert_eq!(error, super::TASK_GONE, "{command:?}");
    }
    assert_eq!(w.ws.version(), version, "nothing was written");
    assert!(
        !tasks::is_task_node(&w.ws, node).unwrap(),
        "and nothing was resurrected"
    );
    assert!(names(&seen).is_empty(), "and nothing was owed");
}

/// **Priority keeps ADR-286's route**: the routed write, owing a
/// `PriorityChanged` intent on set, changed and cleared, and nothing for a
/// write that changes nothing.
#[test]
fn a_priority_from_the_page_owes_set_changed_cleared_and_nothing_for_a_no_op() {
    let (ws, task) = a_task();
    let node = task.node_id();
    let (mut w, id) = holding(ws);
    let (mut host, seen) = recorder();
    let mut set = |w: &mut koru_workspace::Workspace, priority: Option<Priority>| {
        super::serve_with(
            w,
            &TaskCommand::SetPriority {
                document: id,
                node,
                priority,
            },
            &mut host,
        )
        .expect("a task takes a priority")
    };

    set(&mut w, Some(Priority::High));
    assert_eq!(w.ws.read(node).unwrap().priority(), Some(Priority::High));
    set(&mut w, Some(Priority::Medium));
    set(&mut w, Some(Priority::Medium));
    set(&mut w, None);
    assert_eq!(w.ws.read(node).unwrap().priority(), None);
    set(&mut w, None);
    assert_eq!(
        names(&seen),
        vec![
            "task.priority.changed",
            "task.priority.changed",
            "task.priority.changed"
        ],
        "set, changed, cleared — and neither no-op owed anything"
    );
}

/// **One undo step, and only its own.** The routed write has no boundary of
/// its own; without the `compose` bracket a priority set from the page would
/// fold into whatever typing group was open in the document and one Ctrl+Z
/// would take back both.
#[test]
fn a_priority_from_the_page_is_exactly_one_undo_step() {
    let (mut ws, task) = a_task();
    let node = task.node_id();
    ws.enable_undo();
    // Typing in the document, with its group still open.
    ws.begin_group();
    ws.insert_text(node, 0, "urgently ").unwrap();
    let typed = ws.read(node).unwrap().content.as_text().unwrap().to_owned();
    let (mut w, id) = holding(ws);

    super::serve(
        &mut w,
        &TaskCommand::SetPriority {
            document: id,
            node,
            priority: Some(Priority::High),
        },
    )
    .unwrap();
    assert_eq!(w.ws.read(node).unwrap().priority(), Some(Priority::High));

    assert!(w.ws.undo().unwrap());
    assert_eq!(
        w.ws.read(node).unwrap().priority(),
        None,
        "one Ctrl+Z takes the priority back"
    );
    assert_eq!(
        w.ws.read(node).unwrap().content.as_text(),
        Some(typed.as_str()),
        "and nothing else — the typing is still there"
    );
}

/// The tokens the switch prints for the two new verbs: the verb only, never
/// a date or a level.
#[test]
fn the_in_place_writes_have_closed_tokens() {
    let (document, node) = (DocumentId::new(), NodeId::new());
    let tokens: Vec<&str> = [
        TaskCommand::SetDue {
            document,
            node,
            due: Some(on("2026-09-04")),
        },
        TaskCommand::SetDue {
            document,
            node,
            due: None,
        },
        TaskCommand::SetPriority {
            document,
            node,
            priority: Some(Priority::Urgent),
        },
        TaskCommand::SetPriority {
            document,
            node,
            priority: None,
        },
    ]
    .iter()
    .map(TaskCommand::token)
    .collect();
    assert_eq!(
        tokens,
        ["set_due", "clear_due", "set_priority", "clear_priority"]
    );
}
