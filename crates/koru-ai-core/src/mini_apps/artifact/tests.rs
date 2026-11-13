// SPDX-License-Identifier: MPL-2.0
use super::*;

fn fixture() -> Envelope {
    Envelope::capture(
        WorkspaceId::from_uuid(Uuid::new_v4()),
        &Declaration::parse(super::super::FIXTURE.as_bytes()).unwrap(),
    )
    .unwrap()
}
#[test]
fn saved_format_is_portable_and_contains_no_live_identity() {
    let mut original = fixture();
    original.definition.tasks = Tasks::List {
        parameter: ListParameter::TaskList,
    };
    original.definition.document = Some(DocumentParameter::Document);
    original.bindings.task_list = Some(Uuid::new_v4());
    original.bindings.document = Some(DocumentId::from_uuid(Uuid::new_v4()));
    let bytes = original.encode().unwrap();
    assert_eq!(Envelope::decode(&bytes).unwrap(), original);
    let json = String::from_utf8(bytes).unwrap();
    for forbidden in [
        "surfaceId",
        "renderer",
        "request",
        "components",
        "text",
        "title",
        "path",
        "credentials",
    ] {
        assert!(!json.contains(forbidden));
    }
    let mut rebound = original.clone();
    rebound.bindings = Bindings {
        workspace: WorkspaceId::from_uuid(Uuid::new_v4()),
        task_list: Some(Uuid::new_v4()),
        document: Some(DocumentId::from_uuid(Uuid::new_v4())),
    };
    assert_eq!(rebound.definition, original.definition);
    let a = original
        .instantiate(original.bindings.workspace, "fresh-first".into())
        .unwrap();
    let b = rebound
        .instantiate(rebound.bindings.workspace, "fresh-second".into())
        .unwrap();
    assert_ne!(a.selection, b.selection);
    assert_ne!(a.document, b.document);
    assert_ne!(a.envelope.surface.surface_id, b.envelope.surface.surface_id);
    assert!(
        original
            .instantiate(rebound.bindings.workspace, "wrong".into())
            .is_err()
    );
}
#[test]
fn unknown_versions_fields_catalogs_and_oversized_bodies_are_refused() {
    let original = fixture();
    let bytes = original.encode().unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["format_version"] = 2.into();
    assert!(Envelope::decode(&serde_json::to_vec(&json).unwrap()).is_err());
    json["format_version"] = 1.into();
    json["definition"]["content"] = "forbidden".into();
    assert!(Envelope::decode(&serde_json::to_vec(&json).unwrap()).is_err());
    assert_eq!(
        original.encode().unwrap(),
        bytes,
        "refusal never rewrites the saved value"
    );
    let mut other = original.clone();
    other.definition.catalog_id = "future".into();
    assert!(other.encode().is_err());
    assert!(Envelope::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
}
#[test]
fn bindings_must_match_declared_slots_and_cannot_be_nil() {
    let original = fixture();
    let mut other = original.clone();
    other.bindings.task_list = Some(Uuid::new_v4());
    assert!(other.validate().is_err());
    other = original.clone();
    other.bindings.document = Some(DocumentId::from_uuid(Uuid::new_v4()));
    assert!(other.validate().is_err());
    other = original;
    other.bindings.workspace = WorkspaceId::from_uuid(Uuid::nil());
    assert!(other.validate().is_err());
}
