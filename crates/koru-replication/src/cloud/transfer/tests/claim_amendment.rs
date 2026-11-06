// SPDX-License-Identifier: MPL-2.0

//! The app-connection claim amendment's vectors, against the decoder that
//! implements it (Cloud MCP Plan 3.2).
//!
//! `claim.json` freezes the amended contract's results (`code`) beside the
//! results of the decoder as it stood before the amendment (`today`). Every
//! case now asserts `code`, and each case whose two results differ proves
//! the amendment moved it.
//!
//! The decoder is the two steps a session runs: [`decode_control`] reads the
//! strict shape, and the receiving [`Stream`] checks the session, not the
//! unit (DS-D46). A case's `receiver` names that stream's end:
//! `worker_device_session` and `client` must refuse a `claim`, and only
//! `worker_app_connection_session` accepts one.

use super::*;
use crate::cloud::transfer::metadata;

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../../../contracts/vectors/cloud-transfer-v1/claim.json"
    ))
    .unwrap()
}

fn cloud_id(text: &str) -> CloudId {
    serde_json::from_value(serde_json::Value::String(text.into())).unwrap()
}

/// The scope a refusal frame decodes under, as before.
fn scope() -> Scope {
    Scope {
        principal: CloudReplicaPrincipalV1::Workspace {
            workspace_id: id(1),
            incarnation_id: id(3),
            logical_replica_id: id(4),
        },
        eligibility_workspace: ws(),
        profile: id(5),
        manifest: doc(),
    }
}

/// The dataset a vector's unit belongs to, so that the room mapping admits
/// it and only the session decides. Every vector names the same ids.
fn scope_for(unit: Unit) -> Scope {
    let other = cloud_id("33333333-3333-4333-8333-333333333333");
    let as_workspace = |id: CloudId| -> WorkspaceId {
        serde_json::from_value(serde_json::to_value(id).unwrap()).unwrap()
    };
    let as_document = |id: CloudId| -> DocumentId {
        serde_json::from_value(serde_json::to_value(id).unwrap()).unwrap()
    };
    match unit {
        Unit::Document { workspace_id, .. } => Scope {
            principal: CloudReplicaPrincipalV1::Workspace {
                workspace_id,
                incarnation_id: id(3),
                logical_replica_id: id(4),
            },
            eligibility_workspace: as_workspace(workspace_id),
            profile: id(5),
            manifest: as_document(other),
        },
        Unit::Identity { profile_id } => Scope {
            principal: CloudReplicaPrincipalV1::Profile {
                profile_id,
                lineage_id: id(3),
                logical_replica_id: id(4),
            },
            eligibility_workspace: as_workspace(other),
            profile: profile_id,
            manifest: DocumentId::from_uuid(uuid::Uuid::nil()),
        },
        other => panic!("no vector names {other:?}"),
    }
}

/// The ends a `receiver` stands for. `client` is both client ends: a
/// device's, and an app connection's, which sends references and still
/// receives none.
fn ends(receiver: &str) -> &'static [ClaimReferences] {
    match receiver {
        "worker_device_session" => &[ClaimReferences::None],
        "worker_app_connection_session" => &[ClaimReferences::Received],
        "client" => &[ClaimReferences::None, ClaimReferences::Sent],
        other => panic!("unknown receiver {other}"),
    }
}

/// Deliver one decoded control to a stream at `end` that has exchanged
/// policies, and answer what it admitted.
fn receive(control: Control, end: ClaimReferences) -> Result<(), Error> {
    let unit = match &control {
        Control::TransferStart(start) => start.unit,
        _ => unit(),
    };
    let (mut stream, _) = setup_claims(MAX_PAYLOAD, scope_for(unit), end);
    let (mut peer, _) = setup_claims(MAX_PAYLOAD, scope_for(unit), ClaimReferences::None);
    let policy = peer.initial_policy(0).unwrap();
    stream.initial_policy(0).unwrap();
    stream.receive_control(policy, 0, 0, |_| Ok(())).unwrap();
    stream
        .receive_control(control, 1, 1, |_| Ok(()))
        .map(|_| ())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn claim_vectors_match_the_amended_decoder() {
    let vectors = vectors();
    let cases = vectors["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 21);
    let mut moved = 0;
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let json = case["json"].as_str().unwrap().as_bytes();
        let expected = case["code"].as_str().unwrap();
        if case["code"] != case["today"] {
            moved += 1;
        }
        match case["kind"].as_str().unwrap() {
            "control" => {
                let decoded = decode_control(json);
                for &end in ends(case["receiver"].as_str().unwrap()) {
                    let admitted = decoded.clone().and_then(|control| receive(control, end));
                    assert_eq!(code(admitted), expected, "{id} at {end:?}");
                }
                let Ok(control) = decoded else { continue };
                let encoded = encode_record(&Record::Control(control.clone())).unwrap();
                if case.get("record_hex").is_some() || case.get("byte_identical_to").is_some() {
                    // Byte-identical: the encoder writes exactly the frozen
                    // bytes back, length prefix and tag included.
                    assert_eq!(&encoded[5..], json, "{id}");
                    if let Some(record) = case["record_hex"].as_str() {
                        assert_eq!(hex(&encoded), record, "{id}");
                    }
                } else {
                    // Property order is not significant (C15): the value
                    // survives, and is re-encoded in the encoder's order.
                    assert!(decode_control(&encoded[5..]).unwrap() == control, "{id}");
                }
            }
            "peer" => {
                let Record::Peer(frame) = decode_record(0, json).unwrap() else {
                    panic!("{id}: not a peer frame")
                };
                let mut framed = vec![0, 0, 0, 0, 0];
                framed[..4].copy_from_slice(&((json.len() + 1) as u32).to_be_bytes());
                framed.extend_from_slice(json);
                assert_eq!(hex(&framed), case["record_hex"].as_str().unwrap(), "{id}");
                let Err(refusal) = metadata::decode(scope(), frame, 1) else {
                    panic!("{id}: a refusal frame decoded as metadata")
                };
                assert_eq!(refusal.code(), expected, "{id}");
                // The producer's side: the frame a worker writes for this
                // refusal is the frozen record, byte for byte.
                let written = encode_record(&Record::Peer(refusal.frame())).unwrap();
                assert_eq!(hex(&written), case["record_hex"].as_str().unwrap(), "{id}");
            }
            other => panic!("{id}: unknown kind {other}"),
        }
    }
    // C03, C15, C16, C20 and C21: the cases whose amended result is not
    // today's. Each now answers `code`, so each proves the amendment moved it.
    assert_eq!(moved, 5);
}

/// The claim the vectors carry decodes to the SHA-256 of the fixture handle,
/// the claim row's `claim_hash`.
#[test]
fn the_vectors_claim_is_the_hash_of_the_fixture_handle() {
    let vectors = vectors();
    let hash: [u8; 32] = Sha256::digest(vectors["claim_handle_fixture"].as_str().unwrap()).into();
    let case = &vectors["cases"][2];
    assert_eq!(case["id"], "C03");
    let Control::TransferStart(start) =
        decode_control(case["json"].as_str().unwrap().as_bytes()).unwrap()
    else {
        panic!("C03 is a start")
    };
    assert!(start.claim == Some(Bytes(hash)));
    assert_eq!(
        serde_json::to_value(start.claim.unwrap()).unwrap(),
        vectors["claim_sha256_base64url"]
    );
}
