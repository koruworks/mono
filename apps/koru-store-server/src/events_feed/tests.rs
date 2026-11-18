// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_projection::{AttributeChange, AttributeEvent, AttributeEventKind};
use koru_store_core::{
    DictionaryCommitted, IdentityChange, IdentityCommitted, ProfileId, ReminderChange,
    ReminderCommitted, ReminderId,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct FlakySink {
    failures_left: AtomicUsize,
    delivered: Arc<Mutex<Vec<Vec<FeedNote>>>>,
}

impl FeedSink for FlakySink {
    // `fetch_update` is deprecated for `try_update`, which is unstable on the
    // Rust 1.93 the Linux API image builds with; rename when that image moves.
    #[allow(deprecated)]
    fn feed(&self, notes: &[FeedNote]) -> Result<u32, String> {
        if self
            .failures_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err("injected journal failure".into());
        }
        self.delivered.lock().unwrap().push(notes.to_vec());
        Ok(notes.len() as u32)
    }
}

#[test]
fn this_suite_sends_no_telemetry() {
    assert!(!koru_events_server::forward::sends_by_default());
    assert!(koru_events_server::forward::configured_sink().is_none());
}

#[test]
fn reminder_wakes_map_every_change_without_content() {
    let profile = ProfileId::new();
    let reminder = ReminderId::new();
    let changes = [
        ReminderChange::Rule,
        ReminderChange::Action,
        ReminderChange::History,
        ReminderChange::Settings,
    ]
    .map(|change| ReminderCommitted {
        profile,
        reminder: Some(reminder),
        change,
    });
    let notes = reminder_notes(&changes);
    assert_eq!(notes.len(), 4);
    for (note, expected) in notes.iter().zip([
        ReminderWake::Rule,
        ReminderWake::Action,
        ReminderWake::History,
        ReminderWake::Settings,
    ]) {
        assert!(matches!(
            note,
            FeedNote::ReminderChanged { profile: value, change }
                if value == &profile.to_storage() && *change == expected
        ));
    }
}

#[test]
fn dictionary_wake_names_only_its_profile() {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut feed = EventsFeed {
        sink: Box::new(FlakySink {
            failures_left: AtomicUsize::new(0),
            delivered: Arc::clone(&delivered),
        }),
        projections: Projections::in_memory().unwrap(),
        pending: BTreeMap::new(),
        task_digests: HashMap::new(),
        tasks_dirty: BTreeSet::new(),
    };
    let profile = ProfileId::new();
    feed.dictionary_changes(&[DictionaryCommitted { profile }]);
    assert!(matches!(
        &delivered.lock().unwrap()[0][0],
        FeedNote::DictionaryChanged { profile: value } if value == &profile.to_storage()
    ));
}

/// The imported-identity wake reaches the journal as its own note, and the two
/// facts stay apart: one key each, so a rename and a recolour in the same pass
/// are two notes rather than one overwriting the other.
#[test]
fn an_imported_identity_wake_carries_which_fact_moved() {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut feed = EventsFeed {
        sink: Box::new(FlakySink {
            failures_left: AtomicUsize::new(0),
            delivered: Arc::clone(&delivered),
        }),
        projections: Projections::in_memory().unwrap(),
        pending: BTreeMap::new(),
        task_digests: HashMap::new(),
        tasks_dirty: BTreeSet::new(),
    };
    let profile = ProfileId::new();
    feed.identity_changes(&[
        IdentityCommitted {
            profile,
            change: IdentityChange::Name,
        },
        IdentityCommitted {
            profile,
            change: IdentityChange::Appearance,
        },
    ]);
    let delivered = delivered.lock().unwrap();
    let notes = &delivered[0];
    assert_eq!(notes.len(), 2);
    for (note, expected) in notes
        .iter()
        .zip([IdentityWake::Appearance, IdentityWake::Name])
    {
        assert!(matches!(
            note,
            FeedNote::ProfileIdentityChanged { profile: value, change }
                if value == &profile.to_storage() && *change == expected
        ));
    }
}

#[test]
fn due_changes_and_entity_removals_map_only_ids() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let due = AttributeEvent {
        workspace,
        document,
        node: "11111111-1111-4111-8111-111111111111".into(),
        kind: AttributeEventKind::DueChanged {
            change: AttributeChange::Cleared,
        },
    };
    let notes = due_notes(&[due]);
    assert!(matches!(
        &notes[0],
        FeedNote::DueChanged { workspace: w, document: d, entity }
            if w == &workspace.to_storage()
                && d == &document.to_storage()
                && entity == "11111111-1111-4111-8111-111111111111"
    ));
    let removed = entity_removed_notes(
        workspace,
        document,
        &["22222222-2222-4222-8222-222222222222".into()],
    );
    assert!(matches!(
        &removed[0],
        FeedNote::EntityRemoved { workspace: w, document: d, entity }
            if w == &workspace.to_storage()
                && d == &document.to_storage()
                && entity == "22222222-2222-4222-8222-222222222222"
    ));
}

#[test]
fn a_failed_feed_is_coalesced_and_retried_without_another_commit() {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut feed = EventsFeed {
        sink: Box::new(FlakySink {
            failures_left: AtomicUsize::new(1),
            delivered: Arc::clone(&delivered),
        }),
        projections: Projections::in_memory().unwrap(),
        pending: BTreeMap::new(),
        task_digests: HashMap::new(),
        tasks_dirty: BTreeSet::new(),
    };
    let profile = ProfileId::new().to_storage();

    feed.deliver(vec![FeedNote::ReminderChanged {
        profile: profile.clone(),
        change: ReminderWake::Action,
    }]);
    assert_eq!(feed.pending.len(), 1);
    assert!(delivered.lock().unwrap().is_empty());

    // The next idle projector tick invokes this method even with no new store
    // commit.
    feed.flush_pending();
    assert!(feed.pending.is_empty());
    let delivered = delivered.lock().unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].len(), 1);
    assert!(matches!(
        &delivered[0][0],
        FeedNote::ReminderChanged { profile: value, change: ReminderWake::Action }
            if value == &profile
    ));
}

// --------------------------------------------- `projection.tasks.changed`

fn feed_into(delivered: &Arc<Mutex<Vec<Vec<FeedNote>>>>) -> EventsFeed {
    EventsFeed {
        sink: Box::new(FlakySink {
            failures_left: AtomicUsize::new(0),
            delivered: Arc::clone(delivered),
        }),
        projections: Projections::in_memory().unwrap(),
        pending: BTreeMap::new(),
        task_digests: HashMap::new(),
        tasks_dirty: BTreeSet::new(),
    }
}

fn plain(id: &str, text: &str) -> koru_projection::ProjectedNode {
    koru_projection::ProjectedNode {
        id: id.to_owned(),
        parent: None,
        ordinal: 0,
        depth: 0,
        kind: "paragraph".to_owned(),
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
        in_trash: false,
        runs: Vec::new(),
        attrs: Vec::new(),
    }
}

/// A task: a node carrying the marker and its place in the list.
fn task(id: &str, text: &str, rank: i64) -> koru_projection::ProjectedNode {
    let attr = |role: &str, kind: &str, value: &str| koru_projection::ProjectedAttr {
        id: format!("{id}-{role}"),
        role: role.to_owned(),
        kind: kind.to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    };
    koru_projection::ProjectedNode {
        attrs: vec![
            attr("koru.task", "bool", "true"),
            attr("koru.rank", "number", &rank.to_string()),
        ],
        ..plain(id, text)
    }
}

fn projection(
    workspace: WorkspaceId,
    document: DocumentId,
    nodes: Vec<koru_projection::ProjectedNode>,
) -> DocumentProjection {
    DocumentProjection {
        workspace,
        document,
        source_revision: Vec::new(),
        nodes,
    }
}

/// How many task notes each delivery carried, and for whom.
fn task_notes(delivered: &Arc<Mutex<Vec<Vec<FeedNote>>>>) -> Vec<String> {
    delivered
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .filter_map(|note| match note {
            FeedNote::TasksChanged { workspace } => Some(workspace.clone()),
            _ => None,
        })
        .collect()
}

/// The answer moved, said once per workspace per tick: a task moved in its
/// list says so wherever the change came from; the same projection again and
/// a paragraph somebody is typing say nothing.
#[test]
fn a_task_that_moved_marks_its_workspace_and_plain_text_does_not() {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut feed = feed_into(&delivered);
    let (workspace, document) = (WorkspaceId::new(), DocumentId::new());
    let body = |rank, words: &str| {
        projection(
            workspace,
            document,
            vec![plain("p", words), task("t", "Buy milk", rank)],
        )
    };

    // First sight of a document that holds a task: it may have just changed.
    feed.projected(&body(10, "notes"));
    assert!(task_notes(&delivered).is_empty(), "nothing before the tick");
    feed.flush_pending();
    assert_eq!(task_notes(&delivered), vec![workspace.to_string()]);

    // The same rows again, and then only the paragraph beside them.
    feed.projected(&body(10, "notes"));
    feed.projected(&body(10, "notes, and more of them"));
    feed.flush_pending();
    assert_eq!(task_notes(&delivered).len(), 1, "typing moves no task");

    // Dragged to another place in its list.
    feed.projected(&body(5, "notes, and more of them"));
    feed.flush_pending();
    assert_eq!(task_notes(&delivered).len(), 2);

    // Ticked: a node field, not an attribute, and still a row that changed.
    let mut ticked = body(5, "notes, and more of them");
    ticked.nodes[1].done = true;
    feed.projected(&ticked);
    feed.flush_pending();
    assert_eq!(task_notes(&delivered).len(), 3);
}

/// A renumbered list is many documents and one event; a document with no
/// task in it never marks anything; a dropped document that held tasks does.
#[test]
fn many_documents_in_one_tick_are_one_note_and_a_taskless_document_is_none() {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut feed = feed_into(&delivered);
    let workspace = WorkspaceId::new();
    let documents: Vec<DocumentId> = (0..40).map(|_| DocumentId::new()).collect();
    for (i, document) in documents.iter().enumerate() {
        feed.projected(&projection(
            workspace,
            *document,
            vec![task("t", "A task", i as i64)],
        ));
    }
    feed.flush_pending();
    assert_eq!(task_notes(&delivered), vec![workspace.to_string()]);

    let elsewhere = WorkspaceId::new();
    let prose = DocumentId::new();
    feed.projected(&projection(elsewhere, prose, vec![plain("p", "prose")]));
    feed.flush_pending();
    assert_eq!(task_notes(&delivered).len(), 1, "no task, no event");
    feed.removed(elsewhere, prose);
    feed.flush_pending();
    assert_eq!(task_notes(&delivered).len(), 1, "and none for dropping it");

    feed.removed(workspace, documents[0]);
    feed.flush_pending();
    assert_eq!(
        task_notes(&delivered),
        vec![workspace.to_string(), workspace.to_string()],
        "a document that held a task took it along"
    );
}

/// A journal that is briefly away loses nothing and repeats nothing: the
/// note waits, coalesced, for the next tick.
#[test]
fn a_tasks_note_the_journal_missed_is_delivered_once_on_a_later_tick() {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let mut feed = feed_into(&delivered);
    feed.sink = Box::new(FlakySink {
        // `projected` delivers its own note, then the tick's flush: both fail.
        failures_left: AtomicUsize::new(2),
        delivered: Arc::clone(&delivered),
    });
    let (workspace, document) = (WorkspaceId::new(), DocumentId::new());
    feed.projected(&projection(workspace, document, vec![task("t", "A", 1)]));
    feed.flush_pending();
    assert!(task_notes(&delivered).is_empty());
    feed.flush_pending();
    assert_eq!(task_notes(&delivered), vec![workspace.to_string()]);
    feed.flush_pending();
    assert_eq!(task_notes(&delivered).len(), 1);
}
