// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn fixture_and_both_arrangements_are_bounded() {
    assert!(!Declaration::parse(FIXTURE.as_bytes()).unwrap().swapped);
    assert!(
        Declaration::parse(FIXTURE.replace("tasks-first", "document-first").as_bytes())
            .unwrap()
            .swapped
    );
    assert!(Declaration::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
}

#[test]
fn unsupported_programs_content_and_graphs_are_refused() {
    for fixture in [
        FIXTURE.replace("\"sendDataModel\": false", "\"sendDataModel\": true"),
        FIXTURE.replace(
            "\"bucket\": \"today\"",
            "\"bucket\": {\"functionCall\": \"read\"}",
        ),
        FIXTURE.replace(
            "\"component\": \"DocumentEditor\"",
            "\"component\": \"DocumentEditor\", \"text\": \"private content\"",
        ),
        FIXTURE.replace(
            "\"selector\", \"tasks\", \"search\", \"document\"",
            "\"root\"",
        ),
        FIXTURE.replace("DocumentSearch", "ArbitraryTemplate"),
    ] {
        assert!(Declaration::parse(fixture.as_bytes()).is_err());
    }
}

#[test]
fn typed_references_are_current_workspace_only_and_cannot_carry_content() {
    let list = koru_core::ids::ListId::new();
    let document = koru_core::DocumentId::new();
    let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let components = value["createSurface"]["components"].as_array_mut().unwrap();
    components[2].as_object_mut().unwrap().remove("bucket");
    components[2]["list"] = serde_json::json!({"workspace":"current","listId":list.to_storage()});
    components[4]["document"] =
        serde_json::json!({"workspace":"current","documentId":document.to_storage()});
    let parsed = Declaration::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(parsed.target, RailTarget::List(list));
    assert_eq!(parsed.document, Some(document));
    for field in ["list", "document"] {
        let mut other = value.clone();
        let index = if field == "list" { 2 } else { 4 };
        other["createSurface"]["components"][index][field]["workspace"] = "elsewhere".into();
        assert!(Declaration::parse(&serde_json::to_vec(&other).unwrap()).is_err());
    }
    value["createSurface"]["components"][4]["document"]["text"] = "content".into();
    assert!(Declaration::parse(&serde_json::to_vec(&value).unwrap()).is_err());
}
