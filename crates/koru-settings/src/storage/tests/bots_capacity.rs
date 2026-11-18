// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn capacity_is_device_local_positive_nullable_and_preserves_unknown_settings() {
    let mut settings = GlobalSettings::default();
    let file = apply_desktop(
        br#"{"future":true,"bots_session_capacity":3}"#,
        &mut settings,
    );
    assert_eq!(settings.bots_session_capacity, Some(3));
    let patch: crate::GlobalPatch = serde_json::from_str(r#"{"bots_session_capacity":0}"#).unwrap();
    assert!(patch.validate().is_err());
    let patch: crate::GlobalPatch =
        serde_json::from_str(r#"{"bots_session_capacity":null}"#).unwrap();
    assert!(patch.touches_device() && !patch.touches_roaming());
    patch.apply(&mut settings);
    assert_eq!(settings.bots_session_capacity, None);
    let bytes = render_desktop(&settings, &file);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["future"], true);
    assert!(value["bots_session_capacity"].is_null());
}
