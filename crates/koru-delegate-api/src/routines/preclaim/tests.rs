// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;
use serde_json::{Value, json};

fn id(n: u8) -> String {
    format!("abcdef00-0000-4000-8000-0000000000{n:02x}")
}
fn request() -> RoutineBridgeEnvelope<SynchronizeRoutinePreclaim> {
    RoutineBridgeEnvelope {
        version: 1,
        request_id: id(10).parse().unwrap(),
        payload: SynchronizeRoutinePreclaim {
            slot: serde_json::from_value(json!({
                "owner":{"user_scope":id(1),"device":id(2)},
                "bot":id(3),"schedule":id(4),"occurrence_ms":60_000
            }))
            .unwrap(),
            semantic_hash: RoutineDigest32::from_bytes([1; 32]),
            preparation_hash: RoutineDigest32::from_bytes([2; 32]),
        },
    }
}
fn ready() -> RoutineBridgeEnvelope<SynchronizeRoutinePreclaimReply> {
    let request = request();
    let slot = request.payload.slot;
    let policy =
        json!({"workspace":id(5),"destination":id(5),"access":"default","selected_pages":[id(6)]});
    RoutineBridgeEnvelope::reply_to(
        &request,
        SynchronizeRoutinePreclaimReply::Ready {
            slot,
            semantic_hash: request.payload.semantic_hash,
            preparation_hash: request.payload.preparation_hash,
            admission: BotAdmissionBarrier {
                owner: slot.owner,
                bot: slot.bot,
                request: id(7).parse().unwrap(),
                epoch: 3,
                paused: false,
            },
            proof: serde_json::from_value(json!({
                "global":{"key":{"owner":slot.owner,"bot":slot.bot,"request":id(8)},
                    "config_revision":3,"policy_revision":2,"policy":policy,"effects_allowed":true},
                "routine":{"owner":slot.owner,"bot":slot.bot,"schedule":slot.schedule,
                    "request":id(9),"grant_revision":1,"policy":policy,"effects_allowed":true}
            }))
            .unwrap(),
        },
    )
    .unwrap()
}
fn invalid(value: &Value) {
    assert_eq!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(
            &serde_json::to_vec(value).unwrap()
        ),
        Err(RoutineBridgeCodecError::Invalid),
    );
}

#[test]
fn preclaim_codec_roundtrips_ready_refusal_and_exact_route_with_redacted_debug() {
    assert_eq!(
        ROUTINE_PRECLAIM_ROUTE,
        "/internal/bots/routines/v1/synchronize-preclaim"
    );
    let request = request();
    assert_eq!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(&request.encode().unwrap())
            .unwrap(),
        request
    );
    let ready = ready();
    assert_eq!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(&ready.encode().unwrap())
            .unwrap(),
        ready
    );
    assert!(ready.validate_reply_to(&request).is_ok());
    let mut deterministic = ready.clone();
    let SynchronizeRoutinePreclaimReply::Ready { proof, .. } = &mut deterministic.payload else {
        unreachable!()
    };
    proof.global.key.request = "abcdef00-0000-8000-8000-000000000008".parse().unwrap();
    assert_eq!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(
            &deterministic.encode().unwrap()
        )
        .unwrap(),
        deterministic,
    );
    for code in [
        RoutineBridgeRefusal::Stale,
        RoutineBridgeRefusal::Unavailable,
        RoutineBridgeRefusal::Capacity,
    ] {
        let reply = RoutineBridgeEnvelope::reply_to(
            &request,
            SynchronizeRoutinePreclaimReply::Refused { code },
        )
        .unwrap();
        assert_eq!(
            RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(
                &reply.encode().unwrap()
            )
            .unwrap(),
            reply
        );
    }
    assert_eq!(
        format!("{:?}", request.payload),
        "SynchronizeRoutinePreclaim([REDACTED])"
    );
    assert_eq!(
        format!("{:?}", ready.payload),
        "SynchronizeRoutinePreclaimReply([REDACTED])"
    );
    let text = format!("{ready:?}");
    for secret in [id(1), id(5), id(6), "01".repeat(32), "02".repeat(32)] {
        assert!(!text.contains(&secret));
    }
}

#[test]
fn preclaim_codec_rejects_unknown_nested_fields_missing_fields_and_nonready_states() {
    let original = serde_json::to_value(ready()).unwrap();
    for path in [
        "",
        "/payload",
        "/payload/slot",
        "/payload/slot/owner",
        "/payload/admission",
        "/payload/admission/owner",
        "/payload/proof",
        "/payload/proof/global",
        "/payload/proof/global/key",
        "/payload/proof/global/key/owner",
        "/payload/proof/global/policy",
        "/payload/proof/routine",
        "/payload/proof/routine/owner",
        "/payload/proof/routine/policy",
    ] {
        let mut value = original.clone();
        value
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), true.into());
        invalid(&value);
    }
    for field in [
        "slot",
        "semantic_hash",
        "preparation_hash",
        "admission",
        "proof",
    ] {
        let mut value = original.clone();
        value["payload"].as_object_mut().unwrap().remove(field);
        invalid(&value);
    }
    for state in ["linked", "closed", "missing", "pending", "effective"] {
        let mut value = original.clone();
        value["payload"]["kind"] = state.into();
        invalid(&value);
    }
    let mut request = serde_json::to_value(request()).unwrap();
    request["payload"]["proof"] = json!({});
    assert!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(
            &serde_json::to_vec(&request).unwrap()
        )
        .is_err()
    );
}

#[test]
fn preclaim_codec_requires_exact_subject_home_epoch_and_effective_shape() {
    let original = serde_json::to_value(ready()).unwrap();
    for (path, bad) in [
        ("/payload/admission/owner/device", json!(id(30))),
        ("/payload/admission/bot", json!(id(30))),
        ("/payload/admission/request", json!(Uuid::nil())),
        ("/payload/admission/epoch", json!(4)),
        ("/payload/admission/paused", json!(true)),
        ("/payload/proof/global/key/owner/user_scope", json!(id(30))),
        ("/payload/proof/global/key/bot", json!(id(30))),
        ("/payload/proof/global/key/request", json!(Uuid::nil())),
        ("/payload/proof/global/config_revision", json!(0)),
        ("/payload/proof/global/policy_revision", json!(0)),
        ("/payload/proof/global/policy_revision", json!(u64::MAX)),
        ("/payload/proof/global/effects_allowed", json!(false)),
        ("/payload/proof/global/policy/destination", json!(id(30))),
        ("/payload/proof/global/policy/access", json!("unknown")),
        ("/payload/proof/routine", Value::Null),
        ("/payload/proof/routine/owner/device", json!(id(30))),
        ("/payload/proof/routine/bot", json!(id(30))),
        ("/payload/proof/routine/schedule", json!(id(30))),
        ("/payload/proof/routine/grant_revision", json!(0)),
        ("/payload/proof/routine/grant_revision", json!(u64::MAX)),
        ("/payload/proof/routine/request", json!(Uuid::nil())),
        ("/payload/proof/routine/effects_allowed", json!(false)),
        ("/payload/proof/routine/policy/destination", json!(id(30))),
    ] {
        let mut value = original.clone();
        *value.pointer_mut(path).unwrap() = bad;
        invalid(&value);
    }
    let mut foreign = original.clone();
    foreign["payload"]["proof"]["routine"]["policy"]["workspace"] = id(30).into();
    foreign["payload"]["proof"]["routine"]["policy"]["destination"] = id(30).into();
    invalid(&foreign);
    let mut overflow = ready();
    let SynchronizeRoutinePreclaimReply::Ready {
        admission, proof, ..
    } = &mut overflow.payload
    else {
        unreachable!()
    };
    admission.epoch = u64::MAX;
    proof.global.config_revision = u64::MAX;
    assert!(overflow.encode().is_err());
}

#[test]
fn preclaim_codec_correlates_entire_slot_both_hashes_and_envelope_request() {
    let reply = ready();
    for field in [
        "request",
        "scope",
        "device",
        "bot",
        "schedule",
        "occurrence",
        "semantic",
        "preparation",
    ] {
        let mut request = request();
        match field {
            "request" => request.request_id = id(31).parse().unwrap(),
            "scope" => request.payload.slot.owner.user_scope = id(31).parse().unwrap(),
            "device" => request.payload.slot.owner.device = id(31).parse().unwrap(),
            "bot" => request.payload.slot.bot = id(31).parse().unwrap(),
            "schedule" => request.payload.slot.schedule = id(31).parse().unwrap(),
            "occurrence" => request.payload.slot.occurrence_ms += 1,
            "semantic" => request.payload.semantic_hash = RoutineDigest32::from_bytes([9; 32]),
            "preparation" => {
                request.payload.preparation_hash = RoutineDigest32::from_bytes([9; 32])
            }
            _ => unreachable!(),
        }
        assert_eq!(
            reply.validate_reply_to(&request),
            Err(RoutineBridgeCodecError::Mismatch),
            "{field}"
        );
    }
}

#[test]
fn preclaim_codec_preserves_policy_page_bound_and_complete_envelope_limit() {
    let mut reply = ready();
    let SynchronizeRoutinePreclaimReply::Ready { proof, .. } = &mut reply.payload else {
        unreachable!()
    };
    proof.global.policy.selected_pages = (1..=1025)
        .map(|n| DocumentId::from_uuid(Uuid::from_u128(0xabcdef00_0000_4000_8000_000000000000 + n)))
        .collect();
    assert!(reply.encode().is_err());
    let SynchronizeRoutinePreclaimReply::Ready { proof, .. } = &mut reply.payload else {
        unreachable!()
    };
    proof.global.policy.selected_pages.pop_last();
    assert!(reply.encode().is_ok());
    let over = vec![b' '; ROUTINE_BRIDGE_BODY_LIMIT + 1];
    assert_eq!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(&over),
        Err(RoutineBridgeCodecError::ContextTooLarge)
    );
    let mut json = serde_json::to_value(request()).unwrap();
    json["version"] = 2.into();
    assert!(
        RoutineBridgeEnvelope::<SynchronizeRoutinePreclaim>::decode(
            &serde_json::to_vec(&json).unwrap()
        )
        .is_err()
    );
}
