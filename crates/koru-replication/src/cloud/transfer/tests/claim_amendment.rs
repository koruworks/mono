// SPDX-License-Identifier: MPL-2.0

//! The app-connection claim amendment's vectors, against today's decoder.
//!
//! `claim.json` freezes the amended contract's results (`code`) beside the
//! results of the decoder as it stands before the amendment (`today`). This
//! test pins `today`: the strict `Start` refuses every `claim`, a device's
//! start re-encodes to its exact frozen bytes, and the two new refusal codes
//! decode as an unknown authority refusal. The decoder that accepts `claim` on
//! an app connection's session, and maps the two codes to themselves, replaces
//! this test with one asserting `code` instead.

use super::*;
use crate::cloud::transfer::metadata;

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../../../contracts/vectors/cloud-transfer-v1/claim.json"
    ))
    .unwrap()
}

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn claim_vectors_record_todays_strict_decoder_as_the_starting_state() {
    let vectors = vectors();
    let cases = vectors["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 21);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let json = case["json"].as_str().unwrap().as_bytes();
        let today = case["today"].as_str().unwrap();
        match case["kind"].as_str().unwrap() {
            "control" => {
                let decoded = decode_control(json);
                assert_eq!(code(decoded.clone()), today, "{id}");
                if let Ok(control) = decoded {
                    // Byte-identical: today's encoder writes exactly the
                    // frozen bytes back, length prefix and tag included.
                    let encoded = encode_record(&Record::Control(control)).unwrap();
                    assert_eq!(&encoded[5..], json, "{id}");
                    if let Some(record) = case["record_hex"].as_str() {
                        assert_eq!(hex(&encoded), record, "{id}");
                    }
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
                assert_eq!(refusal.code(), today, "{id}");
            }
            other => panic!("{id}: unknown kind {other}"),
        }
    }
}
