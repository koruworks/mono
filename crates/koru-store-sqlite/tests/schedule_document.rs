// SPDX-License-Identifier: MPL-2.0

//! `koru-schedule` through two real SQLite Store roots.
//!
//! This is the Store-level P2.2 proof: manifest registration, durable
//! request/confirmation boundaries, restart, replication, and
//! retire-before-trash/restore use the same `Store<SqliteCatalog,
//! SqliteBackend>` as the product. The provider firing loop does not exist
//! until P2.3, so exactly-one-provider-fires remains deliberately there.

use std::collections::BTreeSet;

use koru_delegate_core::{
    ActorRef, DeviceId, HandoffId, Objective, ProfileId, RunId, ScheduleId, SessionProfileKey,
    WorkspaceScope,
};
use koru_delegate_document::{SCHEDULE_KIND, ScheduleClaimPlan, ScheduleDocument, ScheduleWrite};
use koru_delegate_schedule::{
    DelegationTemplate, ScheduleDefinition, ScheduleFiringOutcome, ScheduleHeader,
    ScheduleLifecycle,
};
use koru_recurrence::{CronExpression, RecurrenceRule};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::manifest::Manifest;
use koru_store_core::model::Timestamp;
use koru_store_core::service::Store;
use koru_store_core::text::Locale;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use loro::VersionVector;

type TestStore = Store<SqliteCatalog, SqliteBackend>;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

struct Root {
    dir: tempfile::TempDir,
    store: TestStore,
}

impl Root {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            store: Self::open(dir.path()),
            dir,
        }
    }

    fn open(root: &std::path::Path) -> TestStore {
        let catalog = SqliteCatalog::open(&root.join("catalog.db")).unwrap();
        Store::open(catalog, SqliteBackend::new(root)).unwrap()
    }

    fn restart(&mut self) {
        self.store = Self::open(self.dir.path());
    }

    fn workspace(&mut self) -> WorkspaceId {
        let profile = self
            .store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
            .unwrap();
        self.store
            .create_workspace(profile.id, "Workspace", at(0))
            .unwrap()
            .id
    }

    fn open_schedule(&mut self, workspace: WorkspaceId, document: DocumentId) -> ScheduleDocument {
        ScheduleDocument::from_snapshot(&self.store.snapshot(workspace, document).unwrap()).unwrap()
    }

    fn pull(&mut self, from: &mut Root, workspace: WorkspaceId, document: DocumentId, at_ms: i64) {
        let mine = self.store.version(workspace, document).unwrap();
        let theirs = from.open_schedule(workspace, document);
        let update = theirs.export_updates_since(&mine).unwrap();
        self.store
            .apply_update(workspace, document, update, at(at_ms), None)
            .unwrap();
        self.store.flush(workspace, document, at(at_ms)).unwrap();
    }
}

fn header(owner: DeviceId) -> ScheduleHeader {
    ScheduleHeader {
        schedule: ScheduleId::mint(),
        created_at_ms: at(1).millis(),
        initial_owner: owner,
        definition: ScheduleDefinition {
            routine: None,
            revision: 1,
            effective_at_ms: at(1).millis(),
            template: DelegationTemplate {
                requested_by: ActorRef::Person {
                    profile: ProfileId::new(),
                },
                objective: Objective::new("Review the release notes").unwrap(),
                agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: WorkspaceScope::Temporary,
                grants: BTreeSet::new(),
                context: Vec::new(),
            },
            recurrence: RecurrenceRule::floating(CronExpression::parse("0 * * * *").unwrap()),
            lifecycle: ScheduleLifecycle::Active,
        },
    }
}

fn skip(occurrence_ms: i64) -> ScheduleClaimPlan {
    ScheduleClaimPlan::Skip {
        occurrence_ms,
        run: RunId::mint(),
        outcome: ScheduleFiringOutcome::SkippedMissed,
        busy_holder: None,
    }
}

struct Replica {
    workspace: WorkspaceId,
    document: DocumentId,
    schedule: ScheduleDocument,
    sent: VersionVector,
}

impl Replica {
    fn create(root: &mut Root, workspace: WorkspaceId, header: &ScheduleHeader) -> Self {
        let (document, _) = root
            .store
            .create_document(workspace, "Schedule", SCHEDULE_KIND, None, None, at(1))
            .unwrap();
        let schedule = ScheduleDocument::create(header).unwrap();
        let mut replica = Self {
            workspace,
            document,
            schedule,
            sent: VersionVector::default(),
        };
        replica.send(root, 2, true);
        replica
    }

    fn joining(root: &mut Root, workspace: WorkspaceId, document: DocumentId) -> Self {
        let schedule = root.open_schedule(workspace, document);
        let sent = schedule.version();
        Self {
            workspace,
            document,
            schedule,
            sent,
        }
    }

    fn send(&mut self, root: &mut Root, at_ms: i64, flush: bool) {
        let update = self.schedule.export_updates_since(&self.sent).unwrap();
        self.sent = self.schedule.version();
        root.store
            .apply_update(self.workspace, self.document, update, at(at_ms), None)
            .unwrap();
        if flush {
            root.store
                .flush(self.workspace, self.document, at(at_ms))
                .unwrap();
        }
    }
}

fn provision_second_root(a: &mut Root, b: &mut Root, workspace: WorkspaceId, document: DocumentId) {
    let manifest = a.store.manifest_id(workspace).unwrap();
    let workspace_record = a.store.workspace(workspace).unwrap();
    let profile = a.store.profile(workspace_record.owner).unwrap();
    b.store
        .provision_replica(&profile, &workspace_record, manifest, at(10))
        .unwrap();
    b.store
        .apply_update(
            workspace,
            manifest,
            a.store.snapshot(workspace, manifest).unwrap(),
            at(11),
            None,
        )
        .unwrap();
    b.store.flush(workspace, manifest, at(11)).unwrap();
    b.store
        .provision_replica_document(workspace, document, at(12))
        .unwrap();
    b.store
        .apply_update(
            workspace,
            document,
            a.store.snapshot(workspace, document).unwrap(),
            at(13),
            None,
        )
        .unwrap();
    b.store.flush(workspace, document, at(13)).unwrap();
}

fn assert_only(
    root: &mut Root,
    workspace: WorkspaceId,
    document: DocumentId,
    owner: DeviceId,
    other: DeviceId,
) {
    let schedule = root.open_schedule(workspace, document);
    assert!(
        schedule
            .claim_authority(workspace, owner, skip(at(100).millis()))
            .unwrap()
            .is_some()
    );
    assert!(
        schedule
            .claim_authority(workspace, other, skip(at(100).millis()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn two_store_roots_preserve_single_owner_across_every_durable_handoff_boundary() {
    let mut a = Root::new();
    let workspace = a.workspace();
    let old = DeviceId::new();
    let target = DeviceId::new();
    let mut writer = Replica::create(&mut a, workspace, &header(old));
    let document = writer.document;
    let mut b = Root::new();
    provision_second_root(&mut a, &mut b, workspace, document);
    assert_only(&mut a, workspace, document, old, target);
    assert_only(&mut b, workspace, document, old, target);

    let handoff = HandoffId::mint();
    writer
        .schedule
        .request_handoff(handoff, target, target, at(20).millis())
        .unwrap();
    writer.send(&mut a, 20, false);
    a.restart();
    assert_only(&mut a, workspace, document, old, target);

    writer = Replica::joining(&mut a, workspace, document);
    assert_eq!(
        writer
            .schedule
            .request_handoff(handoff, target, target, at(21).millis())
            .unwrap(),
        ScheduleWrite::Applied
    );
    writer.send(&mut a, 21, true);
    a.restart();
    b.pull(&mut a, workspace, document, 22);
    b.restart();
    assert_only(&mut a, workspace, document, old, target);
    assert_only(&mut b, workspace, document, old, target);

    writer = Replica::joining(&mut a, workspace, document);
    writer
        .schedule
        .confirm_handoff(handoff, old, at(23).millis())
        .unwrap();
    writer.send(&mut a, 23, false);
    a.restart();
    assert_only(&mut a, workspace, document, old, target);

    writer = Replica::joining(&mut a, workspace, document);
    writer
        .schedule
        .confirm_handoff(handoff, old, at(24).millis())
        .unwrap();
    writer.send(&mut a, 24, true);
    a.restart();
    b.pull(&mut a, workspace, document, 25);
    b.restart();
    assert_only(&mut a, workspace, document, target, old);
    assert_only(&mut b, workspace, document, target, old);
}

#[test]
fn durable_retirement_precedes_trash_and_restore_never_resurrects_authority() {
    let mut root = Root::new();
    let workspace = root.workspace();
    let owner = DeviceId::new();
    let mut writer = Replica::create(&mut root, workspace, &header(owner));
    let document = writer.document;
    writer.schedule.retire(owner, at(30).millis()).unwrap();
    writer.send(&mut root, 30, true);
    root.store
        .trash_document_entry(workspace, document, at(31), Some("test"))
        .unwrap();
    root.restart();
    let trashed = root
        .store
        .documents(workspace)
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == document)
        .unwrap();
    assert!(trashed.trashed_at.is_some());
    assert!(
        root.open_schedule(workspace, document)
            .claim_authority(workspace, owner, skip(at(32).millis()))
            .unwrap()
            .is_none()
    );

    let manifest_id = root.store.manifest_id(workspace).unwrap();
    let mut manifest =
        Manifest::from_snapshot(&root.store.snapshot(workspace, manifest_id).unwrap()).unwrap();
    let before = manifest.version();
    manifest.restore_document(document).unwrap();
    let update = manifest.export_updates_since(&before).unwrap();
    root.store
        .apply_update(workspace, manifest_id, update, at(33), None)
        .unwrap();
    root.store.flush(workspace, manifest_id, at(33)).unwrap();
    root.restart();
    assert!(
        root.open_schedule(workspace, document)
            .claim_authority(workspace, owner, skip(at(34).millis()))
            .unwrap()
            .is_none()
    );
}
