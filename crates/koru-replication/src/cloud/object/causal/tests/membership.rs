// SPDX-License-Identifier: MPL-2.0
use super::*;
use loro::{LoroMap, TreeID, TreeParentId};

fn manifest(version: i64) -> LoroDoc {
    let doc = LoroDoc::new();
    let meta = doc.get_map("meta");
    meta.insert("schema_version", version).unwrap();
    meta.insert(
        "workspace_id",
        identity::uuid_text(identity::parts(template(0).dataset).1),
    )
    .unwrap();
    meta.insert("name", "Private workspace").unwrap();
    meta.insert("created_at", 0).unwrap();
    doc.get_tree("documents");
    doc
}
fn entry(doc: &LoroDoc, parent: TreeParentId, n: u8) -> (TreeID, LoroMap, CloudId) {
    let tree = doc.get_tree("documents");
    let node = tree.create(parent).unwrap();
    let map = tree.get_meta(node).unwrap();
    let id = CloudId::from_bytes([n; 16]);
    map.insert("doc_id", identity::uuid_text(id)).unwrap();
    map.insert("title", format!("Private title {n}")).unwrap();
    map.insert("kind", "page").unwrap();
    (node, map, id)
}
fn inspect(doc: &LoroDoc, ids: &[CloudId]) -> Result<ManifestClassification, StrictReferenceError> {
    let raw = snapshot_object(0, doc);
    let bound = bind_fixture(&raw);
    let content = restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap();
    content.strict_manifest_classification(ids, ReferenceBudget::default())
}
fn deletion(map: &LoroMap) -> LoroMap {
    let events = map.ensure_mergeable_map("deletions").unwrap();
    let event = events
        .ensure_mergeable_map(&uuid::Uuid::new_v4().to_string())
        .unwrap();
    event.insert("at", 17).unwrap();
    event
}

#[test]
fn complete_manifest_proves_selected_live_inherited_trash_and_absent_only() {
    let doc = manifest(2);
    let (parent, trash, one) = entry(&doc, TreeParentId::Root, 1);
    deletion(&trash);
    let (_, _, two) = entry(&doc, TreeParentId::Node(parent), 2);
    let (_, _, three) = entry(&doc, TreeParentId::Root, 3);
    let four = CloudId::from_bytes([4; 16]);
    let result = inspect(&doc, &[one, two, three, four]).unwrap();
    assert_eq!(
        result
            .selected()
            .iter()
            .map(|row| row.membership())
            .collect::<Vec<_>>(),
        [
            ManifestMembership::Trash,
            ManifestMembership::Trash,
            ManifestMembership::Listed,
            ManifestMembership::Unfiled
        ]
    );
    assert_eq!(result.selected()[3].display_name(), None);
    assert!(!format!("{result:?}").contains("Private"));
    assert_eq!(result.classifier_version(), "manifest-membership-v1");
    doc.get_tree("documents").delete(parent).unwrap();
    let after = inspect(&doc, &[one, two]).unwrap();
    assert!(
        after
            .selected()
            .iter()
            .all(|row| row.membership() == ManifestMembership::Unfiled)
    );
}

#[test]
fn map_precedence_legacy_mixed_upgrade_and_restored_ancestors_match_store() {
    for version in [1, 2] {
        let doc = manifest(version);
        let (parent, map, one) = entry(&doc, TreeParentId::Root, 1);
        let (_, child, two) = entry(&doc, TreeParentId::Node(parent), 2);
        map.insert("trashed_at", 12).unwrap();
        assert_eq!(
            inspect(&doc, &[two]).unwrap().selected()[0].membership(),
            ManifestMembership::Trash
        );
        doc.get_map("meta").insert("schema_version", 2).unwrap();
        map.ensure_mergeable_map("deletions").unwrap(); // restored despite stale hint
        deletion(&child);
        let result = inspect(&doc, &[one, two]).unwrap();
        assert_eq!(
            result.selected()[0].membership(),
            ManifestMembership::Listed
        );
        assert_eq!(result.selected()[1].membership(), ManifestMembership::Trash);
    }
}

#[test]
fn malformed_unselected_entries_and_events_never_prove_unfiled() {
    for bad in [
        "duplicate",
        "missing",
        "field",
        "event",
        "v1-events",
        "scalar",
    ] {
        let doc = manifest(2);
        let (_, map, one) = entry(&doc, TreeParentId::Root, 1);
        match bad {
            "duplicate" => {
                entry(&doc, TreeParentId::Root, 1);
            }
            "missing" => {
                map.delete("doc_id").unwrap();
            }
            "field" => {
                map.insert("future", "hidden").unwrap();
            }
            "event" => {
                deletion(&map).insert("at", "bad").unwrap();
            }
            "v1-events" => {
                deletion(&map);
                doc.get_map("meta").insert("schema_version", 1).unwrap();
            }
            "scalar" => {
                map.insert("title", 4).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            inspect(&doc, &[CloudId::from_bytes([9; 16])]).is_err(),
            "{bad}"
        );
        assert!(inspect(&doc, &[one]).is_err(), "{bad}");
    }
}

#[test]
fn selected_bounds_names_and_empty_selection_still_validate_whole_manifest() {
    let doc = manifest(2);
    let (_, map, one) = entry(&doc, TreeParentId::Root, 1);
    map.insert("title", "🦀".repeat(128)).unwrap();
    assert_eq!(
        inspect(&doc, &[one]).unwrap().selected()[0]
            .display_name()
            .unwrap()
            .len(),
        512
    );
    map.insert("title", "a".repeat(129)).unwrap();
    assert_eq!(
        inspect(&doc, &[one]).unwrap().selected()[0].display_name(),
        None
    );
    assert!(inspect(&doc, &[one, one]).is_err());
    assert!(inspect(&doc, &[one; 101]).is_err());
    assert!(inspect(&doc, &[]).unwrap().selected().is_empty());
    map.insert("unknown", 1).unwrap();
    assert!(inspect(&doc, &[]).is_err());
}

#[test]
fn concurrent_unseen_deletion_survives_restore_in_strict_classification() {
    let doc = manifest(2);
    let (node, map, id) = entry(&doc, TreeParentId::Root, 1);
    deletion(&map);
    doc.commit();
    let other = doc.fork();
    let events = map.ensure_mergeable_map("deletions").unwrap();
    for key in events.keys() {
        events.delete(&key).unwrap();
    }
    let other_map = other.get_tree("documents").get_meta(node).unwrap();
    deletion(&other_map);
    doc.import(&other.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    assert_eq!(
        inspect(&doc, &[id]).unwrap().selected()[0].membership(),
        ManifestMembership::Trash
    );
    for key in events.keys() {
        events.delete(&key).unwrap();
    }
    assert_eq!(
        inspect(&doc, &[id]).unwrap().selected()[0].membership(),
        ManifestMembership::Listed
    );
}

#[test]
fn strict_manifest_traversal_budgets_fail_without_partial_rows() {
    let doc = manifest(2);
    let (parent, _, one) = entry(&doc, TreeParentId::Root, 1);
    entry(&doc, TreeParentId::Node(parent), 2);
    let raw = snapshot_object(0, &doc);
    let bound = bind_fixture(&raw);
    let content = restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap();
    for budget in [
        ReferenceBudget {
            nodes: 1,
            ..Default::default()
        },
        ReferenceBudget {
            fields: 1,
            ..Default::default()
        },
        ReferenceBudget {
            depth: 0,
            ..Default::default()
        },
        ReferenceBudget {
            value_bytes: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            content
                .strict_manifest_classification(&[one], budget)
                .unwrap_err(),
            StrictReferenceError::Budget
        );
    }
}
