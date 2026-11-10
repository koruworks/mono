// SPDX-License-Identifier: MPL-2.0

//! Root-catalog Plan 4.1 (A1): crash injection at every durable seam of the
//! catalog 12 → 13 step, and (A2) two copies of one legacy catalog upgraded
//! independently. Every database is in this process's shared memory; the
//! faults are real SQLite refusals — a stray table the DDL collides with, a
//! trigger on a table that exists at 12, a row the seed cannot parse, a
//! foreign key the drop cannot satisfy — never a mocked catalog. The one
//! seam no fault can reach from inside the process is the stamp
//! (`PRAGMA user_version`); `VERIFICATION.md` procedure 8 covers it by hand.
//!
//! Each case asserts the same thing: after the fault the file is a 12 with
//! its legacy rows exactly as written and none of the new tables, the scope
//! and the seeds; after the fault is lifted the very next open migrates,
//! and what it produces is what an unfaulted migration produces.

use super::*;
use koru_store_core::ids::{DocumentId, ReminderId, RuleRevisionId};
use koru_store_core::personal_attributes::{EntityRef, PersonalAttributeValue};
use koru_store_core::profile_units::PersonalManifest;
use koru_store_core::reminders::{OneTimeSchedule, ReminderRule, RuleStatus};
use koru_store_core::root_catalog::{
    CatalogUnitKind, CatalogUnitRef, ProfileCatalogUnit, Staged, WorkspaceCatalogUnit,
};
use koru_store_core::service::K_PEER_ID;
use koru_store_core::temporal::ZonedDateTime;

use crate::catalog::root_catalog::inspection;

/// The one peer every legacy personal unit of an installation was written by.
const STORE_PEER: u64 = 4_242;
const NEW_TABLES: [&str; 5] = [
    "root_catalog_units",
    "root_catalog_overlays",
    "workspace_personal_crdts",
    "workspace_reminder_due",
    "root_catalog_migration_receipts",
];
const LEGACY_TABLES: [&str; 2] = ["profile_personal_crdts", "profile_reminder_due"];

fn table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

fn user_version(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn entity(workspace: WorkspaceId) -> EntityRef {
    EntityRef::new(
        "task",
        workspace,
        DocumentId::new(),
        DocumentId::new().as_uuid(),
    )
    .unwrap()
}

fn text(value: &str) -> PersonalAttributeValue {
    PersonalAttributeValue::Text {
        text: value.to_string(),
    }
}

fn zoned_rule(target: &EntityRef, instant_ms: i64) -> ReminderRule {
    ReminderRule {
        id: ReminderId::new(),
        target: target.clone(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Zoned {
            value: ZonedDateTime::new(instant_ms, "UTC").unwrap(),
        },
        tone_override: None,
    }
}

/// The legacy inventory one installation holds at 12, as ids: what every
/// fixture here writes, so two fixtures with one `Legacy` are two copies of
/// one device.
struct Legacy {
    owner: Profile,
    removed: Profile,
    holder: Profile,
    live: Workspace,
    trashed: Workspace,
    deleted_at: i64,
    target: EntityRef,
    /// The reminder both personal sources of `live` hold, under one id.
    shared_rule: ReminderRule,
    by_owner: Vec<u8>,
    by_removed: Vec<u8>,
}

impl Legacy {
    fn new() -> Self {
        let owner = Profile::new("SENTINEL-OWNER-NAME", Locale::default(), at()).unwrap();
        let mut removed = Profile::new("SENTINEL-REMOVED-NAME", Locale::default(), at()).unwrap();
        removed.created_at = Timestamp::from_millis(at().millis() + 1);
        let mut holder = Profile::holder(Locale::default(), at()).unwrap();
        holder.created_at = Timestamp::from_millis(at().millis() + 2);
        let live = Workspace::new(owner.id, "SENTINEL-LIVE-NAME", at()).unwrap();
        let mut trashed = Workspace::new(owner.id, "SENTINEL-TRASHED-NAME", at()).unwrap();
        trashed.created_at = Timestamp::from_millis(at().millis() + 1);
        let target = entity(live.id);
        let shared_rule = zoned_rule(&target, 1_800_000_000_000);
        // Two personal units for `live`, both written by the one Store peer
        // from counter zero: the §5.2 hazard. Both hold the same reminder
        // rule, so the union must hold it once.
        let by_owner = PersonalManifest::create(owner.id, live.id).unwrap();
        by_owner.set_peer_id(STORE_PEER).unwrap();
        by_owner
            .set_personal_attribute(&target, "from", &text("owner"))
            .unwrap();
        by_owner.put_reminder_rule(&shared_rule).unwrap();
        let by_removed = PersonalManifest::create(removed.id, live.id).unwrap();
        by_removed.set_peer_id(STORE_PEER).unwrap();
        by_removed
            .set_personal_attribute(&target, "also", &text("removed"))
            .unwrap();
        by_removed.put_reminder_rule(&shared_rule).unwrap();
        Self {
            owner,
            removed,
            holder,
            live,
            trashed,
            deleted_at: at().millis() + 9,
            target,
            shared_rule,
            by_owner: by_owner.export_snapshot().unwrap(),
            by_removed: by_removed.export_snapshot().unwrap(),
        }
    }

    /// Write this inventory into a connection stamped 12.
    fn write(&self, connection: &Connection) {
        for profile in [&self.owner, &self.removed, &self.holder] {
            legacy_profile_row(connection, profile);
        }
        connection
            .execute(
                "UPDATE profiles SET removed_at = ?2 WHERE id = ?1",
                params![self.removed.id.to_storage(), at().millis() + 5],
            )
            .unwrap();
        for (workspace, deleted_at) in [(&self.live, None), (&self.trashed, Some(self.deleted_at))]
        {
            connection
                .execute(
                    "INSERT INTO workspaces (id, owner, name, created_at, position, deleted_at)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                    params![
                        workspace.id.to_storage(),
                        workspace.owner.to_storage(),
                        workspace.name,
                        workspace.created_at.millis(),
                        deleted_at,
                    ],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO store_state (key, value) VALUES (?1, ?2)",
                params![K_PEER_ID, STORE_PEER.to_string()],
            )
            .unwrap();
        for (owner, snapshot) in [
            (self.owner.id, &self.by_owner),
            (self.removed.id, &self.by_removed),
        ] {
            connection
                .execute(
                    "INSERT INTO profile_personal_crdts (owner_profile_id, workspace_id, snapshot)
                     VALUES (?1, ?2, ?3)",
                    params![owner.to_storage(), self.live.id.to_storage(), snapshot],
                )
                .unwrap();
        }
    }
}

/// A shared in-memory catalog stamped 12 holding `legacy`, with a keeper
/// connection that outlives every open and is the fault's hand.
struct Twelve {
    name: String,
    keeper: Connection,
}

impl Twelve {
    fn new(label: &str, legacy: &Legacy) -> Self {
        let name = format!("root-catalog-fault-{label}-{}", ProfileId::new());
        let keeper = db::open_shared_in_memory(&name).unwrap();
        let mut connection = db::open_shared_in_memory(&name).unwrap();
        db::migrate(&mut connection, "catalog.db", &MIGRATIONS[..12], 12).unwrap();
        legacy.write(&connection);
        Self { name, keeper }
    }

    fn open(&self) -> Result<SqliteCatalog> {
        SqliteCatalog::from_connection(db::open_shared_in_memory(&self.name).unwrap())
    }

    fn fail_on(&self, sql: &str) {
        self.keeper
            .execute_batch(&format!(
                "CREATE TRIGGER fail {sql} BEGIN SELECT RAISE(ABORT, 'injected fault'); END;"
            ))
            .unwrap();
    }

    fn recover(&self) {
        self.keeper.execute_batch("DROP TRIGGER fail;").unwrap();
    }

    /// The pre-state is intact: a 12, the legacy rows as written, no new
    /// table, no scope, no seed, no receipt.
    fn assert_still_twelve(&self, legacy: &Legacy, what: &str) {
        assert_eq!(user_version(&self.keeper), 12, "{what}: the stamp");
        for table in NEW_TABLES {
            assert!(
                !table_exists(&self.keeper, table),
                "{what}: {table} survived the rollback"
            );
        }
        for table in LEGACY_TABLES {
            assert!(table_exists(&self.keeper, table), "{what}: {table} is gone");
        }
        assert_eq!(
            count(&self.keeper, "SELECT COUNT(*) FROM profile_personal_crdts"),
            2,
            "{what}: the legacy personal rows"
        );
        assert_eq!(
            count(&self.keeper, "SELECT COUNT(*) FROM profiles"),
            3,
            "{what}"
        );
        assert_eq!(
            count(&self.keeper, "SELECT COUNT(*) FROM workspaces"),
            2,
            "{what}"
        );
        assert_eq!(
            count(
                &self.keeper,
                "SELECT COUNT(*) FROM store_state WHERE key = 'local_user_scope'"
            ),
            0,
            "{what}: no scope was minted"
        );
        let stored: Option<String> = self
            .keeper
            .query_row(
                "SELECT value FROM store_state WHERE key = ?1",
                params![K_PEER_ID],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            stored.as_deref(),
            Some(STORE_PEER.to_string().as_str()),
            "{what}: the Store peer is what it was"
        );
        let legacy_snapshot: Vec<u8> = self
            .keeper
            .query_row(
                "SELECT snapshot FROM profile_personal_crdts WHERE owner_profile_id = ?1",
                params![legacy.owner.id.to_storage()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_snapshot, legacy.by_owner, "{what}: bytes untouched");
    }

    /// The post-state is complete: what an unfaulted upgrade of `legacy`
    /// produces, down to the receipt counts.
    fn assert_migrated(&self, c: &SqliteCatalog, legacy: &Legacy, what: &str) {
        assert_eq!(c.schema_version().unwrap(), 13, "{what}");
        assert_eq!(user_version(&self.keeper), 13, "{what}");
        for table in LEGACY_TABLES {
            assert!(!table_exists(&self.keeper, table), "{what}: {table}");
        }
        let scope = c.local_user_scope().unwrap();
        assert_eq!(c.state(K_PEER_ID).unwrap(), Some(STORE_PEER.to_string()));
        assert_eq!(
            c.root_catalog_units(scope, CatalogUnitKind::Profile)
                .unwrap()
                .len(),
            2,
            "{what}: one unit per non-Holder Profile"
        );
        assert!(
            c.root_catalog_unit(
                scope,
                CatalogUnitRef::Profile {
                    profile: legacy.holder.id
                }
            )
            .unwrap()
            .is_none(),
            "{what}: the Holder is excluded"
        );
        assert_eq!(
            c.root_catalog_units(scope, CatalogUnitKind::Workspace)
                .unwrap()
                .len(),
            2,
            "{what}"
        );
        assert_eq!(
            c.workspace_personal_units(scope).unwrap().len(),
            1,
            "{what}"
        );
        assert_eq!(
            count(
                &self.keeper,
                "SELECT COUNT(*) FROM root_catalog_migration_receipts WHERE kind = 'root_seed'"
            ),
            5,
            "{what}: three Profiles (one excluded) and two Workspaces"
        );
        assert_eq!(
            count(
                &self.keeper,
                "SELECT COUNT(*) FROM root_catalog_migration_receipts WHERE kind = 'personal_v2'"
            ),
            1,
            "{what}"
        );
        // The shared reminder is one row in the due index, never two.
        let due = c.workspace_reminder_due(scope).unwrap();
        assert_eq!(due.len(), 1, "{what}: one due row per reminder");
        assert_eq!(due[0].reminder, legacy.shared_rule.id);
        assert_eq!(
            count(
                &self.keeper,
                "SELECT COUNT(*) FROM store_state WHERE key = 'local_user_scope'"
            ),
            1
        );
    }
}

// ------------------------------------------------------------ the DDL

/// Seam: the DDL. A table the DDL will create already exists in another
/// shape (a stray from a hand repair, say). `CREATE TABLE` refuses, the
/// tables created before it in the same transaction roll back with it, the
/// file is a 12, and the retry fails identically until the stray is gone.
#[test]
fn a_fault_in_the_ddl_rolls_back_the_tables_created_before_it_and_the_retry_migrates() {
    let legacy = Legacy::new();
    let twelve = Twelve::new("ddl", &legacy);
    // The third table of the DDL, so the first two were created and must go.
    twelve
        .keeper
        .execute_batch("CREATE TABLE workspace_personal_crdts (stray TEXT);")
        .unwrap();

    let error = twelve.open().err().expect("the DDL collides");
    assert_eq!(error.code(), Code::StorageFailed);
    assert_eq!(user_version(&twelve.keeper), 12);
    assert!(
        !table_exists(&twelve.keeper, "root_catalog_units"),
        "created before the collision and rolled back with it"
    );
    assert!(!table_exists(&twelve.keeper, "root_catalog_overlays"));
    assert!(table_exists(&twelve.keeper, "workspace_personal_crdts"));
    for table in LEGACY_TABLES {
        assert!(table_exists(&twelve.keeper, table));
    }
    assert_eq!(
        count(
            &twelve.keeper,
            "SELECT COUNT(*) FROM store_state WHERE key = 'local_user_scope'"
        ),
        0
    );
    // The same open again is the same refusal, not a half-step forward.
    assert_eq!(twelve.open().err().unwrap().code(), Code::StorageFailed);
    assert_eq!(user_version(&twelve.keeper), 12);

    twelve
        .keeper
        .execute_batch("DROP TABLE workspace_personal_crdts;")
        .unwrap();
    twelve.assert_still_twelve(&legacy, "after the stray is removed");
    let c = twelve.open().unwrap();
    twelve.assert_migrated(&c, &legacy, "after the stray is removed");
}

// ------------------------------------------------- scope and peer mint

/// Seam: the scope mint and the peer mint, the two `store_state` writes
/// between the DDL and the seeds. A trigger on `store_state` is the one
/// trigger that can be installed at 12, because the table exists there.
#[test]
fn a_fault_minting_the_scope_or_the_peer_leaves_a_twelve_with_neither() {
    let legacy = Legacy::new();
    let twelve = Twelve::new("scope", &legacy);
    twelve.fail_on("BEFORE INSERT ON store_state WHEN NEW.key = 'local_user_scope'");
    let error = twelve.open().err().expect("the scope mint faults");
    assert_eq!(error.code(), Code::StorageFailed);
    twelve.assert_still_twelve(&legacy, "scope mint fault");
    twelve.recover();
    let c = twelve.open().unwrap();
    twelve.assert_migrated(&c, &legacy, "after the scope mint fault");

    // An installation the Store has never opened has no peer id yet; the
    // migration mints one. A fault there is a fault before any seed.
    let twelve = Twelve::new("peer", &legacy);
    twelve
        .keeper
        .execute("DELETE FROM store_state WHERE key = ?1", params![K_PEER_ID])
        .unwrap();
    twelve.fail_on("BEFORE INSERT ON store_state WHEN NEW.key = 'peer_id'");
    assert_eq!(
        twelve.open().err().expect("the peer mint faults").code(),
        Code::StorageFailed
    );
    assert_eq!(user_version(&twelve.keeper), 12);
    for table in NEW_TABLES {
        assert!(!table_exists(&twelve.keeper, table), "{table}");
    }
    assert_eq!(
        count(
            &twelve.keeper,
            "SELECT COUNT(*) FROM store_state WHERE key IN ('peer_id', 'local_user_scope')"
        ),
        0
    );
    twelve.recover();
    let c = twelve.open().unwrap();
    assert_eq!(c.schema_version().unwrap(), 13);
    let minted: u64 = c.state(K_PEER_ID).unwrap().unwrap().parse().unwrap();
    assert_ne!(minted, 0);
    let scope = c.local_user_scope().unwrap();
    // Every seed is under the peer the migration minted, not the one the
    // legacy personal units were written by.
    for (unit, row) in c
        .root_catalog_units(scope, CatalogUnitKind::Workspace)
        .unwrap()
    {
        let unit = WorkspaceCatalogUnit::from_snapshot(&row.snapshot, unit.workspace().unwrap(), 1)
            .unwrap();
        assert!(unit.version().get(&minted).is_some());
        assert!(unit.version().get(&STORE_PEER).is_none());
    }
}

// --------------------------------------------------------------- seeds

/// Seam: each seed. A `profiles` row whose id does not parse is met after
/// the valid Profiles were seeded (ORDER BY created_at), and a `workspaces`
/// row the same way after the Profiles and the first Workspace: the seeds
/// written before the fault roll back with it, and the person sees a 12.
#[test]
fn a_fault_in_a_profile_seed_or_a_workspace_seed_leaves_no_partial_seed() {
    let legacy = Legacy::new();
    for (label, sql) in [
        (
            "profile",
            "INSERT INTO profiles (id, name, locale, created_at, kind)
             VALUES ('not-a-profile-id', 'Stray', 'en', 9999999999999, 'normal')",
        ),
        (
            "workspace",
            "INSERT INTO workspaces (id, owner, name, created_at, position)
             VALUES ('not-a-workspace-id', 'not-an-owner-id', 'Stray', 9999999999999, 0)",
        ),
    ] {
        let twelve = Twelve::new(label, &legacy);
        // Foreign keys off for the hand-written stray: the fault under test
        // is the seed's parse, not the fixture's referential integrity.
        twelve
            .keeper
            .execute_batch(&format!(
                "PRAGMA foreign_keys = OFF; {sql}; PRAGMA foreign_keys = ON;"
            ))
            .unwrap();
        let error = twelve.open().err().expect("the seed refuses the stray row");
        assert_eq!(error.code(), Code::CorruptState, "{label}");
        assert_eq!(user_version(&twelve.keeper), 12, "{label}");
        for table in NEW_TABLES {
            assert!(
                !table_exists(&twelve.keeper, table),
                "{label}: {table} — the seeds before the fault rolled back"
            );
        }
        for table in LEGACY_TABLES {
            assert!(table_exists(&twelve.keeper, table), "{label}: {table}");
        }
        // The retry is the same refusal until the row is repaired; then the
        // next open migrates what the unfaulted one would.
        assert_eq!(twelve.open().err().unwrap().code(), Code::CorruptState);
        let table = if label == "profile" {
            "profiles"
        } else {
            "workspaces"
        };
        twelve
            .keeper
            .execute_batch(&format!(
                "PRAGMA foreign_keys = OFF; DELETE FROM {table} WHERE id LIKE 'not-a-%'; PRAGMA foreign_keys = ON;"
            ))
            .unwrap();
        twelve.assert_still_twelve(&legacy, label);
        let c = twelve.open().unwrap();
        twelve.assert_migrated(&c, &legacy, label);
    }
}

// ------------------------------------------------------ personal union

/// Seam: the personal union's row write. A legacy personal source whose
/// Workspace row is absent (written with foreign keys off, as a repair
/// might) passes normalization and fails the v2 row's `REFERENCES
/// workspaces(id)`: every seed before it rolls back, the legacy source is
/// kept, and the next open after the repair migrates it.
#[test]
fn a_fault_writing_a_personal_v2_row_keeps_the_legacy_source_and_every_seed_rolls_back() {
    let legacy = Legacy::new();
    let twelve = Twelve::new("personal", &legacy);
    let orphan_workspace = WorkspaceId::new();
    let orphan = PersonalManifest::create(legacy.owner.id, orphan_workspace).unwrap();
    orphan.set_peer_id(STORE_PEER).unwrap();
    orphan
        .set_personal_attribute(&entity(orphan_workspace), "note", &text("kept"))
        .unwrap();
    twelve
        .keeper
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    twelve
        .keeper
        .execute(
            "INSERT INTO profile_personal_crdts (owner_profile_id, workspace_id, snapshot)
             VALUES (?1, ?2, ?3)",
            params![
                legacy.owner.id.to_storage(),
                orphan_workspace.to_storage(),
                orphan.export_snapshot().unwrap()
            ],
        )
        .unwrap();
    twelve
        .keeper
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();

    let error = twelve
        .open()
        .err()
        .expect("the orphan's v2 row has no Workspace");
    assert_eq!(error.code(), Code::StorageFailed);
    assert_eq!(user_version(&twelve.keeper), 12);
    for table in NEW_TABLES {
        assert!(!table_exists(&twelve.keeper, table), "{table}");
    }
    assert_eq!(
        count(
            &twelve.keeper,
            "SELECT COUNT(*) FROM profile_personal_crdts"
        ),
        3,
        "the legacy sources, the orphan included, are kept"
    );
    // The repair a person would make: give the orphan its Workspace row.
    twelve
        .keeper
        .execute(
            "INSERT INTO workspaces (id, owner, name, created_at, position)
             VALUES (?1, ?2, 'Recovered', ?3, 0)",
            params![
                orphan_workspace.to_storage(),
                legacy.owner.id.to_storage(),
                at().millis()
            ],
        )
        .unwrap();
    let c = twelve.open().unwrap();
    assert_eq!(c.schema_version().unwrap(), 13);
    let scope = c.local_user_scope().unwrap();
    assert_eq!(c.workspace_personal_units(scope).unwrap().len(), 2);
    let recovered = c
        .workspace_personal_unit(scope, orphan_workspace)
        .unwrap()
        .unwrap();
    let doc = PersonalManifest::from_snapshot_v2(&recovered.snapshot, orphan_workspace).unwrap();
    assert_eq!(
        doc.personal_attributes(&entity(orphan_workspace)).len(),
        0,
        "a fresh entity id reads nothing; the attribute is under the fixture's entity"
    );
    assert!(!doc.is_read_only());
}

// --------------------------------------------------------------- drops

/// Seam: the drops. With foreign keys on, `DROP TABLE` first deletes every
/// row, and a child row referencing the legacy personal table makes that
/// delete fail — the seeds, the v2 rows and the receipts written before it
/// roll back, and the legacy tables stand. Removing the child lets the next
/// open finish.
#[test]
fn a_fault_at_the_drop_leaves_the_legacy_tables_and_rolls_the_new_rows_back() {
    let legacy = Legacy::new();
    let twelve = Twelve::new("drop", &legacy);
    twelve
        .keeper
        .execute_batch(
            "CREATE TABLE fault_child (
               owner TEXT NOT NULL, workspace TEXT NOT NULL,
               FOREIGN KEY (owner, workspace)
                 REFERENCES profile_personal_crdts(owner_profile_id, workspace_id)
             );",
        )
        .unwrap();
    twelve
        .keeper
        .execute(
            "INSERT INTO fault_child (owner, workspace) VALUES (?1, ?2)",
            params![legacy.owner.id.to_storage(), legacy.live.id.to_storage()],
        )
        .unwrap();

    let error = twelve
        .open()
        .err()
        .expect("the drop violates the child's key");
    assert_eq!(error.code(), Code::StorageFailed);
    twelve.assert_still_twelve(&legacy, "drop fault");
    assert_eq!(
        count(&twelve.keeper, "SELECT COUNT(*) FROM fault_child"),
        1,
        "nothing of the person's rows went with the rollback"
    );

    twelve
        .keeper
        .execute_batch("DROP TABLE fault_child;")
        .unwrap();
    let c = twelve.open().unwrap();
    twelve.assert_migrated(&c, &legacy, "after the child is removed");
    // The merged unit holds both sources' values and the one shared rule.
    let scope = c.local_user_scope().unwrap();
    let state = c
        .workspace_personal_unit(scope, legacy.live.id)
        .unwrap()
        .unwrap();
    let merged = PersonalManifest::from_snapshot_v2(&state.snapshot, legacy.live.id).unwrap();
    let attributes = merged.personal_attributes(&legacy.target);
    assert_eq!(attributes["from"], text("owner"));
    assert_eq!(attributes["also"], text("removed"));
    assert_eq!(merged.reminders().len(), 1, "one rule, not two");
    assert_eq!(merged.reminders()[0].rule.id, legacy.shared_rule.id);
}

// ------------------------------------- independently upgraded copies (A2)

/// Two devices holding byte-identical copies of one legacy catalog (the
/// same Profile and Workspace UUIDs, the same personal sources written by
/// the same Store peer) upgrade without seeing each other. The normalized
/// personal histories carry the same operation ids, so importing one into
/// the other adds nothing but the other device's own v2 stamp; the root
/// seeds are identical operations and importing one into the other changes
/// nothing at all. Reminder and action ids never change.
#[test]
fn two_copies_of_one_twelve_upgrade_to_the_same_operation_ids_and_joining_them_adds_nothing() {
    let legacy = Legacy::new();
    let a = Twelve::new("copy-a", &legacy);
    let b = Twelve::new("copy-b", &legacy);
    let ca = a.open().unwrap();
    let cb = b.open().unwrap();
    let (sa, sb) = (
        ca.local_user_scope().unwrap(),
        cb.local_user_scope().unwrap(),
    );
    assert_ne!(sa, sb, "each device mints its own scope");

    // Root units: the seeds are deterministic under one Store peer.
    for kind in [CatalogUnitKind::Profile, CatalogUnitKind::Workspace] {
        let ua = ca.root_catalog_units(sa, kind).unwrap();
        let ub = cb.root_catalog_units(sb, kind).unwrap();
        assert_eq!(ua.len(), ub.len());
        for ((unit_a, row_a), (unit_b, row_b)) in ua.iter().zip(&ub) {
            assert_eq!(unit_a, unit_b);
            assert_eq!(row_a.revision, 1);
            match unit_a {
                CatalogUnitRef::Workspace { workspace } => {
                    let held = WorkspaceCatalogUnit::from_snapshot(&row_a.snapshot, *workspace, 1)
                        .unwrap();
                    let other = WorkspaceCatalogUnit::from_snapshot(&row_b.snapshot, *workspace, 1)
                        .unwrap();
                    assert_eq!(held.version(), other.version(), "same operation ids");
                    assert_eq!(held.placement(), other.placement());
                    assert_eq!(held.deletions(), other.deletions());
                    // Joining B's copy into A's: nothing new arrives.
                    let staged = held
                        .staged_import(std::slice::from_ref(&row_b.snapshot))
                        .unwrap();
                    let Staged::Applied(next) = staged else {
                        panic!("a complete snapshot is never pending");
                    };
                    assert_eq!(next.version(), held.version(), "deduplicated");
                }
                CatalogUnitRef::Profile { profile } => {
                    let held =
                        ProfileCatalogUnit::from_snapshot(&row_a.snapshot, *profile, 1).unwrap();
                    let other =
                        ProfileCatalogUnit::from_snapshot(&row_b.snapshot, *profile, 1).unwrap();
                    assert_eq!(held.version(), other.version());
                    let Staged::Applied(next) = held
                        .staged_import(std::slice::from_ref(&row_b.snapshot))
                        .unwrap()
                    else {
                        panic!("never pending");
                    };
                    assert_eq!(next.version(), held.version());
                }
            }
        }
    }

    // Personal v2: the legacy operations under the same derived peers; the
    // only difference is each device's own v2 writer and its one stamp.
    let pa = ca
        .workspace_personal_unit(sa, legacy.live.id)
        .unwrap()
        .unwrap();
    let pb = cb
        .workspace_personal_unit(sb, legacy.live.id)
        .unwrap()
        .unwrap();
    assert_ne!(
        pa.writer_peer_id, pb.writer_peer_id,
        "a fresh v2 writer each"
    );
    let da = PersonalManifest::from_snapshot_v2(&pa.snapshot, legacy.live.id).unwrap();
    let db_ = PersonalManifest::from_snapshot_v2(&pb.snapshot, legacy.live.id).unwrap();
    let legacy_peers = |doc: &PersonalManifest, writer: u64| {
        let mut peers: std::collections::BTreeSet<u64> = doc.version().keys().copied().collect();
        peers.remove(&writer);
        peers
    };
    assert_eq!(
        legacy_peers(&da, pa.writer_peer_id),
        legacy_peers(&db_, pb.writer_peer_id),
        "the derived legacy namespaces are identical on both devices"
    );
    assert!(legacy_peers(&da, pa.writer_peer_id).len() >= 2);
    assert!(!legacy_peers(&da, pa.writer_peer_id).contains(&STORE_PEER));
    let before = da.version();
    let Staged::Applied(joined) = da.stage_v2(std::slice::from_ref(&pb.snapshot)).unwrap() else {
        panic!("a complete snapshot is never pending");
    };
    let after = joined.version();
    assert!(after.includes_vv(&before));
    assert_eq!(
        after.iter().count(),
        before.iter().count() + 1,
        "exactly B's own writer arrived; every legacy operation deduplicated"
    );
    assert!(after.get(&pb.writer_peer_id).is_some());
    // Content and ids agree and are unchanged.
    assert_eq!(
        joined.personal_attributes(&legacy.target),
        da.personal_attributes(&legacy.target)
    );
    let ids = |doc: &PersonalManifest| {
        doc.reminders()
            .into_iter()
            .map(|row| row.rule.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&joined), vec![legacy.shared_rule.id]);
    assert_eq!(ids(&db_), vec![legacy.shared_rule.id]);
    // The migration receipts name the same derived peers on both devices.
    let receipt = |keeper: &Connection| -> String {
        keeper
            .query_row(
                "SELECT receipt FROM root_catalog_migration_receipts
                 WHERE kind = 'personal_v2' AND subject_id = ?1",
                params![legacy.live.id.to_storage()],
                |row| row.get(0),
            )
            .unwrap()
    };
    let (ra, rb) = (receipt(&a.keeper), receipt(&b.keeper));
    let derived_peers = |receipt: &str| -> Vec<String> {
        let value: serde_json::Value = serde_json::from_str(receipt).unwrap();
        let mut found = Vec::new();
        fn walk(value: &serde_json::Value, found: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, item) in map {
                        if key == "derived" {
                            found.push(item.to_string());
                        }
                        walk(item, found);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|i| walk(i, found)),
                _ => {}
            }
        }
        walk(&value, &mut found);
        found.sort();
        found
    };
    assert_eq!(derived_peers(&ra), derived_peers(&rb));
    assert!(!derived_peers(&ra).is_empty());
}

// ------------------------------------------------------ the diagnostic (A4)

/// The read-only inventory a procedure inspects: ids and counts, never a
/// name — the sentinel names of every Profile and Workspace in the fixture
/// appear nowhere in its JSON, and its numbers are the migration's.
#[test]
fn the_inspection_report_carries_ids_and_counts_and_never_a_name() {
    let legacy = Legacy::new();
    let twelve = Twelve::new("inspect", &legacy);
    assert_eq!(
        inspection::inspect(&twelve.keeper).unwrap_err().code(),
        Code::VersionUnavailable,
        "a 12 is reported as not yet migrated, never read as if it were"
    );
    let c = twelve.open().unwrap();
    let report = inspection::inspect(&twelve.keeper).unwrap();
    assert_eq!(report.schema_version, 13);
    assert_eq!(report.scope, c.local_user_scope().unwrap().to_storage());
    assert_eq!(report.profile_units.len(), 2);
    assert_eq!(report.workspace_units.len(), 2);
    assert!(report.overlays.is_empty());
    assert_eq!(report.personal_units.len(), 1);
    assert_eq!(report.due_rows, 1);
    assert_eq!(report.receipts.root_seed, 5);
    assert_eq!(report.receipts.personal_v2, 1);
    assert_eq!(report.join_receipts, 0);
    assert_eq!(report.batch_jobs, 0);
    let trashed = report
        .workspace_units
        .iter()
        .find(|unit| unit.subject == legacy.trashed.id.to_storage())
        .expect("the trashed Workspace is listed");
    assert_eq!(trashed.active_deletions, 1);
    assert_eq!(
        trashed.placement.as_deref(),
        Some(legacy.owner.id.to_storage().as_str())
    );
    assert!(!trashed.live);
    let json = report.to_json().unwrap();
    for name in [
        "SENTINEL-OWNER-NAME",
        "SENTINEL-REMOVED-NAME",
        "SENTINEL-LIVE-NAME",
        "SENTINEL-TRASHED-NAME",
    ] {
        assert!(!json.contains(name), "the report names a subject: {name}");
    }
    assert!(json.contains(&legacy.live.id.to_storage()));
}
