// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::model::{Profile, Timestamp};
use crate::personal_attributes::{EntityRef, PersonalAttributeValue};
use crate::profile_manifest::ProfileManifest;
use crate::reminders::{LocalTime, ToneId, WeekStartPreference};
use crate::text::Locale;
use loro::ExportMode;

fn profile() -> Profile {
    Profile::new("Alice", Locale::undetermined(), Timestamp::from_millis(42)).unwrap()
}
fn entity(workspace: WorkspaceId) -> EntityRef {
    EntityRef::new(
        "task",
        workspace,
        crate::ids::DocumentId::new(),
        uuid::Uuid::new_v4(),
    )
    .unwrap()
}
/// The typed JSON history, exported *without* peer compression so the ids in
/// it are the real ones. The compressed export renumbers every id to an index
/// into its own `peers` table — a document written by peer 7 reports `0@0` —
/// so an assertion about who authored a change must read this form.
fn history(doc: &LoroDoc) -> serde_json::Value {
    serde_json::to_value(doc.export_json_updates_without_peer_compression(
        &loro::VersionVector::default(),
        &doc.oplog_vv(),
    ))
    .unwrap()
}

fn history_entities(value: &serde_json::Value) -> Vec<EntityRef> {
    match value {
        serde_json::Value::String(value) => EntityRef::parse(value).into_iter().collect(),
        serde_json::Value::Array(values) => values.iter().flat_map(history_entities).collect(),
        serde_json::Value::Object(values) => values.values().flat_map(history_entities).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn all_unit_addresses_round_trip_and_never_default() {
    let profile = ProfileId::new();
    for unit in [
        ProfileUnitRef::LegacyPrivate { profile },
        ProfileUnitRef::Identity { profile },
        ProfileUnitRef::Personal {
            owner: profile,
            workspace: WorkspaceId::new(),
        },
        ProfileUnitRef::Private { profile },
    ] {
        let value = serde_json::to_value(unit).unwrap();
        assert!(value.get("type").is_some());
        assert_eq!(
            serde_json::from_value::<ProfileUnitRef>(value).unwrap(),
            unit
        );
    }
    for value in [
        serde_json::json!({"profile":profile}),
        serde_json::json!({"type":"unknown","profile":profile}),
        serde_json::json!({"type":"private","profile":profile,"workspace":WorkspaceId::new()}),
    ] {
        assert!(serde_json::from_value::<ProfileUnitRef>(value).is_err());
    }
}

#[test]
fn fresh_identity_preserves_values_without_deleted_legacy_operations() {
    let mut profile = profile();
    profile.kind = crate::model::ProfileKind::Holder;
    profile.account = Some(format!("koru:{}", uuid::Uuid::new_v4()));
    profile.icon = Some("tree".into());
    let legacy = ProfileManifest::create(&profile).unwrap();
    legacy.set_peer_id(11).unwrap();
    legacy
        .add_dictionary_word("en", "private_history_marker")
        .unwrap();
    legacy
        .remove_dictionary_word("en", "private_history_marker")
        .unwrap();
    legacy
        .update_reminder_settings(
            Some(Some(LocalTime::parse("19:00:00").unwrap())),
            None,
            None,
        )
        .unwrap();
    legacy
        .update_reminder_settings(Some(None), None, None)
        .unwrap();
    let unrelated = entity(WorkspaceId::new());
    legacy
        .set_personal_attribute(
            &unrelated,
            "private",
            &PersonalAttributeValue::Text {
                text: "hidden_history".into(),
            },
        )
        .unwrap();
    legacy
        .clear_personal_attribute(&unrelated, "private")
        .unwrap();
    let identity = IdentityManifest::create(&legacy.profile().unwrap()).unwrap();
    let private = PrivateManifest::create(profile.id).unwrap();
    let personal = PersonalManifest::create(profile.id, WorkspaceId::new()).unwrap();
    let authorized = entity(personal.workspace());
    personal
        .set_personal_attribute(
            &authorized,
            "note",
            &PersonalAttributeValue::Text {
                text: "new shared workspace fact".into(),
            },
        )
        .unwrap();
    let restored = IdentityManifest::from_snapshot(&identity.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(restored.profile().unwrap()).unwrap(),
        serde_json::to_value(profile).unwrap()
    );
    let legacy_history = history(legacy.doc()).to_string();
    for doc in [&identity.inner.doc, &private.inner.doc] {
        let decoded = history(doc);
        let ops: Vec<_> = decoded["changes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|change| change["ops"].as_array().unwrap())
            .collect();
        assert!(!ops.is_empty());
        assert!(ops.iter().all(|op| op["container"] == "cid:root-meta:Map"));
        assert!(history_entities(&decoded).is_empty());
    }
    let personal_entities = history_entities(&history(&personal.inner.doc));
    assert!(personal_entities.contains(&authorized));
    assert!(
        personal_entities
            .iter()
            .all(|entity| entity.workspace() == personal.workspace())
    );
    assert!(legacy_history.contains("private_history_marker"));
    assert!(legacy_history.contains(&unrelated.to_storage()));
    for doc in [&identity.inner.doc, &private.inner.doc, &personal.inner.doc] {
        let decoded = history(doc).to_string();
        assert!(!decoded.contains("private_history_marker"));
        assert!(!decoded.contains("hidden_history"));
        assert!(!decoded.contains(&unrelated.to_storage()));
        assert!(!doc.oplog_vv().includes_vv(&legacy.version()));
    }
    assert_eq!(private.reminder_settings(), Default::default());
    assert!(private.dictionary().is_empty());
    assert!(personal.reminders().is_empty());
}

#[test]
fn private_concurrent_first_maps_merge_and_removal_survives_restart() {
    let mut a = PrivateManifest::create(ProfileId::new()).unwrap();
    a.set_peer_id(1).unwrap();
    let mut b = PrivateManifest::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    b.set_peer_id(2).unwrap();
    a.add_dictionary_word("en", "alpha").unwrap();
    b.add_dictionary_word("en", "beta").unwrap();
    a.update_reminder_settings(None, Some(ToneId::Soft), None)
        .unwrap();
    b.update_reminder_settings(None, None, Some(WeekStartPreference::Monday))
        .unwrap();
    let a_updates = a.export_updates_since(&b.version()).unwrap();
    a.import_frames(&[b.export_updates_since(&a.version()).unwrap()])
        .unwrap();
    b.import_frames(&[a_updates]).unwrap();
    assert_eq!(a.dictionary(), b.dictionary());
    assert_eq!(a.dictionary()["en"], vec!["alpha", "beta"]);
    assert_eq!(a.reminder_settings(), b.reminder_settings());
    assert_eq!(a.reminder_settings().default_tone, ToneId::Soft);
    assert_eq!(
        a.reminder_settings().week_start,
        WeekStartPreference::Monday
    );
    a.remove_dictionary_word("en", "alpha").unwrap();
    a.update_reminder_settings(None, Some(ToneId::System), None)
        .unwrap();
    b.import_frames(&[a.export_updates_since(&b.version()).unwrap()])
        .unwrap();
    let b = PrivateManifest::from_snapshot(&b.export_snapshot().unwrap()).unwrap();
    assert_eq!(b.dictionary()["en"], vec!["beta"]);
    assert_eq!(b.reminder_settings().default_tone, ToneId::System);
}

#[test]
fn clearing_private_dictionary_deletes_known_words_and_keeps_a_concurrent_addition() {
    let mut clearing = PrivateManifest::create(ProfileId::new()).unwrap();
    clearing.set_peer_id(1).unwrap();
    clearing.add_dictionary_word("en-US", "alpha").unwrap();
    clearing.add_dictionary_word("de", "beta").unwrap();
    let mut adding = PrivateManifest::from_snapshot(&clearing.export_snapshot().unwrap()).unwrap();
    adding.set_peer_id(2).unwrap();

    assert_eq!(clearing.clear_dictionary().unwrap(), 2);
    adding.add_dictionary_word("en-US", "gamma").unwrap();
    let clearing_delta = clearing.export_updates_since(&adding.version()).unwrap();
    let adding_delta = adding.export_updates_since(&clearing.version()).unwrap();
    clearing.import_frames(&[adding_delta]).unwrap();
    adding.import_frames(&[clearing_delta]).unwrap();

    assert_eq!(clearing.dictionary(), adding.dictionary());
    assert_eq!(clearing.dictionary()["en-US"], vec!["gamma"]);
    let before = clearing.version();
    assert_eq!(clearing.clear_dictionary().unwrap(), 1);
    let after = clearing.version();
    assert_eq!(clearing.clear_dictionary().unwrap(), 0);
    assert_eq!(clearing.version(), after);
    assert_ne!(before, after);
}

#[test]
fn scoped_personal_rejects_wrong_workspace_before_mutation() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let personal = PersonalManifest::create(owner, workspace).unwrap();
    let before = personal.version();
    assert_eq!(
        personal
            .set_personal_attribute(
                &entity(WorkspaceId::new()),
                "note",
                &PersonalAttributeValue::Text {
                    text: "secret".into()
                }
            )
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch
    );
    assert_eq!(personal.version(), before);
    let target = entity(workspace);
    personal
        .set_personal_attribute(
            &target,
            "note",
            &PersonalAttributeValue::Text {
                text: "Alice".into(),
            },
        )
        .unwrap();
    let bob = PersonalManifest::create(ProfileId::new(), workspace).unwrap();
    bob.set_personal_attribute(
        &target,
        "note",
        &PersonalAttributeValue::Text { text: "Bob".into() },
    )
    .unwrap();
    assert_ne!(
        personal.personal_attributes(&target),
        bob.personal_attributes(&target)
    );
    assert_eq!(
        personal
            .staged_import(&[bob.export_snapshot().unwrap()])
            .err()
            .unwrap()
            .code,
        Code::ProfileUnitScopeMismatch
    );
}

#[test]
fn rejected_import_leaves_original_snapshot_and_version_unchanged() {
    let mut private = PrivateManifest::create(ProfileId::new()).unwrap();
    let bad = LoroDoc::new();
    bad.import(&private.export_snapshot().unwrap()).unwrap();
    bad.get_map("reminders")
        .ensure_mergeable_map("rules")
        .unwrap()
        .insert("forbidden", true)
        .unwrap();
    bad.commit();
    let before = private.version();
    assert!(
        private
            .import_frames(&[bad.export(ExportMode::snapshot()).unwrap()])
            .is_err()
    );
    assert_eq!(private.version(), before);
    assert!(private.dictionary().is_empty());
    assert!(PrivateManifest::from_snapshot(&private.export_snapshot().unwrap()).is_ok());
}

#[test]
fn foreign_snapshot_cannot_hide_scope_by_losing_metadata_conflict() {
    let mut own = PrivateManifest::create(ProfileId::new()).unwrap();
    let foreign = PrivateManifest::create(ProfileId::new()).unwrap();
    foreign.add_dictionary_word("en", "foreign_secret").unwrap();
    // Force our scope register's Lamport timestamp above the foreign snapshot;
    // a check of merged live metadata alone would miss the foreign content.
    for _ in 0..30 {
        own.inner
            .doc
            .get_map("meta")
            .insert("profile_id", ProfileId::new().to_storage())
            .unwrap();
        own.inner.doc.commit();
    }
    own.inner
        .doc
        .get_map("meta")
        .insert("profile_id", own.profile_id().to_storage())
        .unwrap();
    own.inner.doc.commit();
    let unsafe_merge = LoroDoc::new();
    unsafe_merge
        .import(&own.export_snapshot().unwrap())
        .unwrap();
    unsafe_merge
        .import(&foreign.export_snapshot().unwrap())
        .unwrap();
    assert_eq!(
        string(&unsafe_merge.get_map("meta"), "profile_id"),
        Some(own.profile_id().to_storage())
    );
    assert!(
        !super::content::ProfileContent { doc: &unsafe_merge }
            .dictionary()
            .is_empty()
    );
    assert_eq!(
        own.import_frames(&[foreign.export_snapshot().unwrap()])
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch
    );
    assert!(own.dictionary().is_empty());
}

#[test]
fn missing_ancestors_are_rejected_without_false_inclusion() {
    let mut target = PrivateManifest::create(ProfileId::new()).unwrap();
    let source = PrivateManifest::from_snapshot(&target.export_snapshot().unwrap()).unwrap();
    source.set_peer_id(123).unwrap();
    source.add_dictionary_word("en", "first").unwrap();
    let middle = source.version();
    source.add_dictionary_word("en", "second").unwrap();
    let before = target.version();
    assert_eq!(
        target
            .import_frames(&[source.export_updates_since(&middle).unwrap()])
            .unwrap_err()
            .code,
        Code::CrdtRejected
    );
    assert_eq!(target.version(), before);
    assert!(!target.version().includes_vv(&source.version()));
}

#[test]
fn future_format_is_retained_but_cannot_write_or_export_updates() {
    let private = PrivateManifest::create(ProfileId::new()).unwrap();
    private
        .inner
        .doc
        .get_map("meta")
        .insert("schema_version", 2)
        .unwrap();
    private
        .inner
        .doc
        .get_map("future_private")
        .insert("field", true)
        .unwrap();
    private.inner.doc.commit();
    let mut future = PrivateManifest::from_snapshot(&private.export_snapshot().unwrap()).unwrap();
    assert!(future.is_read_only());
    assert_eq!(
        future.add_dictionary_word("en", "word").unwrap_err().code,
        Code::SchemaTooNew
    );
    assert_eq!(
        future
            .export_updates_since(&Default::default())
            .unwrap_err()
            .code,
        Code::SchemaTooNew
    );
    assert_eq!(
        future.import_frames(&[]).unwrap_err().code,
        Code::SchemaTooNew
    );
    assert!(
        PrivateManifest::from_snapshot(&future.export_snapshot().unwrap())
            .unwrap()
            .is_read_only()
    );
}

#[test]
fn absent_schema_and_foreign_root_are_corruption_not_empty_unit() {
    let private = PrivateManifest::create(ProfileId::new()).unwrap();
    private
        .inner
        .doc
        .get_map("meta")
        .delete("schema_version")
        .unwrap();
    private.inner.doc.commit();
    assert!(PrivateManifest::from_snapshot(&private.export_snapshot().unwrap()).is_err());
    let identity = IdentityManifest::create(&profile()).unwrap();
    identity
        .inner
        .doc
        .get_map("personal_dictionary")
        .insert("leak", true)
        .unwrap();
    identity.inner.doc.commit();
    assert!(IdentityManifest::from_snapshot(&identity.export_snapshot().unwrap()).is_err());
}

#[test]
fn identity_import_refuses_another_unit_but_merges_a_rival_lineage() {
    // A rewritten `profile_id` names a different unit and is still refused:
    // accepting it would file one Profile's content under another's id.
    let mut identity = IdentityManifest::create(&profile()).unwrap();
    let other = LoroDoc::new();
    other.import(&identity.export_snapshot().unwrap()).unwrap();
    other
        .get_map("meta")
        .insert("profile_id", ProfileId::new().to_storage())
        .unwrap();
    other.commit();
    let before = identity.version();
    let refused = identity
        .import_frames(&[other.export(ExportMode::snapshot()).unwrap()])
        .unwrap_err();
    assert_eq!(refused.code(), Code::ProfileUnitScopeMismatch);
    assert_eq!(identity.version(), before);

    // `created_at` and `kind` name *this* unit and disagree about a field that
    // cannot change, which means a second lineage exists. That is reported and
    // merged, never refused: refusing is permanent and self-sealing, and one
    // refused unit ends the whole dataset pass, freezing every unit beside it.
    // Merging costs a label a person can retype and converges everywhere.
    for key in ["kind", "created_at"] {
        let subject = profile();
        let mut identity = IdentityManifest::create(&subject).unwrap();
        let rival = LoroDoc::new();
        rival.import(&identity.export_snapshot().unwrap()).unwrap();
        let meta = rival.get_map("meta");
        match key {
            "kind" => meta.insert(key, "holder").unwrap(),
            _ => meta.insert(key, 500_i64).unwrap(),
        }
        rival.commit();
        let before = identity.version();
        identity
            .import_frames(&[rival.export(ExportMode::snapshot()).unwrap()])
            .unwrap_or_else(|e| panic!("{key} must merge, not refuse: {e}"));
        assert_ne!(identity.version(), before, "{key} merged nothing");
        // Still a valid, readable identity afterwards. That is the whole point:
        // the pass continues and the units beside this one keep flowing.
        let merged = IdentityManifest::from_snapshot(&identity.export_snapshot().unwrap())
            .unwrap_or_else(|e| panic!("{key} left an unreadable identity: {e}"));
        assert_eq!(merged.profile().unwrap().id, subject.id, "{key}");
    }
    let identity = IdentityManifest::create(&profile()).unwrap();
    identity.set_appearance(Some("tree"), None).unwrap();
    identity.set_appearance(None, None).unwrap();
    assert!(identity.inner.doc.get_map("meta").get("icon").is_none());
    identity
        .inner
        .doc
        .get_map("meta")
        .insert("avatar", LoroValue::Null)
        .unwrap();
    identity.inner.doc.commit();
    assert!(IdentityManifest::from_snapshot(&identity.export_snapshot().unwrap()).is_err());
}

#[test]
fn concurrent_same_register_and_word_add_remove_reuse_legacy_resolution() {
    let profile = profile();
    let legacy_a = ProfileManifest::create(&profile).unwrap();
    legacy_a.add_dictionary_word("en", "word").unwrap();
    let legacy_b = ProfileManifest::from_snapshot(&legacy_a.export_snapshot().unwrap()).unwrap();
    let mut private_a = PrivateManifest::create(profile.id).unwrap();
    private_a.add_dictionary_word("en", "word").unwrap();
    let mut private_b =
        PrivateManifest::from_snapshot(&private_a.export_snapshot().unwrap()).unwrap();
    legacy_a.set_peer_id(1).unwrap();
    legacy_b.set_peer_id(2).unwrap();
    private_a.set_peer_id(1).unwrap();
    private_b.set_peer_id(2).unwrap();
    legacy_a
        .update_reminder_settings(None, Some(ToneId::Soft), None)
        .unwrap();
    legacy_b
        .update_reminder_settings(None, Some(ToneId::Chime), None)
        .unwrap();
    private_a
        .update_reminder_settings(None, Some(ToneId::Soft), None)
        .unwrap();
    private_b
        .update_reminder_settings(None, Some(ToneId::Chime), None)
        .unwrap();
    legacy_a.add_dictionary_word("en", "word").unwrap();
    legacy_b.remove_dictionary_word("en", "word").unwrap();
    private_a.add_dictionary_word("en", "word").unwrap();
    private_b.remove_dictionary_word("en", "word").unwrap();
    let legacy_delta = legacy_a.export_updates_since(&legacy_b.version()).unwrap();
    legacy_a
        .import_frames(&[legacy_b.export_updates_since(&legacy_a.version()).unwrap()])
        .unwrap();
    legacy_b.import_frames(&[legacy_delta]).unwrap();
    let private_delta = private_a
        .export_updates_since(&private_b.version())
        .unwrap();
    private_a
        .import_frames(&[private_b
            .export_updates_since(&private_a.version())
            .unwrap()])
        .unwrap();
    private_b.import_frames(&[private_delta]).unwrap();
    assert_eq!(private_a.dictionary(), private_b.dictionary());
    assert_eq!(private_a.dictionary(), legacy_a.dictionary());
    assert_eq!(private_a.reminder_settings(), private_b.reminder_settings());
    assert_eq!(private_a.reminder_settings(), legacy_a.reminder_settings());
}

// ------------------------------------------------------------ personal v2

#[test]
fn personal_v2_header_names_the_workspace_only_and_loads_under_its_address() {
    let workspace = WorkspaceId::new();
    let personal = PersonalManifest::create_v2(workspace, 7).unwrap();
    assert_eq!(personal.inner.doc.peer_id(), 7);
    assert_eq!(personal.workspace(), workspace);
    assert_eq!(personal.owner(), None);
    assert_eq!(personal.unit(), None);
    assert_eq!(
        personal.scope(),
        PersonalScope::Workspace(PersonalUnitRef { workspace })
    );
    let meta = personal.inner.doc.get_map("meta");
    assert_eq!(
        integer(&meta, "schema_version"),
        Some(PERSONAL_V2_CRDT_SCHEMA_VERSION)
    );
    assert_eq!(string(&meta, "workspace_id"), Some(workspace.to_storage()));
    assert!(meta.get("owner_profile_id").is_none());
    assert!(meta.get("user_scope_id").is_none());
    // `create_v2` takes the writer before it writes anything, so the header
    // itself is authored by it: nothing of this unit carries the throwaway
    // peer the empty document started with.
    let decoded = history(&personal.inner.doc);
    let changes = decoded["changes"].as_array().unwrap();
    assert!(!changes.is_empty());
    for change in changes {
        assert!(change["id"].as_str().unwrap().ends_with("@7"));
    }

    let target = entity(workspace);
    personal
        .set_personal_attribute(
            &target,
            "note",
            &PersonalAttributeValue::Text {
                text: "follows the workspace".into(),
            },
        )
        .unwrap();
    let bytes = personal.export_snapshot().unwrap();
    let reloaded = PersonalManifest::from_snapshot_v2(&bytes, workspace).unwrap();
    assert_eq!(
        reloaded.personal_attributes(&target),
        personal.personal_attributes(&target)
    );
    assert!(!reloaded.is_read_only());
    assert_eq!(
        PersonalManifest::from_snapshot_v2(&bytes, WorkspaceId::new())
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch
    );
    // The v1 loader stays strict: no owner in the header is not a v1 unit.
    assert!(PersonalManifest::from_snapshot(&bytes).is_err());
    // Content scope is unchanged: an entity of another Workspace is refused.
    assert_eq!(
        reloaded
            .set_personal_attribute(
                &entity(WorkspaceId::new()),
                "note",
                &PersonalAttributeValue::Text {
                    text: "elsewhere".into()
                }
            )
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch
    );
}

#[test]
fn personal_v1_source_is_refused_under_the_v2_address_until_migrated() {
    let owner = ProfileId::new();
    let workspace = WorkspaceId::new();
    let legacy = PersonalManifest::create(owner, workspace).unwrap();
    let bytes = legacy.export_snapshot().unwrap();
    // Strict v1 still works and still names its owner.
    let loaded = PersonalManifest::from_snapshot(&bytes).unwrap();
    assert_eq!(loaded.owner(), Some(owner));
    assert_eq!(
        loaded.unit(),
        Some(ProfileUnitRef::Personal { owner, workspace })
    );
    assert_eq!(
        PersonalManifest::from_snapshot_v2(&bytes, workspace)
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch,
        "a relabel is not a migration"
    );
    // A v2 unit given an owner key does not become a writable v1 unit.
    let v2 = PersonalManifest::create_v2(workspace, 1).unwrap();
    v2.inner
        .doc
        .get_map("meta")
        .insert("owner_profile_id", owner.to_storage())
        .unwrap();
    v2.inner.doc.commit();
    let bytes = v2.export_snapshot().unwrap();
    assert!(
        PersonalManifest::from_snapshot(&bytes)
            .unwrap()
            .is_read_only(),
        "schema 2 is newer than the v1 contract understands"
    );
}

#[test]
fn personal_v2_ignores_a_legacy_owner_field_and_refuses_a_mistyped_one() {
    let workspace = WorkspaceId::new();
    let personal = PersonalManifest::create_v2(workspace, 1).unwrap();
    let meta = personal.inner.doc.get_map("meta");
    for legacy in [
        LoroValue::from(ProfileId::new().to_storage()),
        LoroValue::from("not even an id"),
        LoroValue::Null,
    ] {
        meta.insert("owner_profile_id", legacy).unwrap();
        personal.inner.doc.commit();
        let loaded =
            PersonalManifest::from_snapshot_v2(&personal.export_snapshot().unwrap(), workspace)
                .unwrap();
        assert_eq!(loaded.owner(), None, "history, not ownership");
        assert!(!loaded.is_read_only());
    }
    meta.insert("owner_profile_id", true).unwrap();
    personal.inner.doc.commit();
    assert_eq!(
        PersonalManifest::from_snapshot_v2(&personal.export_snapshot().unwrap(), workspace)
            .unwrap_err()
            .code,
        Code::CorruptState
    );
    meta.delete("owner_profile_id").unwrap();
    meta.insert("user_scope_id", "leak").unwrap();
    personal.inner.doc.commit();
    assert_eq!(
        PersonalManifest::from_snapshot_v2(&personal.export_snapshot().unwrap(), workspace)
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch,
        "the scope is never a header key"
    );
    meta.delete("user_scope_id").unwrap();
    personal
        .inner
        .doc
        .get_map("placement")
        .insert("current", LoroValue::Null)
        .unwrap();
    personal.inner.doc.commit();
    assert_eq!(
        PersonalManifest::from_snapshot_v2(&personal.export_snapshot().unwrap(), workspace)
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch,
        "only the three named roots"
    );
}

#[test]
fn personal_v2_newer_format_opens_read_only() {
    let workspace = WorkspaceId::new();
    let personal = PersonalManifest::create_v2(workspace, 1).unwrap();
    personal
        .inner
        .doc
        .get_map("meta")
        .insert("schema_version", PERSONAL_V2_CRDT_SCHEMA_VERSION + 1)
        .unwrap();
    personal
        .inner
        .doc
        .get_map("future_personal")
        .insert("field", true)
        .unwrap();
    personal.inner.doc.commit();
    let future =
        PersonalManifest::from_snapshot_v2(&personal.export_snapshot().unwrap(), workspace)
            .unwrap();
    assert!(future.is_read_only());
    assert_eq!(
        future.compatibility(),
        Compatibility::TooNew {
            found: PERSONAL_V2_CRDT_SCHEMA_VERSION + 1,
            understood: PERSONAL_V2_CRDT_SCHEMA_VERSION
        }
    );
    assert_eq!(
        future
            .set_personal_attribute(
                &entity(workspace),
                "note",
                &PersonalAttributeValue::Text { text: "x".into() }
            )
            .unwrap_err()
            .code,
        Code::SchemaTooNew
    );
    assert!(future.reminders().is_empty());
}

#[test]
fn personal_v2_replicas_merge_content_and_refuse_frames_of_another_workspace() {
    let workspace = WorkspaceId::new();
    let mut a = PersonalManifest::create_v2(workspace, 1).unwrap();
    let mut b =
        PersonalManifest::from_snapshot_v2(&a.export_snapshot().unwrap(), workspace).unwrap();
    b.set_peer_id(2).unwrap();
    let target = entity(workspace);
    a.set_personal_attribute(
        &target,
        "plan",
        &PersonalAttributeValue::Text { text: "a".into() },
    )
    .unwrap();
    b.set_personal_attribute(
        &target,
        "mood",
        &PersonalAttributeValue::Text { text: "b".into() },
    )
    .unwrap();
    let from_a = a.export_updates_since(&b.version()).unwrap();
    a.import_frames(&[b.export_updates_since(&a.version()).unwrap()])
        .unwrap();
    b.import_frames(&[from_a]).unwrap();
    assert_eq!(
        a.personal_attributes(&target),
        b.personal_attributes(&target)
    );
    assert_eq!(a.personal_attributes(&target).len(), 2);
    let foreign = PersonalManifest::create_v2(WorkspaceId::new(), 3).unwrap();
    assert_eq!(
        a.import_frames(&[foreign.export_snapshot().unwrap()])
            .unwrap_err()
            .code,
        Code::ProfileUnitScopeMismatch
    );
}
