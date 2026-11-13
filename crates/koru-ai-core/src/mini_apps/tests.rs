// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn strict_catalog_accepts_order_independent_reference_only_components() {
    let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
    value["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert!(!Declaration::from_value(&value).unwrap().swapped);
    let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
    value["createSurface"]["components"][0]["arrangement"] = "document-first".into();
    assert!(Declaration::from_value(&value).unwrap().swapped);
    assert!(Declaration::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    for path in ["dataModel", "metadata", "updateComponents"] {
        let mut invalid = value.clone();
        invalid["createSurface"][path] = json!({});
        assert!(Declaration::from_value(&invalid).is_err());
    }
    for field in ["text", "rows", "title", "style", "@catalogId", "template"] {
        let mut invalid = value.clone();
        invalid["createSurface"]["components"][2][field] = "content".into();
        assert!(Declaration::from_value(&invalid).is_err());
    }
    value["createSurface"]["components"][4]["id"] = "tasks".into();
    assert!(Declaration::from_value(&value).is_err());
}
#[test]
fn authority_allows_one_success_and_revokes_in_flight_jobs() {
    let host = HostContext {
        renderer_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        workspace_id: WorkspaceId::new(),
    };
    let authority = Authority::new(host);
    assert!(authority.begin());
    assert!(!authority.begin());
    assert!(authority.finish(false));
    assert!(authority.begin());
    assert!(authority.finish(true));
    assert!(!authority.begin());
    let other = Authority::new(host);
    assert!(other.begin());
    other.clone().revoke();
    assert!(!other.finish(true));
    assert!(!other.live());
}
