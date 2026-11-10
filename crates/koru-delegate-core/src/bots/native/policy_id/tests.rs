// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn policy_reader_preserves_legacy_v4_and_accepts_only_canonical_v8_extension() {
    for text in [
        "00000000-0000-4000-8000-000000000001",
        "abcdef00-0000-8000-8000-000000000002",
    ] {
        let id: NativePolicyRequestId = text.parse().unwrap();
        assert_eq!(id.to_storage(), text);
        assert_eq!(
            serde_json::from_str::<NativePolicyRequestId>(&serde_json::to_string(&id).unwrap())
                .unwrap(),
            id
        );
    }
    for text in [
        "00000000-0000-0000-0000-000000000000",
        "00000000-0000-5000-8000-000000000001",
        "00000000-0000-8000-c000-000000000001",
        "ABCDEF00-0000-8000-8000-000000000002",
        "abcdef00000080008000000000000002",
    ] {
        assert!(text.parse::<NativePolicyRequestId>().is_err(), "{text}");
    }
    assert!(
        "abcdef00-0000-8000-8000-000000000002"
            .parse::<crate::BotRequestId>()
            .is_err()
    );
}
