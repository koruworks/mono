// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn decision_ids_keep_canonical_v4_and_existing_store_domains() {
    let s = uuid(0xab).to_string();
    assert_eq!(s.parse::<DecisionId>().unwrap().to_storage(), s);
    for bad in [
        s.to_uppercase(),
        "00000000-0000-1000-8000-000000000001".into(),
        "bad".into(),
    ] {
        assert!(bad.parse::<DecisionId>().is_err());
        assert!(bad.parse::<DecisionAnswerId>().is_err());
        assert!(bad.parse::<BotWaitId>().is_err());
        assert!(bad.parse::<DecisionTransitionId>().is_err());
    }
    let mut b = json(&binding());
    b["target"] = serde_json::json!("00000000-0000-1000-8000-000000000001");
    b["human_profile"] = serde_json::json!("00000000-0000-1000-8000-000000000002");
    assert!(decode::<DecisionBindingV1>(&b).is_ok());
}
#[test]
fn decision_revision_generation_and_version_refuse_zero_and_overflow() {
    for n in [0, u64::MAX] {
        assert!(DecisionRevision::new(n).is_err());
    }
    assert_eq!(rev(i64::MAX as u64).get(), i64::MAX as u64);
    for (key, value) in [
        ("version", 2),
        ("origin_generation", 0),
        ("payload_version", 0),
    ] {
        let mut b = json(&binding());
        b[key] = value.into();
        assert!(decode::<DecisionBindingV1>(&b).is_err());
    }
}
#[test]
fn decision_bounded_bytes_enforce_raw_and_encoded_domains() {
    let bytes = DecisionBytes::<4>::new(vec![255; 4]).unwrap();
    assert_eq!(serde_json::to_string(&bytes).unwrap(), "[255,255,255,255]");
    for s in ["[]", "[1,2,3,4,5]", "[256]", "[-1]", "[1.5]"] {
        assert!(serde_json::from_str::<DecisionBytes<4>>(s).is_err());
    }
    assert!(DecisionBytes::<4>::new(vec![0; 5]).is_err());
    assert!(serde_json::from_str::<DecisionList<u8, 2>>("[1,2,3]").is_err());
    assert_eq!(
        serde_json::from_str::<DecisionList<u8, 2>>("[1,2]")
            .unwrap()
            .as_slice(),
        [1, 2]
    );
}
#[test]
fn decision_complete_frame_limit_includes_json_envelope() {
    #[derive(Serialize, Deserialize)]
    struct Frame {
        value: String,
    }
    impl DecisionValidate for Frame {
        fn validate(&self) -> Result<(), DecisionError> {
            Ok(())
        }
    }
    let overhead = serde_json::to_vec(&Frame {
        value: String::new(),
    })
    .unwrap()
    .len();
    let mut v = Frame {
        value: "x".repeat(DECISION_FRAME_BYTES - overhead),
    };
    let bytes = encode_decision_frame(&v).unwrap();
    assert_eq!(bytes.len(), DECISION_FRAME_BYTES);
    assert!(decode_decision_frame::<Frame>(&bytes).is_ok());
    v.value.push('x');
    assert_eq!(encode_decision_frame(&v), Err(DecisionError::TooLarge));
    assert!(matches!(
        decode_decision_frame::<Frame>(&vec![b' '; DECISION_FRAME_BYTES + 1]),
        Err(DecisionError::TooLarge)
    ));
}
#[test]
fn decision_strict_empty_variants_and_nested_records_refuse_extras() {
    for s in [
        r#"{"choice":"allow_once","extra":1}"#,
        r#"{"choice":"deny","extra":1}"#,
    ] {
        assert!(serde_json::from_str::<DecisionAnswerChoice>(s).is_err());
    }
    assert!(
        serde_json::from_str::<OriginWaitStop>(r#"{"origin_stop":"pending","extra":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<ConsumerCleanup>(r#"{"cleanup":"not_requested","extra":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<BotWaitReason>(r#"{"reason":"capacity","extra":true}"#).is_err()
    );
    let mut b = json(&binding());
    b["global_policy"]["key"]["extra"] = true.into();
    assert!(decode::<DecisionBindingV1>(&b).is_err());
    let mut b = json(&binding());
    b["extra"] = true.into();
    assert!(decode::<DecisionBindingV1>(&b).is_err());
}
#[test]
fn decision_content_debug_and_errors_are_redacted() {
    let mut e = edit();
    e.payload = DecisionBytes::new(b"SECRET-PAYLOAD".to_vec()).unwrap();
    let debug = format!("{:?}", DecisionBindingV1::KoruEdit(e));
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("SECRET"));
    assert!(!debug.contains("83, 69"));
    let err = decode_decision_frame::<DecisionBindingV1>(br#"{"SECRET-PAYLOAD":1}"#).unwrap_err();
    assert_eq!(format!("{err}"), "invalid decision record");
}
#[test]
#[cfg(feature = "schema")]
fn decision_schema_reports_uuid_revision_and_byte_array_domains() {
    let ids = serde_json::to_value(schemars::schema_for!(DecisionId)).unwrap();
    assert!(ids["pattern"].as_str().unwrap().contains("-4"));
    let revision = serde_json::to_value(schemars::schema_for!(DecisionRevision)).unwrap();
    assert_eq!(revision["minimum"], 1);
    assert_eq!(revision["maximum"], i64::MAX);
    let bytes = serde_json::to_value(schemars::schema_for!(DecisionBytes<4>)).unwrap();
    assert_eq!(bytes["type"], "array");
    assert_eq!(bytes["maxItems"], 4);
    assert_eq!(bytes["items"]["maximum"], 255);
}
