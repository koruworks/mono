// SPDX-License-Identifier: MPL-2.0

//! Real SQLite boundaries: a removal cursor cannot outrun SystemDoc content,
//! even when workspace commit, receipt writes, or the final cursor fail.

use super::*;
use client_store::removal;
use koru_core::{SystemDoc, pages::purge::Event};
use transfer::removal::{Entry, State};
use uuid::Uuid;

struct RemovalFixture {
    store: Store<crate::SqliteCatalog, MemoryWorkspace>,
    workspace_control: Connection,
    catalog_control: Connection,
    catalog_name: String,
    uri: String,
    workspace: WorkspaceId,
    document: DocumentId,
    scope: Scope,
}

impl RemovalFixture {
    fn new() -> Self {
        let catalog_name = format!("removal-catalog-{}", DocumentId::new());
        let catalog = crate::SqliteCatalog::shared_in_memory(&catalog_name).unwrap();
        let catalog_control = crate::db::open_shared_in_memory(&catalog_name).unwrap();
        let uri = format!(
            "file:removal-workspace-{}?mode=memory&cache=shared",
            DocumentId::new()
        );
        let workspace_control = connect(&uri);
        let mut store = Store::open(catalog, MemoryWorkspace(uri.clone())).unwrap();
        let profile = store
            .create_profile("Owner", None, None, Locale::default(), at())
            .unwrap()
            .id;
        let workspace = store
            .create_workspace(profile, "Workspace", at())
            .unwrap()
            .id;
        let document = store
            .create_document(workspace, "Document", "page", None, None, at())
            .unwrap()
            .0;
        let manifest = store.manifest_id(workspace).unwrap();
        store.flush(workspace, document, at()).unwrap();
        store.flush(workspace, manifest, at()).unwrap();
        let scope = Scope {
            principal: Dataset::Workspace {
                workspace_id: CloudId::from_bytes(workspace.as_uuid().into_bytes()),
                incarnation_id: CloudId::from_bytes([6; 16]),
                logical_replica_id: CloudId::from_bytes([7; 16]),
            },
            eligibility_workspace: workspace,
            profile: CloudId::from_bytes(profile.as_uuid().into_bytes()),
            manifest,
        };
        Self {
            store,
            workspace_control,
            catalog_control,
            catalog_name,
            uri,
            workspace,
            document,
            scope,
        }
    }

    fn reopen(self) -> Self {
        let Self {
            store,
            workspace_control,
            catalog_control,
            catalog_name,
            uri,
            workspace,
            document,
            scope,
        } = self;
        drop(store);
        let store = Store::open(
            crate::SqliteCatalog::shared_in_memory(&catalog_name).unwrap(),
            MemoryWorkspace(uri.clone()),
        )
        .unwrap();
        Self {
            store,
            workspace_control,
            catalog_control,
            catalog_name,
            uri,
            workspace,
            document,
            scope,
        }
    }

    fn incarnation(&self) -> Uuid {
        incarnation(self.scope)
    }

    fn event(&self, peer: u64) -> Event {
        let mut version = loro::VersionVector::new();
        version.insert(peer, 3);
        Event::new(
            self.document.as_uuid(),
            self.document.as_uuid(),
            Some(&version),
            "other device",
        )
    }

    fn system(&mut self, id: DocumentId, events: &[Event]) {
        let mut system = SystemDoc::create().unwrap();
        for event in events {
            system.write_purge_event(event).unwrap();
        }
        self.store
            .import_document_at(
                self.workspace,
                id,
                "",
                koru_core::explore::KIND_SYSTEM,
                None,
                None,
                system.export_snapshot().unwrap(),
                at(),
            )
            .unwrap();
    }

    fn consume(&mut self, entries: &[Entry], cursor: u64) -> std::result::Result<(), Failure> {
        removal::consume(&mut self.store, self.scope, entries, cursor, at())
    }

    fn cursor(&mut self) -> u64 {
        let incarnation = self.incarnation();
        removal::cursor(&mut self.store, self.workspace, incarnation).unwrap()
    }

    fn pending(&mut self) -> Vec<Event> {
        let incarnation = self.incarnation();
        removal::pending(&mut self.store, self.workspace, incarnation).unwrap()
    }

    fn persisted_events(&mut self, system: DocumentId) -> Vec<Event> {
        SystemDoc::from_snapshot(&self.store.snapshot(self.workspace, system).unwrap())
            .unwrap()
            .purge_events()
            .unwrap()
    }

    fn placeholder(&mut self, document: DocumentId) {
        let snapshot = self
            .store
            .snapshot(self.workspace, self.scope.manifest)
            .unwrap();
        let mut manifest = koru_store_core::manifest::Manifest::from_snapshot(&snapshot).unwrap();
        manifest
            .add_document(document, "", koru_core::explore::KIND_SYSTEM, None)
            .unwrap();
        self.store
            .apply_update(
                self.workspace,
                self.scope.manifest,
                manifest.export_snapshot().unwrap(),
                at(),
                self.scope.principal.update_origin(),
            )
            .unwrap();
        self.store
            .flush(self.workspace, self.scope.manifest, at())
            .unwrap();
    }
}

fn incarnation(scope: Scope) -> Uuid {
    let Dataset::Workspace { incarnation_id, .. } = scope.principal else {
        panic!("workspace scope");
    };
    Uuid::from_bytes(*incarnation_id.as_bytes())
}

fn id(text: &str) -> DocumentId {
    DocumentId::parse(text).unwrap()
}

fn entry(sequence: u64, event: &Event) -> Entry {
    Entry {
        sequence,
        document_id: event.document_id,
        state: State::Deleting,
        event: event.clone(),
    }
}

#[test]
fn removal_read_creates_first_system_and_receipts_survive_reopen_without_cloud_write_permission() {
    let mut f = RemovalFixture::new();
    assert!(removal::load(&mut f.store, f.workspace).unwrap().is_none());
    let event = f.event(1);
    // List consumption is an inbound local operation: no outgoing stream,
    // write ticket, or existing system document is needed on a paused device.
    f.consume(&[entry(7, &event)], 7).unwrap();
    let system = removal::load(&mut f.store, f.workspace)
        .unwrap()
        .unwrap()
        .document;
    f = f.reopen();
    assert_eq!(f.persisted_events(system), vec![event.clone()]);
    assert_eq!(f.cursor(), 7);
    assert!(f.pending().is_empty());
    assert!(
        f.store
            .document_removal_received(f.workspace, f.incarnation(), system, event.event_id)
            .unwrap()
    );
    let repository = SqliteWorkspace::from_connection(connect(&f.uri)).unwrap();
    assert!(
        repository
            .updates_after(system, None)
            .unwrap()
            .iter()
            .any(|update| update.origin == f.scope.principal.update_origin())
    );
}

#[test]
fn failed_system_flush_never_earns_a_receipt_or_cursor() {
    let mut f = RemovalFixture::new();
    let system = id("f0000000-0000-4000-8000-000000000001");
    f.system(system, &[]);
    let event = f.event(1);
    f.workspace_control.execute_batch("CREATE TRIGGER deny_removal_commit BEFORE UPDATE OF version_vector ON loro_documents BEGIN SELECT RAISE(ABORT, 'injected removal commit failure'); END;").unwrap();
    assert!(f.consume(&[entry(1, &event)], 1).is_err());
    f = f.reopen();
    // Opening a document can write its local metadata. End the injected
    // failure before inspection, after the failed Store owner was dropped.
    f.workspace_control
        .execute_batch("DROP TRIGGER deny_removal_commit")
        .unwrap();
    assert_eq!(f.cursor(), 0);
    assert!(f.persisted_events(system).is_empty());
    assert!(
        !f.store
            .document_removal_received(f.workspace, f.incarnation(), system, event.event_id)
            .unwrap()
    );
    f.consume(&[entry(1, &event)], 1).unwrap();
    f = f.reopen();
    assert_eq!(f.persisted_events(system), vec![event]);
    assert_eq!(f.cursor(), 1);
}

#[test]
fn failed_cursor_write_reopens_with_durable_events_and_replays_the_unconsumed_page() {
    let mut f = RemovalFixture::new();
    let system = id("f0000000-0000-4000-8000-000000000001");
    f.system(system, &[]);
    let event = f.event(1);
    f.catalog_control.execute_batch("CREATE TRIGGER deny_removal_cursor BEFORE INSERT ON store_state WHEN NEW.key LIKE 'document_removal.cursor.%' BEGIN SELECT RAISE(ABORT, 'injected cursor failure'); END;").unwrap();
    assert!(f.consume(&[entry(1, &event)], 1).is_err());
    f = f.reopen();
    assert_eq!(f.persisted_events(system), vec![event.clone()]);
    assert_eq!(f.cursor(), 0);
    assert_eq!(f.pending(), vec![event.clone()]);
    let receipts: i64 = f
        .catalog_control
        .query_row(
            "SELECT count(*) FROM store_state WHERE key LIKE 'document_removal.received.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        receipts, 1,
        "event content was flushed before the partial receipt"
    );
    f.catalog_control
        .execute_batch("DROP TRIGGER deny_removal_cursor")
        .unwrap();
    f.consume(&[entry(1, &event)], 1).unwrap();
    f = f.reopen();
    assert_eq!(f.cursor(), 1);
    assert!(f.pending().is_empty());
}

#[test]
fn receipt_failure_after_one_event_cannot_advance_the_page_cursor() {
    let mut f = RemovalFixture::new();
    let system = id("f0000000-0000-4000-8000-000000000001");
    f.system(system, &[]);
    let mut first = f.event(1);
    let mut second = f.event(2);
    first.event_id = Uuid::parse_str("10000000-0000-4000-8000-000000000001").unwrap();
    second.event_id = Uuid::parse_str("20000000-0000-4000-8000-000000000002").unwrap();
    f.catalog_control.execute_batch(&format!("CREATE TRIGGER deny_second_removal_receipt BEFORE INSERT ON store_state WHEN NEW.key = 'document_removal.received.{}.{}' BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END;", f.workspace, second.event_id)).unwrap();
    let page = [entry(1, &first), entry(2, &second)];
    assert!(f.consume(&page, 2).is_err());
    f = f.reopen();
    assert_eq!(
        f.persisted_events(system),
        vec![first.clone(), second.clone()]
    );
    assert_eq!(f.cursor(), 0);
    assert_eq!(f.pending().len(), 2);
    f.catalog_control
        .execute_batch("DROP TRIGGER deny_second_removal_receipt")
        .unwrap();
    f.consume(&page, 2).unwrap();
    f = f.reopen();
    assert_eq!(f.cursor(), 2);
    assert!(f.pending().is_empty());
}

#[test]
fn another_event_for_the_same_document_needs_its_own_list_receipt() {
    let mut f = RemovalFixture::new();
    let system = id("f0000000-0000-4000-8000-000000000001");
    let first = f.event(1);
    let second = f.event(2);
    f.system(system, &[first.clone(), second.clone()]);
    f.consume(&[entry(1, &first)], 1).unwrap();
    assert_eq!(f.pending(), vec![second.clone()]);
    f = f.reopen();
    assert_eq!(f.pending(), vec![second.clone()]);
    f.consume(&[entry(2, &second)], 2).unwrap();
    assert!(f.pending().is_empty());
}

#[test]
fn incarnation_or_canonical_system_replacement_invalidates_old_receipts() {
    let mut f = RemovalFixture::new();
    let original_system = id("f0000000-0000-4000-8000-000000000001");
    let event = f.event(1);
    f.system(original_system, &[event.clone()]);
    f.consume(&[entry(1, &event)], 1).unwrap();
    let old_scope = f.scope;
    if let Dataset::Workspace { incarnation_id, .. } = &mut f.scope.principal {
        *incarnation_id = CloudId::from_bytes([8; 16]);
    }
    assert_eq!(f.cursor(), 0);
    assert_eq!(f.pending(), vec![event.clone()]);
    f.consume(&[], 0).unwrap();
    f.scope = old_scope;
    assert_eq!(
        f.cursor(),
        0,
        "switching back cannot reuse the previous receipt generation"
    );
    f.consume(&[entry(1, &event)], 1).unwrap();
    let replacement = id("10000000-0000-4000-8000-000000000001");
    f.system(replacement, &[]);
    assert_eq!(f.cursor(), 0);
    assert_eq!(f.pending(), vec![event.clone()]);
    f.consume(&[entry(1, &event)], 1).unwrap();
    f = f.reopen();
    assert_eq!(f.persisted_events(replacement), vec![event]);
    assert_eq!(f.cursor(), 1);
    assert!(f.pending().is_empty());
}

#[test]
fn an_empty_page_persists_the_union_of_duplicate_system_documents_before_its_cursor() {
    let mut f = RemovalFixture::new();
    let canonical = id("10000000-0000-4000-8000-000000000001");
    let duplicate = id("f0000000-0000-4000-8000-000000000001");
    let a = f.event(1);
    let b = f.event(2);
    f.system(canonical, &[a.clone()]);
    f.system(duplicate, &[b.clone()]);
    assert_eq!(
        removal::load(&mut f.store, f.workspace)
            .unwrap()
            .unwrap()
            .content
            .purge_events()
            .unwrap()
            .len(),
        2
    );
    f.consume(&[], 0).unwrap();
    f = f.reopen();
    let persisted = f.persisted_events(canonical);
    assert_eq!(persisted.len(), 2);
    assert!(persisted.contains(&a) && persisted.contains(&b));
}

#[test]
fn malformed_page_or_changed_event_identity_leaves_cursor_and_content_unchanged() {
    let mut f = RemovalFixture::new();
    let system = id("f0000000-0000-4000-8000-000000000001");
    let first = f.event(1);
    f.system(system, &[]);
    f.consume(&[entry(1, &first)], 1).unwrap();
    let second = f.event(2);
    let mut changed = first.clone();
    changed.version = None;
    let mut mismatched = entry(2, &second);
    mismatched.event.document_id = f.scope.manifest.as_uuid();
    let mut invalid_actor = second.clone();
    invalid_actor.by = "x".repeat(129);
    let malformed = [
        (vec![entry(2, &second), entry(3, &changed)], 3),
        (vec![mismatched], 2),
        (vec![entry(0, &second)], 2),
        (vec![entry(2, &second)], 3),
        (vec![entry(2, &second), entry(2, &second)], 2),
        (vec![entry(2, &invalid_actor)], 2),
    ];
    for (page, cursor) in malformed {
        assert!(f.consume(&page, cursor).is_err());
        f = f.reopen();
        assert_eq!(f.cursor(), 1);
        assert_eq!(f.persisted_events(system), vec![first.clone()]);
        assert!(f.pending().is_empty());
    }
}

#[test]
fn malformed_first_page_does_not_create_a_system_document() {
    let mut f = RemovalFixture::new();
    let mut event = f.event(1);
    event.event_id = Uuid::nil();
    assert!(f.consume(&[entry(1, &event)], 1).is_err());
    f = f.reopen();
    assert!(removal::load(&mut f.store, f.workspace).unwrap().is_none());
    assert_eq!(f.cursor(), 0);
}

#[test]
fn unhydrated_system_placeholders_do_not_break_fresh_sync_or_document_export() {
    let mut f = RemovalFixture::new();
    let placeholder = id("10000000-0000-4000-8000-000000000001");
    f.placeholder(placeholder);
    assert!(removal::load(&mut f.store, f.workspace).unwrap().is_none());
    assert!(f.pending().is_empty());
    assert_eq!(f.cursor(), 0);
    // The manifest can arrive before either the document row or its content.
    f.store
        .provision_replica_document(f.workspace, placeholder, at())
        .unwrap();
    assert!(removal::load(&mut f.store, f.workspace).unwrap().is_none());
    let source = LoroDoc::new();
    source
        .get_text("ordinary-content")
        .insert(0, "local work")
        .unwrap();
    source.commit();
    f.store
        .apply_update(
            f.workspace,
            f.document,
            source.export(ExportMode::all_updates()).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    let unit = Unit::Document {
        workspace_id: CloudId::from_bytes(f.workspace.as_uuid().into_bytes()),
        document_id: CloudId::from_bytes(f.document.as_uuid().into_bytes()),
    };
    let budget = MemoryBudget::new(2 * transfer::MAX_PAYLOAD);
    let export =
        client_store::ExportRequest::new(f.scope, unit, loro::VersionVector::new(), budget)
            .unwrap()
            .run(&mut f.store, at(), |_, _, _| Ok(()))
            .unwrap();
    assert!(
        export.is_some(),
        "a missing system snapshot must not block ordinary sync"
    );
    let event = f.event(1);
    f.consume(&[entry(1, &event)], 1).unwrap();
    f = f.reopen();
    assert_eq!(f.cursor(), 1);
    assert!(f.pending().is_empty());
}

#[test]
fn loaded_system_is_used_until_lower_placeholder_arrives_then_both_are_merged() {
    let mut f = RemovalFixture::new();
    let placeholder = id("10000000-0000-4000-8000-000000000001");
    let loaded = id("f0000000-0000-4000-8000-000000000001");
    let a = f.event(1);
    let b = f.event(2);
    f.system(loaded, &[a.clone()]);
    f.placeholder(placeholder);
    assert_eq!(
        removal::load(&mut f.store, f.workspace)
            .unwrap()
            .unwrap()
            .document,
        loaded
    );
    f.store
        .provision_replica_document(f.workspace, placeholder, at())
        .unwrap();
    assert_eq!(
        removal::load(&mut f.store, f.workspace)
            .unwrap()
            .unwrap()
            .document,
        loaded
    );
    let mut remote = SystemDoc::create().unwrap();
    remote.write_purge_event(&b).unwrap();
    f.store
        .apply_update(
            f.workspace,
            placeholder,
            remote.export_snapshot().unwrap(),
            at(),
            f.scope.principal.update_origin(),
        )
        .unwrap();
    f.store.flush(f.workspace, placeholder, at()).unwrap();
    let merged = removal::load(&mut f.store, f.workspace).unwrap().unwrap();
    assert_eq!(merged.document, placeholder);
    assert_eq!(merged.content.purge_events().unwrap().len(), 2);
    f.consume(&[], 0).unwrap();
    f = f.reopen();
    assert_eq!(f.persisted_events(placeholder).len(), 2);
}

#[test]
fn malformed_durable_system_evidence_is_an_error_not_an_unhydrated_placeholder() {
    let mut f = RemovalFixture::new();
    let system = id("f0000000-0000-4000-8000-000000000001");
    let raw = LoroDoc::new();
    raw.import(&SystemDoc::create().unwrap().export_snapshot().unwrap())
        .unwrap();
    raw.get_map("pages")
        .ensure_mergeable_map("purged")
        .unwrap()
        .ensure_mergeable_map(&f.document.to_storage())
        .unwrap()
        .insert("bad-event", "malformed")
        .unwrap();
    raw.commit();
    f.store
        .import_document_at(
            f.workspace,
            system,
            "",
            koru_core::explore::KIND_SYSTEM,
            None,
            None,
            raw.export(ExportMode::Snapshot).unwrap(),
            at(),
        )
        .unwrap();
    f = f.reopen();
    assert!(removal::load(&mut f.store, f.workspace).is_err());
    let incarnation = f.incarnation();
    assert!(removal::pending(&mut f.store, f.workspace, incarnation).is_err());
}
