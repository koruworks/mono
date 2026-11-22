// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn mini_app_pin_scopes_are_explicit_and_do_not_grant_writes_from_reads() {
    let mut principal = GatewayPrincipal {
        label: "fixture".into(),
        scopes: vec![SETTINGS_READ_SCOPE.into()],
    };
    assert!(!permits(&principal, SETTINGS_READ_SCOPE));
    principal.scopes.push("store.read_documents".into());
    assert!(permits(&principal, SETTINGS_READ_SCOPE));
    assert!(!permits(&principal, SETTINGS_WRITE_SCOPE));
    principal.scopes.push(SETTINGS_WRITE_SCOPE.into());
    assert!(permits(&principal, SETTINGS_WRITE_SCOPE));
    assert!(!principal.scopes.iter().any(|s| s == "artifacts.read"));
}
#[test]
fn pin_openapi_names_do_not_replace_existing_scope_schema() {
    let doc = crate::public_openapi();
    assert!(
        doc["components"]["schemas"]["Scope"]["properties"]
            .get("profile_id")
            .is_some()
    );
    assert!(
        doc["components"]["schemas"]["Scope"]["properties"]
            .get("profile")
            .is_none()
    );
    assert!(
        doc["components"]["schemas"]["MiniAppPinsScope"]["properties"]
            .get("profile")
            .is_some()
    );
    assert_eq!(
        doc["paths"]["/v1/mini-app-pins"]["get"]["security"][0]["bearerAuth"],
        serde_json::json!(["settings.read", "store.read_documents"])
    );
}
