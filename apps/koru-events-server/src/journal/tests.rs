// SPDX-License-Identifier: MPL-2.0

use koru_events::{
    AttrValue, Draft, EmitContext, Envelope, EventId, EventName, ImportCompleted, ImportOutcome,
    ImportSource, TerminalExperienceSummary, bucket, event_name,
};

use super::*;

/// A delegated run's rows through the journal: dedupe, amendment, retention
/// (P1.8).
mod delegate;

const T0: i64 = 1_756_000_000_000;

fn ctx() -> EmitContext {
    EmitContext::system("test")
}

fn imported(id: u128) -> Envelope {
    Draft::from(ImportCompleted {
        source: ImportSource::Markdown,
        document_count_bucket: bucket(3),
        outcome: ImportOutcome::Ok,
    })
    .build(&ctx(), EventId(uuid::Uuid::from_u128(id)), T0)
}

fn terminal_experience(id: u128) -> Envelope {
    Draft::from(TerminalExperienceSummary {
        delta_present_p95_ms_bucket: Some(bucket(37)),
        render_cpu_p95_ms_bucket: Some(bucket(9)),
        stream_gap_count_bucket: Some(bucket(1)),
        sample_count_bucket: Some(bucket(500)),
        ..Default::default()
    })
    .build(&ctx(), EventId(uuid::Uuid::from_u128(id)), T0)
}

#[test]
fn append_dedupes_by_event_id() {
    let mut journal = Journal::in_memory().unwrap();
    let event = imported(1);
    let first = journal.append(std::slice::from_ref(&event), T0).unwrap();
    assert_eq!((first.accepted, first.duplicates), (1, 0));
    let second = journal
        .append(std::slice::from_ref(&event), T0 + 5)
        .unwrap();
    assert_eq!((second.accepted, second.duplicates), (0, 1));
    assert_eq!(journal.last_seq().unwrap(), 1);
}

#[test]
fn local_only_append_is_durable_without_a_telemetry_outbox() {
    let mut journal = Journal::in_memory().unwrap();
    journal.set_consent(true, "consent-1").unwrap();
    let event = imported(101);
    let receipt = journal
        .append_local(std::slice::from_ref(&event), T0)
        .unwrap();
    assert_eq!(receipt.accepted, 1);
    assert_eq!(
        journal.read_after(0, 10, None, None).unwrap().events[0].envelope,
        event
    );
    assert!(journal.telemetry_pending(10).unwrap().is_empty());
    assert_eq!(journal.meta("installation_id").unwrap(), None);
}

#[test]
fn a_rejected_event_rejects_alone() {
    let mut journal = Journal::in_memory().unwrap();
    let good = imported(2);
    let bad = Draft::new(EventName::new("document.sneezed")).build(
        &ctx(),
        EventId(uuid::Uuid::from_u128(3)),
        T0,
    );
    let receipt = journal.append(&[bad, good], T0).unwrap();
    assert_eq!(receipt.accepted, 1);
    assert_eq!(receipt.rejected.len(), 1);
    assert_eq!(receipt.rejected[0].index, 0);
    assert_eq!(receipt.rejected[0].code, "unknown_event");
}

#[test]
fn read_after_pages_by_cursor_and_filters_by_name() {
    let mut journal = Journal::in_memory().unwrap();
    let session = Draft::new(event_name!("app.session.started"))
        .attr("previous_session_clean", AttrValue::Token("clean".into()))
        .build(&ctx(), EventId(uuid::Uuid::from_u128(4)), T0);
    let import = imported(5);
    journal.append(&[import.clone(), session], T0).unwrap();
    let all = journal.read_after(0, 100, None, None).unwrap();
    assert_eq!(all.events.len(), 2);
    assert_eq!(all.next_after, 2);
    let rest = journal.read_after(1, 100, None, None).unwrap();
    assert_eq!(rest.events.len(), 1);
    let apps = journal.read_after(0, 100, Some("app."), None).unwrap();
    assert_eq!(apps.events.len(), 1);
    assert_eq!(apps.events[0].envelope.name.as_str(), "app.session.started");
    // A stored event round-trips whole.
    assert_eq!(all.events[0].envelope, import);
}

fn reminder_notes(profile: &str) -> Vec<FeedNote> {
    [
        ReminderWake::Rule,
        ReminderWake::Action,
        ReminderWake::History,
        ReminderWake::Settings,
    ]
    .into_iter()
    .map(|change| FeedNote::ReminderChanged {
        profile: profile.into(),
        change,
    })
    .collect()
}

#[test]
fn reminder_due_and_entity_feed_events_are_content_free_and_cursor_replay_once() {
    let mut journal = Journal::in_memory().unwrap();
    let profile = "11111111-1111-4111-8111-111111111111";
    let workspace = "22222222-2222-4222-8222-222222222222";
    let document = "33333333-3333-4333-8333-333333333333";
    let entity = "44444444-4444-4444-8444-444444444444";
    journal
        .seed_baseline(&[BaselineEntry {
            workspace: workspace.into(),
            document: document.into(),
            revision: Vec::new(),
            trashed: false,
        }])
        .unwrap();
    let mut notes = reminder_notes(profile);
    notes.push(FeedNote::DictionaryChanged {
        profile: profile.into(),
    });
    notes.push(FeedNote::DueChanged {
        workspace: workspace.into(),
        document: document.into(),
        entity: entity.into(),
    });
    notes.push(FeedNote::EntityRemoved {
        workspace: workspace.into(),
        document: document.into(),
        entity: entity.into(),
    });

    let derived = journal.apply_notes(&notes, &ctx(), T0).unwrap();
    assert_eq!(
        derived
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>(),
        [
            "reminder.rule.changed",
            "reminder.action.appended",
            "reminder.history.cleared",
            "reminder.settings.changed",
            "profile.dictionary.changed",
            "due.changed",
            "entity.removed",
        ]
    );
    for event in &derived[..5] {
        assert_eq!(event.scope.profile_id.unwrap().to_string(), profile);
        assert!(event.scope.workspace_id.is_none());
        assert!(event.subject.is_none());
        assert!(event.attrs.is_empty());
    }
    for event in &derived[5..] {
        assert_eq!(event.scope.workspace_id.unwrap().to_string(), workspace);
        assert_eq!(event.subject.as_ref().unwrap().id, document);
        assert_eq!(event.attrs.len(), 1);
        assert_eq!(
            event.attrs["entity_id"],
            AttrValue::Id(entity.parse().unwrap())
        );
    }
    assert!(journal.telemetry_pending(100).unwrap().is_empty());

    let page = journal.read_after(0, 100, None, None).unwrap();
    assert_eq!(page.events.len(), 7);
    assert!(
        journal
            .read_after(page.next_after, 100, None, None)
            .unwrap()
            .events
            .is_empty()
    );
    let duplicate = journal
        .append(std::slice::from_ref(&derived[0]), T0 + 1)
        .unwrap();
    assert_eq!((duplicate.accepted, duplicate.duplicates), (0, 1));
}

#[test]
fn new_feed_events_stay_quiet_before_baseline_and_malformed_ids_are_skipped() {
    let mut journal = Journal::in_memory().unwrap();
    let mut valid = reminder_notes("11111111-1111-4111-8111-111111111111");
    valid.push(FeedNote::DueChanged {
        workspace: "22222222-2222-4222-8222-222222222222".into(),
        document: "33333333-3333-4333-8333-333333333333".into(),
        entity: "44444444-4444-4444-8444-444444444444".into(),
    });
    valid.push(FeedNote::EntityRemoved {
        workspace: "22222222-2222-4222-8222-222222222222".into(),
        document: "33333333-3333-4333-8333-333333333333".into(),
        entity: "44444444-4444-4444-8444-444444444444".into(),
    });
    assert!(journal.apply_notes(&valid, &ctx(), T0).unwrap().is_empty());
    assert_eq!(journal.last_seq().unwrap(), 0);

    journal.seed_baseline(&[]).unwrap();
    let malformed = [
        FeedNote::ReminderChanged {
            profile: "not-a-profile".into(),
            change: ReminderWake::Rule,
        },
        FeedNote::DictionaryChanged {
            profile: "not-a-profile".into(),
        },
        FeedNote::DueChanged {
            workspace: "not-a-workspace".into(),
            document: "not-a-document".into(),
            entity: "not-an-entity".into(),
        },
        FeedNote::EntityRemoved {
            workspace: "not-a-workspace".into(),
            document: "not-a-document".into(),
            entity: "not-an-entity".into(),
        },
    ];
    assert!(
        journal
            .apply_notes(&malformed, &ctx(), T0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(journal.last_seq().unwrap(), 0);
}

/// **The far end of the fix for a Profile renamed on another device.** The
/// Store's imported-identity wake arrives here as a feed note and has to leave
/// as the same two events a local `PATCH /v1/profiles/{id}` publishes — a
/// surface must not have to know which device made the change. The profile's id
/// travels as an attr, not in the envelope's scope: `scope.profile_id` is the
/// emitting process's profile, not the record's.
#[test]
fn an_imported_profile_identity_becomes_the_same_events_a_local_edit_publishes() {
    let mut journal = Journal::in_memory().unwrap();
    let profile = "11111111-1111-4111-8111-111111111111";
    let notes = [
        FeedNote::ProfileIdentityChanged {
            profile: profile.into(),
            change: IdentityWake::Name,
        },
        FeedNote::ProfileIdentityChanged {
            profile: profile.into(),
            change: IdentityWake::Appearance,
        },
    ];
    // Before the baseline nothing is announced: an upgrade must not report
    // every profile it meets as freshly renamed.
    assert!(journal.apply_notes(&notes, &ctx(), T0).unwrap().is_empty());
    journal.seed_baseline(&[]).unwrap();

    let derived = journal.apply_notes(&notes, &ctx(), T0).unwrap();
    assert_eq!(
        derived
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>(),
        ["profile.renamed", "profile.appearance_changed"]
    );
    for event in &derived {
        assert_eq!(
            event.attrs["profile"],
            AttrValue::Id(profile.parse().unwrap())
        );
        // Content-free: no name, no icon, no colour. A surface refetches.
        assert_eq!(event.attrs.len(), 1);
        assert!(event.subject.is_none());
    }
    assert!(
        journal
            .apply_notes(
                &[FeedNote::ProfileIdentityChanged {
                    profile: "not-a-profile".into(),
                    change: IdentityWake::Name,
                }],
                &ctx(),
                T0,
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pruning_never_reuses_a_sequence() {
    let mut journal = Journal::in_memory().unwrap();
    journal.append(&[imported(6), imported(7)], T0).unwrap();
    assert_eq!(journal.last_seq().unwrap(), 2);
    // A century from now everything is past retention; the prune empties the
    // journal but the AUTOINCREMENT high-water mark survives.
    journal
        .prune_now(T0 + 100 * 365 * 24 * 60 * 60 * 1_000)
        .unwrap();
    assert_eq!(
        journal.read_after(0, 100, None, None).unwrap().events.len(),
        0
    );
    journal.append(&[imported(8)], T0).unwrap();
    let page = journal.read_after(0, 100, None, None).unwrap();
    assert_eq!(page.events.len(), 1);
    assert!(
        page.events[0].seq > 2,
        "a pruned seq must never come back; got {}",
        page.events[0].seq
    );
}

// ------------------------------------------------- the per-subject read

const WS: u128 = 0x5555_5555_5555_5555_5555_5555_5555_5555;
const DOC: u128 = 0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa;

fn task_at(node: u128) -> koru_events::TaskSubject {
    koru_events::TaskSubject {
        workspace: uuid::Uuid::from_u128(WS),
        document: uuid::Uuid::from_u128(DOC),
        node: uuid::Uuid::from_u128(node),
    }
}

fn task_completed(id: u128, node: u128) -> Envelope {
    Draft::from(koru_events::TaskCompleted {
        task: task_at(node),
        reason: koru_events::TaskCompletedReason::ExplicitComplete,
    })
    .build(&ctx(), EventId(uuid::Uuid::from_u128(id)), T0)
}

/// **A task's activity is that task's, exactly.** The filter is on both
/// halves of the subject and on the whole id — not a prefix — because two
/// tasks in one document differ only in the last segment, and a `LIKE` here
/// would hand one task's history to another. The limit is a bound the caller
/// cannot exceed, and the cursor continues where the page stopped.
#[test]
fn read_subject_is_bounded_and_matches_one_subject_exactly() {
    let mut journal = Journal::in_memory().unwrap();
    let mine = task_at(0xbbbb);
    let theirs = task_at(0xcccc);
    journal
        .append(
            &[
                task_completed(0x10, 0xbbbb),
                task_completed(0x11, 0xcccc),
                task_completed(0x12, 0xbbbb),
                task_completed(0x13, 0xbbbb),
                imported(0x14),
            ],
            T0,
        )
        .unwrap();

    let all = journal
        .read_subject(koru_events::SubjectKind::Task, &mine.subject().id, 0, 100)
        .unwrap();
    assert_eq!(all.events.len(), 3, "one task's three, and nobody else's");
    for stored in &all.events {
        assert_eq!(
            stored.envelope.subject.as_ref().unwrap().id,
            mine.subject().id
        );
    }
    assert_eq!(
        journal
            .read_subject(koru_events::SubjectKind::Task, &theirs.subject().id, 0, 100)
            .unwrap()
            .events
            .len(),
        1,
        "the neighbouring task keeps its own"
    );

    // Bounded, and the cursor picks up where the bound stopped.
    let first = journal
        .read_subject(koru_events::SubjectKind::Task, &mine.subject().id, 0, 2)
        .unwrap();
    assert_eq!(first.events.len(), 2);
    assert_eq!(first.next_after, first.events[1].seq);
    let rest = journal
        .read_subject(
            koru_events::SubjectKind::Task,
            &mine.subject().id,
            first.next_after,
            100,
        )
        .unwrap();
    assert_eq!(rest.events.len(), 1);
    assert_eq!(rest.events[0].envelope.name.as_str(), "task.completed");

    // An empty answer leaves the cursor alone, the read_after convention.
    let none = journal
        .read_subject(koru_events::SubjectKind::Task, "task:no:such:thing", 7, 100)
        .unwrap();
    assert!(none.events.is_empty());
    assert_eq!(none.next_after, 7);
}

/// The kind is part of a subject's identity (SPEC §3): a `task:` subject and
/// a document subject are different subjects even where the string is the
/// same, and the query says so rather than trusting the id to be unique.
#[test]
fn the_subject_kind_is_part_of_the_match() {
    let mut journal = Journal::in_memory().unwrap();
    let task = task_at(0xbbbb);
    let confusable = Draft::new(event_name!("document.created"))
        .subject(koru_events::Subject::document(task.subject().id.clone()))
        .build(&ctx(), EventId(uuid::Uuid::from_u128(0x20)), T0);
    journal
        .append(&[task_completed(0x21, 0xbbbb), confusable], T0)
        .unwrap();

    let as_task = journal
        .read_subject(koru_events::SubjectKind::Task, &task.subject().id, 0, 100)
        .unwrap();
    assert_eq!(as_task.events.len(), 1);
    assert_eq!(as_task.events[0].envelope.name.as_str(), "task.completed");

    let as_document = journal
        .read_subject(
            koru_events::SubjectKind::Document,
            &task.subject().id,
            0,
            100,
        )
        .unwrap();
    assert_eq!(as_document.events.len(), 1);
    assert_eq!(
        as_document.events[0].envelope.name.as_str(),
        "document.created"
    );
}

/// A task event round-trips whole — the subject kind included, which is the
/// half a string column loses if the two mappings ever disagree.
#[test]
fn a_task_event_round_trips_through_the_journal() {
    let mut journal = Journal::in_memory().unwrap();
    let event = task_completed(0x30, 0xbbbb);
    journal.append(std::slice::from_ref(&event), T0).unwrap();
    let page = journal.read_after(0, 100, None, None).unwrap();
    assert_eq!(page.events[0].envelope, event);
    assert_eq!(
        page.events[0].envelope.subject.as_ref().unwrap().kind,
        koru_events::SubjectKind::Task
    );
}

/// v1 exactly as the shipped build wrote it: no `workspace_state`, stamped
/// as version 1. Shared by the two migration tests, because a fixture that
/// drifts from what really shipped tests nothing.
const V1_SCHEMA: &str = "
    CREATE TABLE events (
      seq            INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id       TEXT NOT NULL UNIQUE,
      name           TEXT NOT NULL,
      class          TEXT NOT NULL,
      version        INTEGER NOT NULL,
      occurred_at_ms INTEGER NOT NULL,
      recorded_at_ms INTEGER NOT NULL,
      actor_kind     TEXT NOT NULL,
      actor_id       TEXT,
      actor_token    TEXT,
      device_id      TEXT,
      session_id     TEXT NOT NULL,
      profile_id     TEXT,
      workspace      TEXT,
      subject_kind   TEXT,
      subject_id     TEXT,
      correlation_id TEXT,
      causation_id   TEXT,
      attrs          TEXT NOT NULL
    );
    CREATE TABLE doc_state (
      workspace            TEXT NOT NULL,
      document             TEXT NOT NULL,
      revision             BLOB NOT NULL,
      trashed              INTEGER NOT NULL DEFAULT 0,
      last_updated_emit_ms INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY (workspace, document)
    );
    CREATE TABLE telemetry_outbox (
      seq          INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id     TEXT NOT NULL UNIQUE,
      body         TEXT NOT NULL,
      queued_at_ms INTEGER NOT NULL
    );
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    INSERT INTO meta (key, value) VALUES ('baseline_seeded', '1');
    PRAGMA user_version = 1;
";

/// **A journal stamped by an older build gains what it is missing.**
///
/// Every other test here starts from a *fresh* journal, which runs the whole
/// creation block and therefore cannot see a migration that was appended to
/// an already-shipped step. This one starts where a real machine does: a
/// database at v1, without `workspace_state`. Before the v2 step existed it
/// opened fine and then failed every feed note with "no such table" —
/// silently, because the feed logs a warning and carries on, so derived
/// document events just stopped.
#[test]
fn an_older_journal_gains_the_tables_it_lacks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    {
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(V1_SCHEMA)
            .unwrap();
    }

    let mut journal = Journal::open(dir.path()).unwrap();
    // The proof is not that it opened — it is that the feed works, which is
    // what the missing table actually broke.
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    let names: Vec<_> = derived.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["workspace.observed", "document.created"]);
    assert_eq!(
        journal.status().unwrap().tracked_documents,
        1,
        "the upgraded journal still tracks what it knew"
    );

    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        version, JOURNAL_SCHEMA_VERSION,
        "the journal is stamped with what it now holds"
    );
}

/// **An upgraded journal does not re-announce the workspaces it already
/// knew.** `workspace_state` arrived after `baseline_seeded` had been set on
/// real journals, so the seeding pass could never fill it — and every
/// workspace known for months announced itself as newly observed on its next
/// feed note. Seen live: two false sightings before the backfill existed,
/// with thirty-three more queued behind them.
#[test]
fn an_upgraded_journal_does_not_re_announce_known_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    {
        // A v1 journal that has been running: its baseline is sealed and it
        // knows documents in two workspaces.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO doc_state VALUES ('ws-1', 'doc-1', X'7231', 0, 0);
             INSERT INTO doc_state VALUES ('ws-2', 'doc-2', X'7231', 0, 0);",
        )
        .unwrap();
    }
    let mut journal = Journal::open(dir.path()).unwrap();

    // A workspace it already knew says nothing; only its document's edit is
    // news. (Also covers a journal already stamped v2 with the table left
    // empty — the v3 step repairs both from one place.)
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r2")], &ctx(), T0)
        .unwrap();
    let names: Vec<_> = derived.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["document.updated"], "ws-1 was not newly observed");

    // A workspace it genuinely has not seen still is.
    let derived = journal
        .apply_notes(
            &[FeedNote::Projected {
                workspace: "ws-new".into(),
                document: "doc-3".into(),
                revision: b"r1".to_vec(),
            }],
            &ctx(),
            T0,
        )
        .unwrap();
    let names: Vec<_> = derived.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["workspace.observed", "document.created"]);
}

#[test]
fn a_newer_schema_is_refused_not_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    drop(Journal::open(dir.path()).unwrap());
    {
        let conn = rusqlite::Connection::open(dir.path().join("events.db")).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    match Journal::open(dir.path()) {
        Err(JournalError::SchemaTooNew { found: 99, .. }) => {}
        Err(other) => panic!("expected SchemaTooNew, got {other}"),
        Ok(_) => panic!("a newer schema must be refused, not rewritten"),
    }
}

fn seeded_journal() -> Journal {
    let mut journal = Journal::in_memory().unwrap();
    journal.seed_baseline(&[]).unwrap();
    journal
}

#[test]
fn failed_derived_event_append_rolls_back_the_state_cursor_for_retry() {
    let mut journal = Journal::in_memory().unwrap();
    journal
        .seed_baseline(&[BaselineEntry {
            workspace: "ws-1".into(),
            document: "doc-1".into(),
            revision: b"r1".to_vec(),
            trashed: false,
        }])
        .unwrap();
    journal
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER fail_derived_event BEFORE INSERT ON events
             BEGIN SELECT RAISE(FAIL, 'injected event append failure'); END;",
        )
        .unwrap();
    let note = projected("doc-1", b"r2");

    assert!(
        journal
            .apply_notes(
                std::slice::from_ref(&note),
                &ctx(),
                T0 + UPDATED_THROTTLE_MS,
            )
            .is_err()
    );
    let revision: Vec<u8> = journal
        .conn
        .query_row(
            "SELECT revision FROM doc_state WHERE workspace = 'ws-1' AND document = 'doc-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, b"r1", "failed append advanced the diff cursor");
    assert_eq!(journal.status().unwrap().tracked_documents, 1);
    assert!(names(&journal).is_empty());

    journal
        .conn
        .execute_batch("DROP TRIGGER fail_derived_event")
        .unwrap();
    let derived = journal
        .apply_notes(&[note], &ctx(), T0 + UPDATED_THROTTLE_MS)
        .unwrap();
    assert_eq!(
        derived
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>(),
        ["document.updated"]
    );
    assert_eq!(journal.status().unwrap().tracked_documents, 1);
    assert_eq!(names(&journal), ["document.updated"]);
}

fn projected(document: &str, revision: &[u8]) -> FeedNote {
    FeedNote::Projected {
        workspace: "ws-1".into(),
        document: document.into(),
        revision: revision.to_vec(),
    }
}

fn names(journal: &Journal) -> Vec<String> {
    journal
        .read_after(0, 100, None, None)
        .unwrap()
        .events
        .iter()
        .map(|stored| stored.envelope.name.as_str().to_string())
        .collect()
}

#[test]
fn an_unseeded_journal_records_without_announcing() {
    let mut journal = Journal::in_memory().unwrap();
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    assert!(
        derived.is_empty(),
        "the created-storm guard: no baseline, no events"
    );
    // Sealing the baseline afterwards keeps what was recorded and starts
    // announcing only what is genuinely new.
    journal.seed_baseline(&[]).unwrap();
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    assert!(
        derived.is_empty(),
        "same revision is idempotent re-projection"
    );
    let derived = journal
        .apply_notes(&[projected("doc-2", b"r1")], &ctx(), T0)
        .unwrap();
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].name.as_str(), "document.created");
}

#[test]
fn seeding_is_idempotent_and_baseline_docs_stay_quiet() {
    let mut journal = Journal::in_memory().unwrap();
    let baseline = BaselineEntry {
        workspace: "ws-1".into(),
        document: "doc-1".into(),
        revision: b"r1".to_vec(),
        trashed: false,
    };
    assert!(
        journal
            .seed_baseline(std::slice::from_ref(&baseline))
            .unwrap()
    );
    assert!(
        !journal.seed_baseline(&[baseline]).unwrap(),
        "a second seed is a no-op"
    );
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    assert!(derived.is_empty());
}

#[test]
fn a_baseline_document_first_projection_is_not_creation() {
    let mut journal = Journal::in_memory().unwrap();
    // Listed at seeding time but never yet projected: revision unknown.
    journal
        .seed_baseline(&[BaselineEntry {
            workspace: "ws-1".into(),
            document: "doc-1".into(),
            revision: Vec::new(),
            trashed: false,
        }])
        .unwrap();
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    assert!(
        derived.is_empty(),
        "it existed before the journal; projecting it is not news"
    );
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r2")], &ctx(), T0 + 1)
        .unwrap();
    assert_eq!(derived.len(), 1, "a real edit after that is");
    assert_eq!(derived[0].name.as_str(), "document.updated");
}

#[test]
fn updated_is_throttled_per_document() {
    let mut journal = seeded_journal();
    journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    let derived = journal
        .apply_notes(
            &[projected("doc-1", b"r2")],
            &ctx(),
            T0 + UPDATED_THROTTLE_MS,
        )
        .unwrap();
    assert_eq!(derived[0].name.as_str(), "document.updated");
    // Inside the window: the revision advances silently.
    let derived = journal
        .apply_notes(
            &[projected("doc-1", b"r3")],
            &ctx(),
            T0 + UPDATED_THROTTLE_MS + 60_000,
        )
        .unwrap();
    assert!(
        derived.is_empty(),
        "one update event per document per window"
    );
    // Past the window it speaks again.
    let derived = journal
        .apply_notes(
            &[projected("doc-1", b"r4")],
            &ctx(),
            T0 + 2 * UPDATED_THROTTLE_MS + 60_000,
        )
        .unwrap();
    assert_eq!(derived[0].name.as_str(), "document.updated");
    assert_eq!(
        names(&journal),
        [
            // The workspace's own first sighting leads, once.
            "workspace.observed",
            "document.created",
            "document.updated",
            "document.updated"
        ]
    );
}

#[test]
fn trash_is_a_diff_not_a_repeat() {
    let mut journal = seeded_journal();
    journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    let listing = |trashed| FeedNote::Listing {
        workspace: "ws-1".into(),
        documents: vec![("doc-1".into(), trashed)],
    };
    let derived = journal.apply_notes(&[listing(true)], &ctx(), T0).unwrap();
    assert_eq!(derived[0].name.as_str(), "document.trashed");
    let derived = journal.apply_notes(&[listing(true)], &ctx(), T0).unwrap();
    assert!(derived.is_empty(), "an unchanged flag is not news");
    let derived = journal.apply_notes(&[listing(false)], &ctx(), T0).unwrap();
    assert_eq!(derived[0].name.as_str(), "document.restored");
}

/// A workspace no writer announced still becomes visible — the coverage the
/// app's own emit cannot have (a phone, an API client, a peer's sync).
#[test]
fn a_workspace_the_journal_has_never_seen_is_observed_once() {
    let mut journal = seeded_journal();
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    let names: Vec<_> = derived.iter().map(|event| event.name.as_str()).collect();
    assert_eq!(names, ["workspace.observed", "document.created"]);
    // A second note about the same workspace is not news.
    let derived = journal
        .apply_notes(&[projected("doc-2", b"r1")], &ctx(), T0)
        .unwrap();
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].name.as_str(), "document.created");
}

/// A workspace that already existed when the journal was created stays quiet
/// — the created-storm guard, one level up from documents.
#[test]
fn a_baseline_workspace_is_not_observed() {
    let mut journal = Journal::in_memory().unwrap();
    journal
        .seed_baseline(&[BaselineEntry {
            workspace: "ws-1".into(),
            document: "doc-1".into(),
            revision: b"r1".to_vec(),
            trashed: false,
        }])
        .unwrap();
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r2")], &ctx(), T0)
        .unwrap();
    assert_eq!(derived.len(), 1, "only the edit is news");
    assert_eq!(derived[0].name.as_str(), "document.updated");
}

/// Permanent deletion edits the manifest, so the listing note is the only
/// immediate word of it — a document the listing stops naming is removed.
#[test]
fn a_document_gone_from_the_listing_is_removed() {
    let mut journal = seeded_journal();
    journal
        .apply_notes(
            &[projected("doc-1", b"r1"), projected("doc-2", b"r1")],
            &ctx(),
            T0,
        )
        .unwrap();
    let listing = FeedNote::Listing {
        workspace: "ws-1".into(),
        documents: vec![("doc-2".into(), false)],
    };
    let derived = journal.apply_notes(&[listing], &ctx(), T0).unwrap();
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].name.as_str(), "document.removed");
    assert_eq!(
        derived[0]
            .subject
            .as_ref()
            .map(|subject| subject.id.as_str()),
        Some("doc-1")
    );
    assert_eq!(
        journal.status().unwrap().tracked_documents,
        1,
        "doc-2 survives"
    );
}

#[test]
fn removal_forgets_the_document() {
    let mut journal = seeded_journal();
    journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    let removed = FeedNote::Removed {
        workspace: "ws-1".into(),
        document: "doc-1".into(),
    };
    let derived = journal
        .apply_notes(std::slice::from_ref(&removed), &ctx(), T0)
        .unwrap();
    assert_eq!(derived[0].name.as_str(), "document.removed");
    let derived = journal.apply_notes(&[removed], &ctx(), T0).unwrap();
    assert!(
        derived.is_empty(),
        "removing what is already gone is not news"
    );
    // The id coming back is a new document.
    let derived = journal
        .apply_notes(&[projected("doc-1", b"r9")], &ctx(), T0)
        .unwrap();
    assert_eq!(derived[0].name.as_str(), "document.created");
}

#[test]
fn the_outbox_obeys_consent() {
    let mut journal = Journal::in_memory().unwrap();
    journal.append(&[imported(10)], T0).unwrap();
    assert!(
        journal.telemetry_pending(100).unwrap().is_empty(),
        "consent is opt-in, so an installation nobody has asked spools nothing"
    );

    journal.set_consent(false, "consent-1").unwrap();
    journal.append(&[imported(99)], T0).unwrap();
    assert!(
        journal.telemetry_pending(100).unwrap().is_empty(),
        "and an explicit refusal spools nothing either"
    );

    journal.set_consent(true, "consent-1").unwrap();
    journal.append(&[imported(11)], T0).unwrap();
    let pending = journal.telemetry_pending(100).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        !serde_json::to_string(&pending[0].properties)
            .unwrap()
            .contains("ws-"),
        "no reference survives projection"
    );

    journal.set_consent(false, "consent-1").unwrap();
    assert!(
        journal.telemetry_pending(100).unwrap().is_empty(),
        "withdrawing consent deletes the spool"
    );
    let status = journal.status().unwrap();
    assert!(!status.telemetry_consent);
    assert_eq!(status.outbox_pending, 0);
}

/// Consent is **opt-in** (the 2026-10-11 contract amending ADR-176): nothing
/// spools before a yes, a yes spools, withdrawing empties what was queued and
/// stops it, and granting again resumes with new events only.
#[test]
fn terminal_experience_obeys_consent_and_withdrawal() {
    let mut journal = Journal::in_memory().unwrap();

    journal.append(&[terminal_experience(29)], T0).unwrap();
    assert!(
        journal.telemetry_pending(100).unwrap().is_empty(),
        "nobody has answered, so nothing spools"
    );

    journal.set_consent(true, "consent-1").unwrap();
    journal.append(&[terminal_experience(30)], T0).unwrap();
    let pending = journal.telemetry_pending(100).unwrap();
    assert_eq!(pending.len(), 1, "after the yes, the new event spools");
    assert_eq!(pending[0].name, "terminal.experience.summary");

    journal.set_consent(false, "consent-1").unwrap();
    assert!(
        journal.telemetry_pending(100).unwrap().is_empty(),
        "withdrawing consent deletes the spool"
    );
    journal.append(&[terminal_experience(31)], T0).unwrap();
    assert!(
        journal.telemetry_pending(100).unwrap().is_empty(),
        "and nothing appended after it is exported"
    );

    journal.set_consent(true, "consent-1").unwrap();
    journal.append(&[terminal_experience(32)], T0).unwrap();
    let pending = journal.telemetry_pending(100).unwrap();
    assert_eq!(pending.len(), 1, "granting again resumes");
    assert_eq!(pending[0].name, "terminal.experience.summary");
}

#[test]
fn terminal_events_and_consent_are_isolated_by_root() {
    let parent = tempfile::tempdir().unwrap();
    let root_a = parent.path().join("root-a").join("events");
    let root_b = parent.path().join("root-b").join("events");
    let mut journal_a = Journal::open(&root_a).unwrap();
    let mut journal_b = Journal::open(&root_b).unwrap();

    journal_a.set_consent(true, "consent-a").unwrap();
    // Isolation is shown by two answers that stay where they were made:
    // root A said yes and spools, root B said no and does not.
    journal_b.set_consent(false, "consent-b").unwrap();
    journal_a.append(&[terminal_experience(32)], T0).unwrap();

    assert_eq!(
        journal_a
            .read_after(0, 10, None, None)
            .unwrap()
            .events
            .len(),
        1
    );
    assert_eq!(journal_a.telemetry_pending(10).unwrap().len(), 1);
    assert!(
        journal_b
            .read_after(0, 10, None, None)
            .unwrap()
            .events
            .is_empty()
    );
    assert!(journal_b.telemetry_pending(10).unwrap().is_empty());
    // **Isolation is about the recorded answer.** Consent is per device and
    // per root: B's refusal stays in B, and A's grant — and the version string
    // it was recorded under — stays in A.
    assert!(!journal_b.consent().unwrap().granted);
    let consent_a = journal_a.consent().unwrap();
    assert!(consent_a.granted);
    assert_eq!(consent_a.consent_version, "consent-a");
    assert!(root_a.join("events.db").is_file());
    assert!(root_b.join("events.db").is_file());
}

#[test]
fn never_policy_events_never_reach_the_outbox() {
    let mut journal = seeded_journal();
    journal.set_consent(true, "consent-1").unwrap();
    journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    let listing = FeedNote::Listing {
        workspace: "ws-1".into(),
        documents: vec![("doc-1".into(), true)],
    };
    journal.apply_notes(&[listing], &ctx(), T0).unwrap();
    let pending = journal.telemetry_pending(100).unwrap();
    // workspace.observed and document.created are AnonymousUsage and
    // export; document.trashed is Never and must not, consent or no
    // consent.
    let exported: Vec<_> = pending.iter().map(|event| event.name.as_str()).collect();
    assert_eq!(exported, ["workspace.observed", "document.created"]);
}

#[test]
fn status_counts_what_it_holds() {
    let mut journal = seeded_journal();
    journal
        .apply_notes(&[projected("doc-1", b"r1")], &ctx(), T0)
        .unwrap();
    journal
        .append(
            &[Draft::new(event_name!("app.session.started"))
                .attr("previous_session_clean", AttrValue::Token("unknown".into()))
                .build(&ctx(), EventId(uuid::Uuid::from_u128(12)), T0)],
            T0,
        )
        .unwrap();
    let status = journal.status().unwrap();
    // workspace.observed, document.created, app.session.started.
    assert_eq!(status.total_events, 3);
    assert_eq!(status.domain_events, 2);
    assert_eq!(status.instrumentation_events, 1);
    assert_eq!(status.tracked_documents, 1);
    assert!(status.baseline_seeded);
    assert_eq!(status.last_seq, 3);
}

/// A fresh journal holds no answer: not granted, and unanswered — which is
/// what makes a surface ask. (ADR-176 read this same state as granted; the
/// opt-in contract of 2026-10-11 reversed it.)
#[test]
fn a_fresh_journal_is_unanswered_and_not_granted() {
    let journal = Journal::in_memory().unwrap();
    let consent = journal.consent().unwrap();
    assert!(!consent.granted);
    assert!(!journal.consent_answered().unwrap());
    let status = journal.status().unwrap();
    assert!(!status.telemetry_consent);
    assert!(!status.telemetry_consent_answered);
}

#[test]
fn acking_removes_exactly_what_was_named() {
    let mut journal = Journal::in_memory().unwrap();
    journal.set_consent(true, "consent-1").unwrap();
    journal
        .append(&[imported(20), imported(21), imported(22)], T0)
        .unwrap();
    let pending = journal.telemetry_pending(100).unwrap();
    assert_eq!(pending.len(), 3);

    let removed = journal
        .telemetry_ack(&[pending[0].event_id.to_string()])
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(journal.telemetry_pending(100).unwrap().len(), 2);

    // Idempotent: acking the same id again, and one nothing knows about, are
    // both no-ops rather than errors — which is what makes a crash between
    // delivering and acking harmless.
    assert_eq!(
        journal
            .telemetry_ack(&[
                pending[0].event_id.to_string(),
                EventId(uuid::Uuid::from_u128(999)).to_string()
            ])
            .unwrap(),
        0
    );
    assert_eq!(journal.telemetry_ack(&[]).unwrap(), 0);
    assert_eq!(journal.telemetry_pending(100).unwrap().len(), 2);
}

#[test]
fn the_installation_id_is_minted_once_and_dies_with_consent() {
    let mut journal = Journal::in_memory().unwrap();
    let first = journal.installation_id().unwrap();
    assert_eq!(first, journal.installation_id().unwrap(), "stable");
    assert!(uuid::Uuid::parse_str(&first).is_ok(), "a v4 uuid");

    journal.set_consent(false, "consent-1").unwrap();
    let after = journal.installation_id().unwrap();
    assert_ne!(
        first, after,
        "opting out deletes it, so opting back in is a rotation and not a resumption"
    );
}

/// `projection.tasks.changed` is the workspace and nothing else: no task,
/// document or node rides on it, it never leaves as telemetry, it stays
/// quiet before the baseline is seeded like every derived wake, and a
/// workspace that is not an id derives nothing.
#[test]
fn a_tasks_changed_note_becomes_one_content_free_workspace_event() {
    let workspace = "22222222-2222-4222-8222-222222222222";
    let note = |workspace: &str| FeedNote::TasksChanged {
        workspace: workspace.into(),
    };

    let mut unseeded = Journal::in_memory().unwrap();
    assert!(
        unseeded
            .apply_notes(&[note(workspace)], &ctx(), T0)
            .unwrap()
            .is_empty(),
        "quiet until the baseline is seeded"
    );

    // A workspace the journal already knows, so its first sight is not what
    // this derives.
    let mut journal = Journal::in_memory().unwrap();
    journal
        .seed_baseline(&[BaselineEntry {
            workspace: workspace.into(),
            document: "33333333-3333-4333-8333-333333333333".into(),
            revision: Vec::new(),
            trashed: false,
        }])
        .unwrap();
    let derived = journal
        .apply_notes(&[note(workspace), note("not-an-id")], &ctx(), T0)
        .unwrap();
    assert_eq!(derived.len(), 1);
    let event = &derived[0];
    assert_eq!(event.name.as_str(), "projection.tasks.changed");
    assert_eq!(event.scope.workspace_id.unwrap().to_string(), workspace);
    assert_eq!(event.subject.as_ref().unwrap().id, workspace);
    assert!(event.attrs.is_empty());
    assert!(journal.telemetry_pending(100).unwrap().is_empty());

    // A reader filtering on the workspace finds it; another workspace's
    // reader does not.
    let mine = journal
        .read_after(0, 100, Some("projection.tasks."), Some(workspace))
        .unwrap();
    assert_eq!(mine.events.len(), 1);
    let theirs = journal
        .read_after(
            0,
            100,
            Some("projection.tasks."),
            Some("55555555-5555-4555-8555-555555555555"),
        )
        .unwrap();
    assert!(theirs.events.is_empty());
}
