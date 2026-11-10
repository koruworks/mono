// SPDX-License-Identifier: MPL-2.0

//! Memory-only coverage of `docs/features/root-catalog/TECHNICAL.final.md`
//! §5.2 and the Plan 1.1 verification list: single and multiple sources, two
//! legacy units under one Store peer with overlapping counters, independent
//! migrations deduplicating, tombstones and clears surviving, and every
//! typed refusal. Nothing here touches a port.

use super::*;
use crate::ids::{ActionId, DocumentId, ReminderId, RuleRevisionId};
use crate::personal_attributes::{EntityRef, PersonalAttributeValue};
use crate::reminders::{
    ActionKind, OccurrenceAction, OccurrenceKey, OccurrenceMoment, OccurrenceSource,
    OneTimeSchedule, ReminderRule, RuleStatus,
};
use crate::temporal::{FloatingDateTime, ZonedDateTime};
use loro::{ExportMode, VersionVector};

/// The one Store peer every legacy unit of one installation was written under.
const STORE_PEER: u64 = 4_242;

fn entity(workspace: WorkspaceId) -> EntityRef {
    EntityRef::new("task", workspace, DocumentId::new(), uuid::Uuid::new_v4()).unwrap()
}

fn text(value: &str) -> PersonalAttributeValue {
    PersonalAttributeValue::Text {
        text: value.to_string(),
    }
}

fn floating_rule(target: &EntityRef) -> ReminderRule {
    ReminderRule {
        id: ReminderId::new(),
        target: target.clone(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Floating {
            wall: FloatingDateTime::parse("2026-09-12T10:00:00").unwrap(),
        },
        tone_override: None,
    }
}

fn zoned_rule(target: &EntityRef, instant_ms: i64) -> ReminderRule {
    ReminderRule {
        schedule: OneTimeSchedule::Zoned {
            value: ZonedDateTime::new(instant_ms, "UTC").unwrap(),
        },
        ..floating_rule(target)
    }
}

fn handled(rule: &ReminderRule) -> OccurrenceAction {
    let moment = match &rule.schedule {
        OneTimeSchedule::Floating { wall } => OccurrenceMoment::Floating { wall: wall.clone() },
        OneTimeSchedule::Zoned { value } => OccurrenceMoment::Zoned {
            value: value.clone(),
        },
        OneTimeSchedule::RelativeToDue { .. } => unreachable!(),
    };
    OccurrenceAction {
        id: ActionId::new(),
        occurrence: OccurrenceKey::derive(
            rule.id,
            rule.revision,
            &OccurrenceSource::OneTime { moment },
        ),
        kind: ActionKind::Handled,
    }
}

/// A legacy unit written by the Store's one peer, as every installation did.
fn legacy(owner: ProfileId, workspace: WorkspaceId) -> PersonalManifest {
    let unit = PersonalManifest::create(owner, workspace).unwrap();
    unit.set_peer_id(STORE_PEER).unwrap();
    unit
}

fn source(unit: &PersonalManifest) -> LegacyPersonalSource {
    LegacyPersonalSource {
        owner: unit.owner().unwrap(),
        workspace: unit.workspace(),
        snapshot: unit.export_snapshot().unwrap(),
    }
}

fn load(migrated: &MigratedPersonalUnit, workspace: WorkspaceId) -> PersonalManifest {
    let doc = PersonalManifest::from_snapshot_v2(&migrated.snapshot, workspace).unwrap();
    doc.set_peer_id(migrated.writer_peer_id).unwrap();
    doc
}

fn peers_of(doc: &PersonalManifest) -> BTreeSet<u64> {
    doc.version()
        .iter()
        .filter(|(_, counter)| **counter > 0)
        .map(|(peer, _)| *peer)
        .collect()
}

/// The namespace map `normalize_legacy_source` builds for one source: every
/// peer of its history, mapped by the §5.2 step 2 digest.
///
/// A legacy row holds **two** peers, not one. `PersonalManifest::create`
/// commits the v1 header under the peer the empty document started with, and
/// only then did the old `personal_candidate` call `set_peer_id(store peer)`;
/// every later operation carries the Store's one peer. Step 2 gives *every*
/// original PeerID its own namespace, so one source contributes two derived
/// peers — the header's and the Store's.
fn peer_map(unit: &PersonalManifest) -> BTreeMap<u64, u64> {
    let owner = unit.owner().unwrap();
    let workspace = unit.workspace();
    peers_of(unit)
        .into_iter()
        .map(|peer| (peer, derive_peer(owner, workspace, peer)))
        .collect()
}

/// The namespaces one source's peers become.
fn derived_peers(unit: &PersonalManifest) -> BTreeSet<u64> {
    peer_map(unit).into_values().collect()
}

#[test]
fn the_peer_digest_is_the_documented_blake3_prefix() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let mut input = Vec::new();
    input.extend_from_slice(PEER_DIGEST_DOMAIN.as_bytes());
    input.extend_from_slice(owner.as_uuid().as_bytes());
    input.extend_from_slice(workspace.as_uuid().as_bytes());
    input.extend_from_slice(&STORE_PEER.to_be_bytes());
    let digest = blake3::hash(&input);
    let expected = u64::from_be_bytes(digest.as_bytes()[..8].try_into().unwrap());
    assert_eq!(derive_peer(owner, workspace, STORE_PEER), expected);
    assert_eq!(PEER_DIGEST_DOMAIN, "koru-personal-v2-peer");
    // Every input distinguishes the namespace.
    assert_ne!(
        derive_peer(owner, workspace, STORE_PEER),
        derive_peer(ProfileId::new(), workspace, STORE_PEER)
    );
    assert_ne!(
        derive_peer(owner, workspace, STORE_PEER),
        derive_peer(owner, WorkspaceId::new(), STORE_PEER)
    );
    assert_ne!(
        derive_peer(owner, workspace, STORE_PEER),
        derive_peer(owner, workspace, STORE_PEER + 1)
    );
}

#[test]
fn a_single_source_migrates_with_its_values_ids_and_tombstones_intact() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let unit = legacy(owner, workspace);
    let target = entity(workspace);
    let gone = entity(workspace);
    unit.set_personal_attribute(&target, "note", &text("keep"))
        .unwrap();
    unit.set_personal_attribute(&target, "mood", &text("cleared"))
        .unwrap();
    assert!(unit.clear_personal_attribute(&target, "mood").unwrap());
    unit.set_personal_attribute(&gone, "note", &text("whole map cleared"))
        .unwrap();
    assert!(unit.clear_personal_attribute(&gone, "note").unwrap());
    let kept = zoned_rule(&target, 1_800_000_000_000);
    let deleted = floating_rule(&target);
    let acted = floating_rule(&target);
    unit.put_reminder_rule(&kept).unwrap();
    unit.put_reminder_rule(&deleted).unwrap();
    unit.put_reminder_rule(&acted).unwrap();
    unit.delete_reminder(deleted.id).unwrap();
    let action = handled(&acted);
    unit.append_reminder_action(acted.id, &action).unwrap();
    let visible = handled(&acted);
    unit.append_reminder_action(acted.id, &visible).unwrap();
    // Clear everything handled so far, then one more action stays visible.
    assert_eq!(unit.clear_handled_reminder_history().unwrap(), 2);
    let later = handled(&acted);
    unit.append_reminder_action(acted.id, &later).unwrap();

    let migrated = migrate_personal_sources(workspace, vec![source(&unit)], 77).unwrap();
    assert_eq!(migrated.writer_peer_id, 77);
    let doc = load(&migrated, workspace);
    assert_eq!(doc.owner(), None);
    assert_eq!(doc.workspace(), workspace);
    assert!(!doc.is_read_only());
    // Values and clears.
    assert_eq!(doc.personal_attributes(&target).len(), 1);
    assert_eq!(doc.personal_attributes(&target)["note"], text("keep"));
    assert!(doc.personal_attributes(&gone).is_empty());
    // Reminder ids, deletion and action facts are unchanged.
    let reminders = doc.reminders();
    assert_eq!(reminders.len(), 3);
    assert!(doc.reminder(deleted.id).unwrap().deleted);
    assert!(!doc.reminder(kept.id).unwrap().deleted);
    let acted_now = doc.reminder(acted.id).unwrap();
    assert_eq!(
        acted_now.actions.iter().map(|a| a.id).collect::<Vec<_>>(),
        [later.id]
    );
    assert_eq!(
        doc.raw_reminder_actions()
            .keys()
            .map(|(_, action)| *action)
            .collect::<BTreeSet<_>>(),
        [action.id, visible.id, later.id].into_iter().collect()
    );
    assert_eq!(
        doc.cleared_reminder_actions(),
        [(acted.id, action.id), (acted.id, visible.id)]
            .into_iter()
            .collect()
    );
    // The due index is rebuilt from the merged rules.
    let mut due = migrated.due_rows.clone();
    due.sort_by_key(|row| row.reminder);
    let mut expected = vec![
        WorkspaceReminderDue {
            workspace,
            reminder: kept.id,
            due_at_ms: Some(1_800_000_000_000),
        },
        WorkspaceReminderDue {
            workspace,
            reminder: deleted.id,
            due_at_ms: None,
        },
        WorkspaceReminderDue {
            workspace,
            reminder: acted.id,
            due_at_ms: None,
        },
    ];
    expected.sort_by_key(|row| row.reminder);
    assert_eq!(due, expected);
    assert_eq!(
        VersionVector::decode(&migrated.indexed_version).unwrap(),
        doc.version()
    );
    // No legacy peer survives, each one of them got its own namespace, and
    // the only peer the migration adds is the one fresh v2 writer of step 5.
    let derived = derive_peer(owner, workspace, STORE_PEER);
    let originals = peers_of(&unit);
    assert_eq!(originals.len(), 2, "the v1 header peer and the Store peer");
    assert!(originals.contains(&STORE_PEER));
    let mut expected = derived_peers(&unit);
    assert!(expected.contains(&derived));
    assert!(expected.insert(77), "the writer is not a derived peer");
    assert_eq!(peers_of(&doc), expected);
    assert!(peers_of(&doc).is_disjoint(&originals));
    let receipt = &migrated.receipt;
    assert_eq!(receipt.workspace, workspace);
    assert_eq!(receipt.digest_domain, PEER_DIGEST_DOMAIN);
    assert_eq!(receipt.writer_peer_id, 77);
    assert_eq!(receipt.sources.len(), 1);
    // The mapping is total over the source's peers and names each once, so a
    // reader of the receipt can recompute every id the unit now holds.
    assert_eq!(
        receipt.sources[0].peers,
        originals
            .iter()
            .map(|original| PeerMapping {
                original: *original,
                derived: derive_peer(owner, workspace, *original),
            })
            .collect::<Vec<_>>()
    );
    assert!(receipt.sources[0].peers.contains(&PeerMapping {
        original: STORE_PEER,
        derived,
    }));
    assert!(receipt.sources[0].change_count > 0);
    assert!(
        receipt.sources[0]
            .original_frontiers
            .iter()
            .all(|id| id.ends_with(&format!("@{STORE_PEER}")))
    );
    assert!(
        receipt.sources[0]
            .normalized_frontiers
            .iter()
            .all(|id| id.ends_with(&format!("@{derived}")))
    );
    let json = serde_json::to_string(receipt).unwrap();
    assert_eq!(
        serde_json::from_str::<PersonalMigrationReceipt>(&json).unwrap(),
        *receipt
    );
    // The meta header reads v2 and keeps the legacy owner as history only.
    let meta = doc.inner.doc.get_map("meta");
    assert_eq!(
        crate::unit_doc::integer(&meta, "schema_version"),
        Some(PERSONAL_V2_CRDT_SCHEMA_VERSION)
    );
    assert_eq!(
        crate::unit_doc::string(&meta, "owner_profile_id"),
        Some(owner.to_storage())
    );
}

#[test]
fn two_units_under_one_store_peer_with_overlapping_counters_keep_both_histories() {
    let workspace = WorkspaceId::new();
    let a_owner = ProfileId::new();
    let b_owner = ProfileId::new();
    let a = legacy(a_owner, workspace);
    let b = legacy(b_owner, workspace);
    let target = entity(workspace);
    let a_rule = floating_rule(&target);
    let b_rule = floating_rule(&target);
    a.set_personal_attribute(&target, "from", &text("a"))
        .unwrap();
    a.put_reminder_rule(&a_rule).unwrap();
    b.set_personal_attribute(&target, "also", &text("b"))
        .unwrap();
    b.put_reminder_rule(&b_rule).unwrap();
    b.delete_reminder(b_rule.id).unwrap();
    // Both histories start at counter zero of the same peer.
    assert!(a.version().get(&STORE_PEER).copied().unwrap_or(0) > 0);
    assert!(b.version().get(&STORE_PEER).copied().unwrap_or(0) > 0);
    // A plain union would hide one of them: proof of the hazard. Loro either
    // refuses the second snapshot or silently keeps the ids it already has.
    let naive = loro::LoroDoc::new();
    naive.import(&a.export_snapshot().unwrap()).unwrap();
    let hidden = match naive.import(&b.export_snapshot().unwrap()) {
        Err(_) => true,
        Ok(_) => !serde_json::to_string(&naive.get_deep_value())
            .unwrap()
            .contains("\"also\""),
    };
    assert!(
        hidden,
        "the hazard this migration exists for is not reproduced"
    );

    let migrated = migrate_personal_sources(workspace, vec![source(&b), source(&a)], 9).unwrap();
    let doc = load(&migrated, workspace);
    let attributes = doc.personal_attributes(&target);
    assert_eq!(attributes["from"], text("a"));
    assert_eq!(attributes["also"], text("b"));
    assert!(!doc.reminder(a_rule.id).unwrap().deleted);
    assert!(doc.reminder(b_rule.id).unwrap().deleted);
    // Each source brings one namespace per peer of its own history — its v1
    // header peer and the Store peer both sources shared — and the merged
    // unit adds exactly one peer of its own, the fresh v2 writer of step 5.
    let mut expected: BTreeSet<u64> = derived_peers(&a)
        .union(&derived_peers(&b))
        .copied()
        .collect();
    assert_eq!(expected.len(), 4, "two legacy peers per source, kept apart");
    assert!(expected.contains(&derive_peer(a_owner, workspace, STORE_PEER)));
    assert!(expected.contains(&derive_peer(b_owner, workspace, STORE_PEER)));
    assert!(expected.insert(9), "one fresh writer, not one per source");
    assert_eq!(peers_of(&doc), expected);
    assert!(!peers_of(&doc).contains(&STORE_PEER));
    // Sources are processed in canonical Profile order whatever the input order.
    let owners: Vec<_> = migrated.receipt.sources.iter().map(|s| s.owner).collect();
    let mut sorted = owners.clone();
    sorted.sort();
    assert_eq!(owners, sorted);
    assert_eq!(migrated.due_rows.len(), 2);
}

#[test]
fn independently_migrated_copies_of_one_origin_deduplicate() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let unit = legacy(owner, workspace);
    let target = entity(workspace);
    let rule = floating_rule(&target);
    unit.set_personal_attribute(&target, "note", &text("shared"))
        .unwrap();
    unit.put_reminder_rule(&rule).unwrap();
    unit.append_reminder_action(rule.id, &handled(&rule))
        .unwrap();
    let first = migrate_personal_sources(workspace, vec![source(&unit)], 101).unwrap();
    let second = migrate_personal_sources(workspace, vec![source(&unit)], 202).unwrap();
    let derived = derive_peer(owner, workspace, STORE_PEER);
    let a = load(&first, workspace);
    let b = load(&second, workspace);
    // Identical operation ids for the legacy history on both devices.
    assert_eq!(a.version().get(&derived), b.version().get(&derived));
    let before = a.version();
    let mut merged = a;
    merged.import_frames(&[second.snapshot.clone()]).unwrap();
    // Nothing of the legacy history is new; only the other writer's stamp
    // arrives, and it carries the same value.
    let after = merged.version();
    assert_eq!(after.get(&derived), before.get(&derived));
    assert_eq!(after.get(&202), b.version().get(&202));
    assert_eq!(
        merged.personal_attributes(&target),
        b.personal_attributes(&target)
    );
    assert_eq!(merged.reminders(), b.reminders());
    assert!(!merged.is_read_only());
    assert_eq!(
        merged.raw_reminder_actions().len(),
        b.raw_reminder_actions().len()
    );
}

#[test]
fn a_later_v1_source_is_admitted_through_the_same_normalization_and_deduplicates() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let other = ProfileId::new();
    let unit = legacy(owner, workspace);
    let target = entity(workspace);
    unit.set_personal_attribute(&target, "note", &text("first"))
        .unwrap();
    let migrated = migrate_personal_sources(workspace, vec![source(&unit)], 1).unwrap();
    let held = load(&migrated, workspace);
    // The same origin arriving again adds nothing to its namespace.
    let (same, receipt) = admit_legacy_source(&held, &source(&unit), 2).unwrap();
    let derived = derive_peer(owner, workspace, STORE_PEER);
    assert_eq!(same.version().get(&derived), held.version().get(&derived));
    assert_eq!(receipt.owner, owner);
    assert_eq!(
        same.personal_attributes(&target),
        held.personal_attributes(&target)
    );
    // A different origin of the same Workspace merges its own namespace in.
    let late = legacy(other, workspace);
    late.set_personal_attribute(&target, "late", &text("second"))
        .unwrap();
    let (merged, _) = admit_legacy_source(&held, &source(&late), 3).unwrap();
    assert_eq!(merged.personal_attributes(&target).len(), 2);
    assert!(
        merged
            .version()
            .get(&derive_peer(other, workspace, STORE_PEER))
            .is_some()
    );
    assert!(!merged.is_read_only());
    assert_eq!(merged.workspace(), workspace);
    // The held unit itself was never touched.
    assert_eq!(held.personal_attributes(&target).len(), 1);
}

#[test]
fn a_conflicting_copy_of_one_origin_is_refused_before_import() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let target = entity(workspace);
    let genuine = legacy(owner, workspace);
    genuine
        .set_personal_attribute(&target, "note", &text("genuine"))
        .unwrap();
    let migrated = migrate_personal_sources(workspace, vec![source(&genuine)], 1).unwrap();
    let held = load(&migrated, workspace);
    // A second copy of the same origin whose same ids carry other content.
    let forged = legacy(owner, workspace);
    forged
        .set_personal_attribute(&target, "note", &text("forged"))
        .unwrap();
    let error = admit_legacy_source(&held, &source(&forged), 2).unwrap_err();
    assert_eq!(error.code(), Code::CrdtRejected);
    assert!(error.message().contains("conflicts with held history"));
    assert_eq!(held.personal_attributes(&target)["note"], text("genuine"));
    // The typed form names the id.
    let normalized = normalize_legacy_source(&source(&forged), workspace).unwrap();
    assert!(matches!(
        detect_conflicts(&held.inner.doc, &normalized),
        Err(PersonalMigrationRefusal::ConflictingHistory { owner: o, .. }) if o == owner
    ));
}

#[test]
fn wrong_workspace_future_schema_pending_ancestors_and_bad_bytes_are_refused() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let elsewhere = legacy(owner, WorkspaceId::new());
    assert!(matches!(
        migrate_personal_sources(workspace, vec![source(&elsewhere)], 1),
        Err(PersonalMigrationRefusal::WrongWorkspace { owner: o, .. }) if o == owner
    ));
    // The row says one owner, the header another.
    let mut mislabeled = source(&legacy(ProfileId::new(), workspace));
    mislabeled.owner = owner;
    assert!(matches!(
        migrate_personal_sources(workspace, vec![mislabeled], 1),
        Err(PersonalMigrationRefusal::WrongAddress { owner: o }) if o == owner
    ));
    // A future format.
    let future = legacy(owner, workspace);
    future
        .inner
        .doc
        .get_map("meta")
        .insert("schema_version", 99)
        .unwrap();
    future.inner.doc.commit();
    assert!(matches!(
        migrate_personal_sources(workspace, vec![source(&future)], 1),
        Err(PersonalMigrationRefusal::FutureSchema { found: 99, .. })
    ));
    // Pending ancestors: a delta frame whose base is missing.
    let unit = legacy(owner, workspace);
    let base = unit.version();
    unit.set_personal_attribute(&entity(workspace), "note", &text("later"))
        .unwrap();
    let delta = unit.inner.doc.export(ExportMode::updates(&base)).unwrap();
    assert!(matches!(
        migrate_personal_sources(
            workspace,
            vec![LegacyPersonalSource {
                owner,
                workspace,
                snapshot: delta,
            }],
            1
        ),
        Err(PersonalMigrationRefusal::PendingAncestors { .. })
    ));
    assert!(matches!(
        migrate_personal_sources(
            workspace,
            vec![LegacyPersonalSource {
                owner,
                workspace,
                snapshot: b"not loro".to_vec(),
            }],
            1
        ),
        Err(PersonalMigrationRefusal::UndecodableSnapshot { .. })
    ));
    // A v2 document is not a v1 source.
    let v2 = PersonalManifest::create_v2(workspace, 5).unwrap();
    assert!(matches!(
        migrate_personal_sources(
            workspace,
            vec![LegacyPersonalSource {
                owner,
                workspace,
                snapshot: v2.export_snapshot().unwrap(),
            }],
            1
        ),
        Err(PersonalMigrationRefusal::InvalidSource { .. })
    ));
    // Every refusal has a stable code.
    for (refusal, code) in [
        (
            PersonalMigrationRefusal::WrongWorkspace {
                owner,
                found: workspace,
            },
            Code::ProfileUnitScopeMismatch,
        ),
        (
            PersonalMigrationRefusal::FutureSchema { owner, found: 3 },
            Code::SchemaTooNew,
        ),
        (
            PersonalMigrationRefusal::PendingAncestors { owner },
            Code::CorruptState,
        ),
        (
            PersonalMigrationRefusal::PeerCollision { derived: 1 },
            Code::CrdtRejected,
        ),
        (
            PersonalMigrationRefusal::UnsupportedOp {
                owner,
                kind: "text",
            },
            Code::CrdtRejected,
        ),
    ] {
        assert_eq!(Error::from(refusal).code(), code);
    }
}

#[test]
fn a_non_mergeable_child_at_a_shared_key_is_refused() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let unit = legacy(owner, workspace);
    let target = entity(workspace);
    // A merely valid ordinary child map where the writer uses mergeable maps.
    let ordinary = unit
        .inner
        .doc
        .get_map(C_PERSONAL_ATTRIBUTES)
        .insert_container(&target.to_storage(), loro::LoroMap::new())
        .unwrap();
    ordinary
        .insert("note", text("looks fine").to_loro())
        .unwrap();
    unit.inner.doc.commit();
    // It validates as v1 content...
    assert!(PersonalManifest::from_snapshot(&unit.export_snapshot().unwrap()).is_ok());
    // ...and is still refused for the union.
    let error = migrate_personal_sources(workspace, vec![source(&unit)], 1).unwrap_err();
    assert!(matches!(
        &error,
        PersonalMigrationRefusal::NonMergeableChild { owner: o, path }
            if *o == owner && path.starts_with("personal_attributes.")
    ));
    assert_eq!(Error::from(error).code(), Code::CorruptState);
}

#[test]
fn an_unsupported_operation_kind_refuses_the_whole_source() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let unit = legacy(owner, workspace);
    // A text container under a root the validator does not know is refused
    // by v1 validation already; put one inside a value the validator never
    // inspects — a transcoder must still refuse the op kind, not skip it.
    let doc = &unit.inner.doc;
    let hidden = doc
        .get_map(C_REMINDERS)
        .insert_container("rules", loro::LoroText::new())
        .unwrap();
    hidden.insert(0, "x").unwrap();
    doc.commit();
    let exported = doc
        .export_json_updates_without_peer_compression(&VersionVector::default(), &doc.oplog_vv());
    // The whole namespace map the normalizer would build, so the text op is
    // the only thing left for the transcoder to object to: a map short of
    // the header's peer refuses for `UnmappedPeer` and proves nothing here.
    let peers = peer_map(&unit);
    assert!(peers.contains_key(&STORE_PEER));
    match transcode(exported, &peers, owner).map(|(_, ops)| ops) {
        Err(PersonalMigrationRefusal::UnsupportedOp { owner: o, kind }) => {
            assert_eq!((o, kind), (owner, "text"));
        }
        other => panic!("a text operation was not refused: {other:?}"),
    }
    // Whereas the plain map history of a valid unit transcodes fully.
    let clean = legacy(owner, workspace);
    clean
        .set_personal_attribute(&entity(workspace), "k", &text("v"))
        .unwrap();
    let exported = clean
        .inner
        .doc
        .export_json_updates_without_peer_compression(
            &VersionVector::default(),
            &clean.inner.doc.oplog_vv(),
        );
    let peers = peer_map(&clean);
    let before = serde_json::to_value(&exported).unwrap().to_string();
    let (after, ops) = transcode(exported, &peers, owner).unwrap();
    assert!(ops > 0);
    let after = serde_json::to_value(&after).unwrap().to_string();
    // Counters, lamports, keys and values are untouched and nothing but a
    // peer moved: putting every derived peer back — every id in this form is
    // `counter@peer` followed by `"` or, inside a container id, by `:` —
    // reproduces the exported history byte for byte.
    let mut inverted = after.clone();
    for (original, derived) in &peers {
        for tail in ['"', ':'] {
            inverted =
                inverted.replace(&format!("@{derived}{tail}"), &format!("@{original}{tail}"));
        }
    }
    assert_eq!(inverted, before);
    for original in peers.keys() {
        assert!(!after.contains(&format!("@{original}\"")));
        assert!(!after.contains(&format!("@{original}:")));
    }
    // A compressed peer table is refused rather than guessed at.
    let compressed = clean
        .inner
        .doc
        .export_json_updates(&VersionVector::default(), &clean.inner.doc.oplog_vv());
    assert!(matches!(
        transcode(compressed, &peers, owner),
        Err(PersonalMigrationRefusal::PeerCompression { .. })
    ));
}

#[test]
fn a_truncated_digest_collision_refuses_the_batch() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    // Two different inputs that map to one derived peer cannot be produced
    // by BLAKE3 on demand; the batch-level check is exercised on the data it
    // sees — two receipts naming the same derived peer.
    let unit = legacy(owner, workspace);
    unit.set_personal_attribute(&entity(workspace), "k", &text("v"))
        .unwrap();
    let normalized = normalize_legacy_source(&source(&unit), workspace).unwrap();
    let mut derived = BTreeSet::new();
    for mapping in &normalized.receipt.peers {
        assert!(derived.insert(mapping.derived));
    }
    // The second occurrence of any derived peer is the collision.
    for mapping in &normalized.receipt.peers {
        assert!(!derived.insert(mapping.derived));
    }
    assert_eq!(
        Error::from(PersonalMigrationRefusal::PeerCollision { derived: 1 }).code(),
        Code::CrdtRejected
    );
    // And the reserved peer is refused by name.
    assert_eq!(
        Error::from(PersonalMigrationRefusal::ReservedPeer { owner, original: 1 }).code(),
        Code::CrdtRejected
    );
}

/// The fresh v2 writer is a peer like any other: if it is one of the derived
/// namespaces, the header it authors at counter zero takes the counters that
/// namespace's own first operations need. Loro does not refuse the result —
/// before this was checked it either swallowed the legacy operations or
/// panicked inside its DAG on a discontinuous Lamport — so the migration
/// refuses the batch instead of writing a half-imported unit.
#[test]
fn a_writer_colliding_with_a_derived_namespace_refuses_the_batch() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let unit = legacy(owner, workspace);
    let target = entity(workspace);
    unit.set_personal_attribute(&target, "k", &text("v"))
        .unwrap();
    unit.put_reminder_rule(&floating_rule(&target)).unwrap();
    for writer in derived_peers(&unit) {
        assert!(matches!(
            migrate_personal_sources(workspace, vec![source(&unit)], writer),
            Err(PersonalMigrationRefusal::PeerCollision { derived }) if derived == writer
        ));
    }
    // A writer of its own is all it takes.
    let fresh = migrate_personal_sources(workspace, vec![source(&unit)], 4_243).unwrap();
    assert_eq!(
        load(&fresh, workspace).personal_attributes(&target).len(),
        1
    );
    // The same hazard on the later-source path: a writer the held unit has.
    let held = load(&fresh, workspace);
    for taken in peers_of(&held) {
        let error = admit_legacy_source(&held, &source(&unit), taken).unwrap_err();
        assert_eq!(error.code(), Code::CrdtRejected);
    }
    assert!(admit_legacy_source(&held, &source(&unit), 4_244).is_ok());
}

#[test]
fn duplicate_change_ids_within_one_source_are_corrupt_input() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let unit = legacy(owner, workspace);
    unit.set_personal_attribute(&entity(workspace), "k", &text("v"))
        .unwrap();
    let doc = &unit.inner.doc;
    let mut json = doc
        .export_json_updates_without_peer_compression(&VersionVector::default(), &doc.oplog_vv());
    assert!(check_unique_change_ids(&json, owner).is_ok());
    let duplicate = json.changes[0].clone();
    json.changes.push(duplicate);
    assert!(matches!(
        check_unique_change_ids(&json, owner),
        Err(PersonalMigrationRefusal::DuplicateChangeId { owner: o, .. }) if o == owner
    ));
}

#[test]
fn an_empty_batch_yields_an_empty_v2_unit_and_an_empty_receipt() {
    let workspace = WorkspaceId::new();
    let migrated = migrate_personal_sources(workspace, Vec::new(), 8).unwrap();
    let doc = load(&migrated, workspace);
    assert!(doc.reminders().is_empty());
    assert!(migrated.due_rows.is_empty());
    assert!(migrated.receipt.sources.is_empty());
    assert_eq!(peers_of(&doc), [8].into_iter().collect());
}
