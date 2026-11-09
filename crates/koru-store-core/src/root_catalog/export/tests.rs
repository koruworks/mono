// SPDX-License-Identifier: MPL-2.0

//! Memory-only coverage of `docs/features/root-catalog/TECHNICAL.final.md`
//! §4.3: hidden historical placements and legacy owners leave no trace in
//! what a recipient receives, nothing current is invented, redacted and
//! full replicas keep converging, and every shape the walker does not
//! understand is refused. In-memory Loro documents only; no port, no disk.

use std::collections::{BTreeMap, BTreeSet};

use loro::{ExportMode, LoroDoc, LoroList, LoroMap, LoroText, LoroValue, VersionVector};
use uuid::Uuid;

use super::*;
use crate::errors::Code;
use crate::ids::{DeletionId, DocumentId, RestoreId};
use crate::personal_attributes::{EntityRef, PersonalAttributeValue};
use crate::profile_units::{LegacyPersonalSource, PersonalManifest, migrate_personal_sources};
use crate::root_catalog::{ProfileCatalogUnit, Staged, WorkspaceCatalogUnit};

const FULL_WRITER: u64 = 10;
const RECIPIENT_WRITER: u64 = 20;

fn capture() -> PolicyCapture {
    PolicyCapture {
        policy_generation: 7,
        authorization: "grant-1".into(),
    }
}

/// A visibility answer hiding exactly `hidden`.
fn hiding(hidden: Vec<ProfileId>) -> impl Fn(ProfileId) -> bool {
    move |profile| !hidden.contains(&profile)
}

/// A Workspace moved A → B → C, with one deletion, the restore that
/// observed it and the purge evidence of that deletion, so every lifecycle
/// map holds a fact. `placements` are the three registers in order.
struct Moved {
    unit: WorkspaceCatalogUnit,
    workspace: WorkspaceId,
    profiles: [ProfileId; 3],
    placements: [Placement; 3],
    deletion: DeletionId,
    restore: RestoreId,
}

fn moved() -> Moved {
    let workspace = WorkspaceId::new();
    let profiles = [ProfileId::new(), ProfileId::new(), ProfileId::new()];
    let unit = WorkspaceCatalogUnit::create(workspace, FULL_WRITER).unwrap();
    let placements = profiles.map(|profile| Placement {
        profile: Some(profile),
        move_id: Uuid::new_v4(),
    });
    let deletion = DeletionId::new();
    let restore = RestoreId::new();
    unit.set_placement(placements[0].profile, placements[0].move_id)
        .unwrap();
    unit.record_deletion(deletion, &DeletionRecord::new(100, Some(Uuid::new_v4())))
        .unwrap();
    unit.set_placement(placements[1].profile, placements[1].move_id)
        .unwrap();
    unit.record_restore(
        restore,
        &RestoreRecord {
            at_ms: 200,
            observed_deletions: vec![deletion],
            batch_id: None,
        },
    )
    .unwrap();
    unit.set_placement(placements[2].profile, placements[2].move_id)
        .unwrap();
    unit.record_purge(deletion, &PurgeRecord { at_ms: 300 })
        .unwrap();
    Moved {
        unit,
        workspace,
        profiles,
        placements,
        deletion,
        restore,
    }
}

fn history(doc: &LoroDoc) -> serde_json::Value {
    serde_json::to_value(
        doc.export_json_updates_without_peer_compression(
            &VersionVector::default(),
            &doc.oplog_vv(),
        ),
    )
    .unwrap()
}

/// Whether `needle` occurs anywhere in a JSON tree: keys and values alike.
fn mentions(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains(needle),
        serde_json::Value::Array(items) => items.iter().any(|item| mentions(item, needle)),
        serde_json::Value::Object(fields) => fields
            .iter()
            .any(|(key, item)| key.contains(needle) || mentions(item, needle)),
        _ => false,
    }
}

/// Import bytes into a fresh document, insisting nothing is left pending.
fn fresh_recipient(bytes: &[u8]) -> LoroDoc {
    let doc = LoroDoc::new();
    let status = doc.import(bytes).unwrap();
    assert!(
        status.pending.is_none(),
        "derived bytes must apply completely"
    );
    doc
}

fn publish(
    subject: ExportSubject,
    snapshot: &[u8],
    hidden: &[ProfileId],
    recipient_vv: Option<&VersionVector>,
) -> Publication {
    let visible = hiding(hidden.to_vec());
    let scope = ExportScope::new(&visible, capture());
    derive(subject, snapshot, &scope, recipient_vv)
        .unwrap()
        .publication()
        .expect("published, not held")
}

/// Every lifecycle fact and reducer answer of a unit, for equality.
#[derive(Debug, PartialEq, Eq)]
struct Lifecycle {
    deletions: BTreeMap<DeletionId, DeletionRecord>,
    restores: BTreeMap<RestoreId, RestoreRecord>,
    purged: BTreeMap<DeletionId, PurgeRecord>,
    active: BTreeSet<DeletionId>,
}

fn lifecycle_of(unit: &WorkspaceCatalogUnit) -> Lifecycle {
    Lifecycle {
        deletions: unit.deletions(),
        restores: unit.restores(),
        purged: unit.purged(),
        active: unit.active_deletions(),
    }
}

// ---------------------------------------------------------- Workspace unit

#[test]
fn hidden_historical_placements_are_redacted_and_nothing_else_changes() {
    let fixture = moved();
    let [a, b, c] = fixture.profiles;
    let snapshot = fixture.unit.snapshot().unwrap();
    let source_history = history(&fixture.unit.inner.doc);
    assert!(mentions(&source_history, &a.to_storage()));
    assert!(mentions(&source_history, &b.to_storage()));

    let publication = publish(
        ExportSubject::WorkspaceCatalog(fixture.workspace),
        &snapshot,
        &[a, b],
        None,
    );
    assert_eq!(
        publication.redacted_ops, 2,
        "exactly the two hidden placements"
    );
    assert_eq!(publication.kind, PayloadKind::Snapshot);
    assert_eq!(publication.from, VersionVector::default());
    assert_eq!(publication.to, fixture.unit.version());
    assert_eq!(publication.capture, capture(), "echoed back unchanged");

    // The derived snapshot is a valid Workspace unit with the source's
    // current placement and lifecycle facts.
    let recipient = WorkspaceCatalogUnit::from_snapshot(
        &publication.bytes,
        fixture.workspace,
        RECIPIENT_WRITER,
    )
    .unwrap();
    assert_eq!(recipient.placement(), Some(fixture.placements[2]));
    assert_eq!(recipient.placement().unwrap().profile, Some(c));
    assert_eq!(lifecycle_of(&recipient), lifecycle_of(&fixture.unit));
    assert_eq!(recipient.deletions().len(), 1);
    assert_eq!(
        recipient.restores()[&fixture.restore].observed_deletions,
        vec![fixture.deletion]
    );
    assert_eq!(recipient.purged()[&fixture.deletion].at_ms, 300);
    assert_eq!(recipient.version(), fixture.unit.version());

    // A fresh recipient applies it with nothing pending, and its whole
    // history — keys and values — never spells A or B.
    let doc = fresh_recipient(&publication.bytes);
    let recipient_history = history(&doc);
    assert!(!mentions(&recipient_history, &a.to_storage()));
    assert!(!mentions(&recipient_history, &b.to_storage()));
    assert!(mentions(&recipient_history, &c.to_storage()));
    assert!(mentions(
        &recipient_history,
        &fixture.placements[2].move_id.to_string()
    ));
    for hidden in [&fixture.placements[0], &fixture.placements[1]] {
        assert!(
            !mentions(&recipient_history, &hidden.move_id.to_string()),
            "the whole atomic placement record is nulled, move id included"
        );
    }
}

#[test]
fn a_hidden_current_placement_holds_the_publication_with_no_bytes() {
    let fixture = moved();
    let [_, _, c] = fixture.profiles;
    let visible = hiding(vec![c]);
    let scope = ExportScope::new(&visible, capture());
    let outcome = derive(
        ExportSubject::WorkspaceCatalog(fixture.workspace),
        &fixture.unit.snapshot().unwrap(),
        &scope,
        None,
    )
    .unwrap();
    assert_eq!(
        outcome,
        Derived::Hold {
            reason: HoldReason::CurrentPlacementHidden
        }
    );
    assert!(outcome.publication().is_none());
    assert_eq!(
        Error::from(HoldReason::CurrentPlacementHidden).code(),
        Code::Forbidden
    );
}

#[test]
fn an_explicitly_unassigned_placement_names_nobody_and_passes() {
    let workspace = WorkspaceId::new();
    let hidden = ProfileId::new();
    let unit = WorkspaceCatalogUnit::create(workspace, FULL_WRITER).unwrap();
    unit.set_placement(Some(hidden), Uuid::new_v4()).unwrap();
    unit.set_placement(None, Uuid::new_v4()).unwrap();
    let publication = publish(
        ExportSubject::WorkspaceCatalog(workspace),
        &unit.snapshot().unwrap(),
        &[hidden],
        None,
    );
    assert_eq!(publication.redacted_ops, 1);
    let recipient =
        WorkspaceCatalogUnit::from_snapshot(&publication.bytes, workspace, RECIPIENT_WRITER)
            .unwrap();
    assert_eq!(recipient.placement().unwrap().profile, None);
    assert!(!mentions(
        &history(&fresh_recipient(&publication.bytes)),
        &hidden.to_storage()
    ));
}

#[test]
fn a_profile_catalog_unit_passes_through_unchanged() {
    let profile = ProfileId::new();
    let unit = ProfileCatalogUnit::create(profile, 3).unwrap();
    let deletion = DeletionId::new();
    unit.record_deletion(deletion, &DeletionRecord::new(5, None))
        .unwrap();
    let publication = publish(
        ExportSubject::ProfileCatalog(profile),
        &unit.snapshot().unwrap(),
        &[ProfileId::new()],
        None,
    );
    assert_eq!(publication.redacted_ops, 0);
    assert_eq!(publication.to, unit.version());
    let recipient =
        ProfileCatalogUnit::from_snapshot(&publication.bytes, profile, RECIPIENT_WRITER).unwrap();
    assert_eq!(
        recipient.inner.doc.get_deep_value(),
        unit.inner.doc.get_deep_value(),
        "byte-equal state"
    );
    assert_eq!(
        recipient.active_deletions(),
        [deletion].into_iter().collect()
    );
}

// ------------------------------------------------------------ convergence

#[test]
fn redacted_and_full_replicas_keep_converging_in_both_directions() {
    let fixture = moved();
    let [a, b, _] = fixture.profiles;
    let mut full = fixture.unit;
    let subject = ExportSubject::WorkspaceCatalog(fixture.workspace);
    let first = publish(subject, &full.snapshot().unwrap(), &[a, b], None);
    let mut recipient =
        WorkspaceCatalogUnit::from_snapshot(&first.bytes, fixture.workspace, RECIPIENT_WRITER)
            .unwrap();

    // The redacted replica moves the Workspace; the full one takes it.
    let d = ProfileId::new();
    let to_d = Uuid::new_v4();
    recipient.set_placement(Some(d), to_d).unwrap();
    let updates = recipient.updates_since(&first.to).unwrap();
    assert_eq!(full.import_frames(&[updates]).unwrap(), Staged::Applied(()));
    let expected = Placement {
        profile: Some(d),
        move_id: to_d,
    };
    assert_eq!(full.placement(), Some(expected));
    assert_eq!(recipient.placement(), Some(expected));
    assert_eq!(full.version().get(&RECIPIENT_WRITER), Some(&1));
    assert_eq!(full.version(), recipient.version());

    // The full replica moves it again and records a deletion; the redacted
    // one takes the derived updates for its exact version.
    let e = ProfileId::new();
    let to_e = Uuid::new_v4();
    full.set_placement(Some(e), to_e).unwrap();
    let later = DeletionId::new();
    full.record_deletion(later, &DeletionRecord::new(400, None))
        .unwrap();
    let second = publish(
        subject,
        &full.snapshot().unwrap(),
        &[a, b],
        Some(&recipient.version()),
    );
    assert_eq!(second.kind, PayloadKind::Updates);
    assert_eq!(second.from, recipient.version());
    assert_eq!(second.to, full.version());
    assert_eq!(second.redacted_ops, 2, "counted over the whole history");
    assert_eq!(
        recipient.import_frames(&[second.bytes]).unwrap(),
        Staged::Applied(())
    );
    assert_eq!(recipient.placement().unwrap().profile, Some(e));
    assert_eq!(recipient.placement(), full.placement());
    assert_eq!(lifecycle_of(&recipient), lifecycle_of(&full));
    assert_eq!(recipient.version(), full.version());
    let recipient_history = history(&recipient.inner.doc);
    assert!(!mentions(&recipient_history, &a.to_storage()));
    assert!(!mentions(&recipient_history, &b.to_storage()));
}

#[test]
fn updates_for_a_recipient_version_apply_onto_the_derived_snapshot() {
    let fixture = moved();
    let [a, b, _] = fixture.profiles;
    let full = fixture.unit;
    let subject = ExportSubject::WorkspaceCatalog(fixture.workspace);
    let snapshot = publish(subject, &full.snapshot().unwrap(), &[a, b], None);
    let mut recipient =
        WorkspaceCatalogUnit::from_snapshot(&snapshot.bytes, fixture.workspace, RECIPIENT_WRITER)
            .unwrap();
    let held = recipient.version();

    let f = ProfileId::new();
    full.set_placement(Some(f), Uuid::new_v4()).unwrap();
    let updates = publish(subject, &full.snapshot().unwrap(), &[a, b], Some(&held));
    assert_eq!(updates.kind, PayloadKind::Updates);
    let staged = recipient
        .staged_import(std::slice::from_ref(&updates.bytes))
        .unwrap();
    assert!(!staged.is_pending());
    assert_eq!(
        recipient.import_frames(&[updates.bytes]).unwrap(),
        Staged::Applied(())
    );
    assert_eq!(recipient.placement(), full.placement());
    assert_eq!(recipient.version(), full.version());
}

#[test]
fn a_replica_that_already_knew_a_value_keeps_it() {
    let fixture = moved();
    let [a, b, _] = fixture.profiles;
    let full = fixture.unit;
    let mut other_full =
        WorkspaceCatalogUnit::from_snapshot(&full.snapshot().unwrap(), fixture.workspace, 30)
            .unwrap();
    let publication = publish(
        ExportSubject::WorkspaceCatalog(fixture.workspace),
        &full.snapshot().unwrap(),
        &[a, b],
        None,
    );
    assert_eq!(
        other_full.import_frames(&[publication.bytes]).unwrap(),
        Staged::Applied(())
    );
    assert_eq!(other_full.placement(), full.placement());
    let kept = history(&other_full.inner.doc);
    assert!(
        mentions(&kept, &a.to_storage()) && mentions(&kept, &b.to_storage()),
        "prevention of new disclosure, not retroactive erasure"
    );
}

// --------------------------------------------------------- shape fidelity

/// One map insert as a recipient's history shows it, keyed by its op id.
///
/// Deliberately not the change's `deps` array. Loro records dependencies
/// per *change*, and a change is only a batch: clearing the freeform commit
/// messages that §4.3 step 3 requires removes the boundary that split two
/// of this fixture's changes, so the derived history carries the same
/// operations in one change and the dependency that was an explicit `deps`
/// entry becomes the implicit counter adjacency inside it. The causal graph
/// is unchanged, and it is the graph §4.3 promises, so it is asserted
/// batching-independently by [`causal_parents`] instead.
#[derive(Debug, PartialEq, Eq)]
struct Insert {
    lamport: i64,
    container: String,
    key: String,
    value: serde_json::Value,
}

/// `peer@counter -> insert` for every map insert in a JSON history. Keyed
/// per op, not per change, so how Loro happened to batch changes on either
/// side does not enter the comparison.
fn inserts(json: &serde_json::Value) -> BTreeMap<String, Insert> {
    let mut out = BTreeMap::new();
    for change in json["changes"].as_array().unwrap() {
        let id = change["id"].as_str().unwrap();
        let (start, peer) = id.split_once('@').unwrap();
        let start: i64 = start.parse().unwrap();
        for op in change["ops"].as_array().unwrap() {
            if op["content"]["type"] == "insert" {
                let counter = op["counter"].as_i64().unwrap();
                out.insert(
                    format!("{peer}@{counter}"),
                    Insert {
                        lamport: change["lamport"].as_i64().unwrap() + counter - start,
                        container: op["container"].as_str().unwrap().to_string(),
                        key: op["content"]["key"].as_str().unwrap().to_string(),
                        value: op["content"]["value"].clone(),
                    },
                );
            }
        }
    }
    out
}

/// `peer@counter -> the op ids it causally depends on directly`, for every
/// operation of a JSON history, read without regard to how the history was
/// batched into changes.
///
/// A Loro change is a contiguous run of one peer's operations that share
/// one dependency frontier: a remote dependency can only appear at a
/// change's first operation, because anything new to depend on starts a new
/// change. So the first operation's parents are the change's `deps`, and
/// every later operation's sole parent is its own predecessor counter —
/// which is exactly what a `deps` entry on a change starting there would
/// have said. Two histories that agree here have the same causal graph
/// however they were grouped, and a dependency genuinely dropped by a
/// re-encode would show as a missing parent.
fn causal_parents(json: &serde_json::Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for change in json["changes"].as_array().unwrap() {
        let id = change["id"].as_str().unwrap();
        let (start, peer) = id.split_once('@').unwrap();
        let start: i64 = start.parse().unwrap();
        let deps: BTreeSet<String> = change["deps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|dep| dep.as_str().unwrap().to_string())
            .collect();
        for op in change["ops"].as_array().unwrap() {
            let counter = op["counter"].as_i64().unwrap();
            let parents = if counter == start {
                deps.clone()
            } else {
                [format!("{}@{peer}", counter - 1)].into_iter().collect()
            };
            out.insert(format!("{peer}@{counter}"), parents);
        }
    }
    out
}

#[test]
fn ids_containers_and_keys_are_preserved_and_messages_stripped() {
    let fixture = moved();
    let [a, b, c] = fixture.profiles;
    let mut unit = fixture.unit;
    // A concurrent second writer, merged back in, so the history under
    // comparison is a real graph with a cross-peer dependency and a merge
    // change — not one peer's linear run, which any grouping would satisfy.
    let other =
        WorkspaceCatalogUnit::from_snapshot(&unit.snapshot().unwrap(), fixture.workspace, 30)
            .unwrap();
    let forked = other.version();
    let concurrent = DeletionId::new();
    other
        .record_deletion(concurrent, &DeletionRecord::new(500, None))
        .unwrap();
    unit.record_restore(
        RestoreId::new(),
        &RestoreRecord {
            at_ms: 600,
            observed_deletions: vec![fixture.deletion],
            batch_id: None,
        },
    )
    .unwrap();
    assert_eq!(
        unit.import_frames(&[other.updates_since(&forked).unwrap()])
            .unwrap(),
        Staged::Applied(())
    );
    // A freeform message on one more commit; the derived history drops it.
    unit.inner.doc.set_next_commit_message("moved for the demo");
    unit.set_placement(Some(c), Uuid::new_v4()).unwrap();
    let source = history(&unit.inner.doc);
    assert!(
        source["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["msg"] == "moved for the demo")
    );
    let source_parents = causal_parents(&source);
    assert!(
        source_parents.values().any(|parents| parents.len() > 1),
        "the fixture is a graph: one operation merges two peers' histories"
    );

    let publication = publish(
        ExportSubject::WorkspaceCatalog(fixture.workspace),
        &unit.snapshot().unwrap(),
        &[a, b],
        None,
    );
    let recipient_doc = fresh_recipient(&publication.bytes);
    let derived = history(&recipient_doc);
    for change in derived["changes"].as_array().unwrap() {
        assert!(change["msg"].is_null(), "messages are stripped");
    }

    let before = inserts(&source);
    let after = inserts(&derived);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "every op id survives"
    );
    let mut nulled = 0;
    for (id, insert) in &before {
        let derived = &after[id];
        assert_eq!(
            insert.lamport, derived.lamport,
            "{id}: Lamport is preserved"
        );
        assert_eq!(
            insert.container, derived.container,
            "{id}: container ids are preserved"
        );
        assert_eq!(insert.key, derived.key, "{id}: map keys are preserved");
        if derived.value.is_null() && !insert.value.is_null() {
            nulled += 1;
            assert_eq!(insert.container, "cid:root-placement:Map");
            assert_eq!(insert.key, "current");
        } else {
            assert_eq!(insert, derived);
        }
    }
    assert_eq!(nulled, 2);

    // The same lineage, not just the same operations: every operation —
    // deletes included, which `inserts` does not see — keeps the exact
    // direct causal parents it had, and the derived document ends at the
    // authoritative version and frontier. The change-level `deps` arrays
    // may differ, because stripping the commit message lets two of the
    // source's changes merge into one; nothing causal moves with them.
    assert_eq!(
        source_parents,
        causal_parents(&derived),
        "every operation keeps its direct causal parents"
    );
    assert_eq!(
        recipient_doc.oplog_vv(),
        unit.inner.doc.oplog_vv(),
        "the derived history ends at the authoritative version"
    );
    assert_eq!(
        recipient_doc.oplog_frontiers(),
        unit.inner.doc.oplog_frontiers(),
        "and at the same frontier, so it is the same lineage"
    );
}

// ---------------------------------------------------------- personal v2

fn entity(workspace: WorkspaceId) -> EntityRef {
    EntityRef::new("task", workspace, DocumentId::new(), Uuid::new_v4()).unwrap()
}

fn text(value: &str) -> PersonalAttributeValue {
    PersonalAttributeValue::Text {
        text: value.to_string(),
    }
}

/// A v1 personal unit owned by `owner`, migrated to v2 the way the Store
/// does it, so `meta.owner_profile_id` is history and not a relabel.
fn migrated_personal(owner: ProfileId, workspace: WorkspaceId) -> (Vec<u8>, EntityRef) {
    let legacy = PersonalManifest::create(owner, workspace).unwrap();
    legacy.set_peer_id(4_242).unwrap();
    let target = entity(workspace);
    legacy
        .set_personal_attribute(&target, "note", &text("keep"))
        .unwrap();
    legacy
        .set_personal_attribute(&target, "mood", &text("gone"))
        .unwrap();
    assert!(legacy.clear_personal_attribute(&target, "mood").unwrap());
    let migrated = migrate_personal_sources(
        workspace,
        vec![LegacyPersonalSource {
            owner,
            workspace,
            snapshot: legacy.export_snapshot().unwrap(),
        }],
        77,
    )
    .unwrap();
    (migrated.snapshot, target)
}

#[test]
fn a_hidden_legacy_owner_is_redacted_and_the_personal_content_is_intact() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let (snapshot, target) = migrated_personal(owner, workspace);
    let source = PersonalManifest::from_snapshot_v2(&snapshot, workspace).unwrap();
    let source_doc = fresh_recipient(&snapshot);
    assert_eq!(
        crate::unit_doc::string(&source_doc.get_map("meta"), "owner_profile_id"),
        Some(owner.to_storage())
    );

    let publication = publish(
        ExportSubject::PersonalV2(workspace),
        &snapshot,
        &[owner],
        None,
    );
    assert_eq!(publication.redacted_ops, 1);
    let recipient = PersonalManifest::from_snapshot_v2(&publication.bytes, workspace).unwrap();
    assert!(!recipient.is_read_only());
    assert_eq!(recipient.owner(), None);
    let recipient_doc = fresh_recipient(&publication.bytes);
    for root in ["personal_attributes", "reminders"] {
        assert_eq!(
            recipient_doc.get_map(root).get_deep_value(),
            source_doc.get_map(root).get_deep_value(),
            "{root} is byte-for-byte the same deep value"
        );
    }
    assert_eq!(recipient.personal_attributes(&target)["note"], text("keep"));
    assert_eq!(recipient.personal_attributes(&target).len(), 1);
    let meta = recipient_doc.get_map("meta");
    assert!(matches!(
        meta.get("owner_profile_id"),
        Some(loro::ValueOrContainer::Value(LoroValue::Null))
    ));
    assert_eq!(
        crate::unit_doc::integer(&meta, "schema_version"),
        Some(crate::version::PERSONAL_V2_CRDT_SCHEMA_VERSION)
    );
    assert!(!mentions(&history(&recipient_doc), &owner.to_storage()));
    assert_eq!(recipient.version(), source.version());
}

#[test]
fn a_visible_legacy_owner_is_left_alone() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let (snapshot, _) = migrated_personal(owner, workspace);
    let publication = publish(
        ExportSubject::PersonalV2(workspace),
        &snapshot,
        &[ProfileId::new()],
        None,
    );
    assert_eq!(publication.redacted_ops, 0);
    assert!(PersonalManifest::from_snapshot_v2(&publication.bytes, workspace).is_ok());
    assert_eq!(
        crate::unit_doc::string(
            &fresh_recipient(&publication.bytes).get_map("meta"),
            "owner_profile_id"
        ),
        Some(owner.to_storage())
    );
}

// ---------------------------------------------------------------- refusals

/// A valid Workspace unit whose history holds `inject`'s operations on a
/// child that is deleted again, so the validator sees nothing wrong and
/// only the history walk can.
fn with_hidden_history(inject: impl FnOnce(&LoroDoc)) -> (Vec<u8>, WorkspaceId) {
    let workspace = WorkspaceId::new();
    let unit = WorkspaceCatalogUnit::create(workspace, FULL_WRITER).unwrap();
    unit.set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    let doc = &unit.inner.doc;
    inject(doc);
    doc.get_map("deletions").delete("scratch").unwrap();
    doc.commit();
    (unit.snapshot().unwrap(), workspace)
}

#[test]
fn injected_list_text_and_ordinary_child_operations_are_refused() {
    let scratch = |doc: &LoroDoc| {
        doc.get_map("deletions")
            .insert_container("scratch", LoroList::new())
            .unwrap()
            .push(1)
            .unwrap();
    };
    let (snapshot, workspace) = with_hidden_history(scratch);
    assert!(
        WorkspaceCatalogUnit::from_snapshot(&snapshot, workspace, 1).is_ok(),
        "the current state validates; only the history is wrong"
    );
    let visible = hiding(vec![]);
    let scope = ExportScope::new(&visible, capture());
    assert!(matches!(
        derive(
            ExportSubject::WorkspaceCatalog(workspace),
            &snapshot,
            &scope,
            None
        ),
        Err(ExportRefusal::UnknownOperation { kind: "list", .. })
    ));

    let (snapshot, workspace) = with_hidden_history(|doc| {
        doc.get_map("deletions")
            .insert_container("scratch", LoroText::new())
            .unwrap()
            .insert(0, "x")
            .unwrap();
    });
    assert!(matches!(
        derive(
            ExportSubject::WorkspaceCatalog(workspace),
            &snapshot,
            &scope,
            None
        ),
        Err(ExportRefusal::UnknownOperation { kind: "text", .. })
    ));

    // A map operation, but on an ordinary child container no catalog
    // writer creates: refused as a container, not passed through.
    let (snapshot, workspace) = with_hidden_history(|doc| {
        doc.get_map("deletions")
            .insert_container("scratch", LoroMap::new())
            .unwrap()
            .insert("k", 1)
            .unwrap();
    });
    assert!(matches!(
        derive(
            ExportSubject::WorkspaceCatalog(workspace),
            &snapshot,
            &scope,
            None
        ),
        Err(ExportRefusal::UnknownContainer { .. })
    ));
}

#[test]
fn undecodable_and_incomplete_sources_are_refused_before_any_walk() {
    let fixture = moved();
    let visible = hiding(vec![]);
    let scope = ExportScope::new(&visible, capture());
    let subject = ExportSubject::WorkspaceCatalog(fixture.workspace);
    assert_eq!(
        derive(subject, b"not loro", &scope, None).unwrap_err(),
        ExportRefusal::UndecodableSnapshot
    );
    // Updates since a version this device never saw: a gap, not a snapshot.
    let after_first = VersionVector::from_iter([(FULL_WRITER, 1)]);
    let partial = fixture
        .unit
        .inner
        .doc
        .export(ExportMode::updates(&after_first))
        .unwrap();
    assert_eq!(
        derive(subject, &partial, &scope, None).unwrap_err(),
        ExportRefusal::PendingAncestors
    );
    // The right bytes under the wrong address fail the subject's validator.
    assert!(matches!(
        derive(
            ExportSubject::WorkspaceCatalog(WorkspaceId::new()),
            &fixture.unit.snapshot().unwrap(),
            &scope,
            None
        ),
        Err(ExportRefusal::SourceInvalid { .. })
    ));
    assert!(matches!(
        derive(
            ExportSubject::PersonalV2(fixture.workspace),
            &fixture.unit.snapshot().unwrap(),
            &scope,
            None
        ),
        Err(ExportRefusal::SourceInvalid { .. })
    ));
}

#[test]
fn refusals_and_holds_map_to_stable_codes() {
    for (refusal, code) in [
        (ExportRefusal::UndecodableSnapshot, Code::CorruptState),
        (ExportRefusal::PendingAncestors, Code::CorruptState),
        (
            ExportRefusal::SourceInvalid { detail: "x".into() },
            Code::CorruptState,
        ),
        (
            ExportRefusal::FormatTooNew {
                found: 2,
                understood: 1,
            },
            Code::SchemaTooNew,
        ),
        (ExportRefusal::PeerCompression, Code::CrdtRejected),
        (
            ExportRefusal::UnknownOperation {
                container: "c".into(),
                kind: "text",
            },
            Code::CrdtRejected,
        ),
        (
            ExportRefusal::UnknownContainer {
                container: "c".into(),
            },
            Code::CrdtRejected,
        ),
        (
            ExportRefusal::MalformedReference {
                path: "placement.current".into(),
            },
            Code::CorruptState,
        ),
        (
            ExportRefusal::RedactionFailed { detail: "x".into() },
            Code::CrdtRejected,
        ),
        (ExportRefusal::DerivedPending, Code::CrdtRejected),
        (
            ExportRefusal::DerivedInvalid { detail: "x".into() },
            Code::CorruptState,
        ),
        (
            ExportRefusal::DerivedStateDiffers {
                path: "meta".into(),
            },
            Code::CorruptState,
        ),
        (ExportRefusal::Crdt, Code::StorageFailed),
    ] {
        assert_eq!(Error::from(refusal.clone()).code(), code, "{refusal}");
    }
    for reason in [
        HoldReason::CurrentPlacementHidden,
        HoldReason::OwnerHidden,
        HoldReason::LifecycleDiffers,
    ] {
        assert_eq!(Error::from(reason).code(), Code::Forbidden);
    }
}

#[test]
fn a_derived_key_is_the_subject_version_scope_and_generation() {
    let fixture = moved();
    let subject = ExportSubject::WorkspaceCatalog(fixture.workspace);
    let key = DerivedKey::new(subject, &fixture.unit.version(), 0xf1, 7);
    assert_eq!(
        key,
        DerivedKey::new(subject, &fixture.unit.version(), 0xf1, 7)
    );
    assert_eq!(
        key.authoritative_vv,
        [(
            FULL_WRITER,
            *fixture.unit.version().get(&FULL_WRITER).unwrap()
        )]
        .into_iter()
        .collect()
    );
    fixture
        .unit
        .set_placement(Some(ProfileId::new()), Uuid::new_v4())
        .unwrap();
    assert_ne!(
        key,
        DerivedKey::new(subject, &fixture.unit.version(), 0xf1, 7)
    );
    assert_ne!(
        key,
        DerivedKey::new(subject, &fixture.unit.version(), 0xf2, 7)
    );
    assert_ne!(
        key,
        DerivedKey::new(subject, &fixture.unit.version(), 0xf1, 8)
    );
    assert_ne!(
        key,
        DerivedKey::new(
            ExportSubject::PersonalV2(fixture.workspace),
            &fixture.unit.version(),
            0xf1,
            7
        )
    );
}
