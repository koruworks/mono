// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn credentials_validate_roundtrip_and_redact() {
    let secret = "run-authority-test-secret-with-enough-entropy".to_owned();
    let value = RunAuthorityCredential::new(secret.clone()).unwrap();
    let encoded = serde_json::to_string(&value).unwrap();
    let decoded: RunAuthorityCredential = serde_json::from_str(&encoded).unwrap();
    assert!(value.matches(&decoded));
    assert!(!format!("{value:?}").contains(&secret));
    assert!(!value.matches(&RunAuthorityCredential::new("x".repeat(32)).unwrap()));
    assert!(RunAuthorityCredential::new("short".into()).is_none());
    assert!(RunAuthorityCredential::new("x".repeat(257)).is_none());
    assert!(serde_json::from_str::<RunAuthorityCredential>("\"short\"").is_err());
}
