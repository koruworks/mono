// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn an_id_round_trips_through_storage() {
    let id = WorkspaceId::new();
    assert_eq!(WorkspaceId::parse(&id.to_storage()), Some(id));
}

#[test]
fn two_ids_differ() {
    assert_ne!(ProfileId::new(), ProfileId::new());
}

#[test]
fn a_malformed_id_is_none_rather_than_a_panic() {
    assert_eq!(DocumentId::parse(""), None);
    assert_eq!(DocumentId::parse("not-a-uuid"), None);
    // A name is never an id. This is the invariant, spelled as a test.
    assert_eq!(DocumentId::parse("Mein Arbeitsbereich"), None);
    assert_eq!(DocumentId::parse("مساحة العمل"), None);
}

#[test]
fn storage_form_is_hyphenated_lowercase() {
    let s = WorkspaceId::new().to_storage();
    assert_eq!(s.len(), 36, "{s}");
    assert_eq!(s.matches('-').count(), 4, "{s}");
    assert_eq!(s, s.to_lowercase(), "{s}");
}

#[test]
fn debug_is_short_and_display_is_whole() {
    let id = ProfileId::new();
    let short = format!("{id:?}");
    assert!(short.starts_with("prof:"), "{short}");
    assert_eq!(short.len(), "prof:".len() + 8);
    assert_eq!(format!("{id}"), id.to_storage());
}

#[test]
fn serde_uses_the_storage_form() {
    let id = DocumentId::new();
    let json = serde_json::to_string(&id).expect("serialize");
    assert_eq!(json, format!("\"{}\"", id.to_storage()));
    let back: DocumentId = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, id);
}

#[test]
fn serde_rejects_a_non_uuid_rather_than_inventing_one() {
    assert!(serde_json::from_str::<WorkspaceId>("\"Espacio de trabajo\"").is_err());
}

#[test]
fn reminder_ids_require_canonical_lowercase_v4() {
    let valid = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    assert!(ReminderId::parse(valid).is_some());
    assert!(RuleRevisionId::parse(valid).is_some());
    assert!(ActionId::parse(valid).is_some());
    assert!(ReminderId::parse(&valid.to_uppercase()).is_none());
    assert!(ReminderId::parse("11111111-1111-1111-8111-111111111111").is_none());
    assert!(ReminderId::parse("00000000-0000-0000-0000-000000000000").is_none());
}

#[test]
fn catalog_ids_round_trip_in_canonical_lowercase_form() {
    fn check(storage: String, parse: fn(&str) -> Option<String>) {
        assert_eq!(storage.len(), 36, "{storage}");
        assert_eq!(storage, storage.to_lowercase(), "{storage}");
        assert_eq!(parse(&storage).as_deref(), Some(storage.as_str()));
        assert_eq!(parse("not-a-scope"), None);
        assert_eq!(parse("Mein Konto"), None);
    }
    check(UserScopeId::new().to_storage(), |s| {
        UserScopeId::parse(s).map(|id| id.to_storage())
    });
    check(DeletionId::new().to_storage(), |s| {
        DeletionId::parse(s).map(|id| id.to_storage())
    });
    check(RestoreId::new().to_storage(), |s| {
        RestoreId::parse(s).map(|id| id.to_storage())
    });
    let scope = UserScopeId::new();
    assert!(format!("{scope:?}").starts_with("scope:"));
    let json = serde_json::to_string(&scope).unwrap();
    assert_eq!(json, format!("\"{}\"", scope.to_storage()));
    assert_eq!(serde_json::from_str::<UserScopeId>(&json).unwrap(), scope);
}

/// **The reading-document id is a contract with the application.**
/// `koru_core::ids::reading_document_id` derives the same id on the other side
/// of the boundary and pins the same vector, computed independently of either
/// crate (Python's `uuid.uuid5` over the parent's sixteen bytes).
#[test]
fn a_reading_document_id_is_derived_from_its_parent_alone() {
    let parent = DocumentId::parse("6f1c2a3e-8b4d-4e5f-9a0b-1c2d3e4f5a6b").unwrap();
    assert_eq!(
        reading_document_id(parent).to_storage(),
        "3f0c7cc1-3b13-556a-b11f-d1454d28c21e"
    );
    assert_eq!(
        READING_DOCUMENT_NS_V1.to_string(),
        "05793a5c-5b98-4d2a-a3f2-4ac0b564ad87"
    );
    assert_ne!(
        reading_document_id(parent),
        reading_document_id(DocumentId::new())
    );
}
