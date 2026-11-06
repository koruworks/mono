// SPDX-License-Identifier: MPL-2.0

//! The app-connection claim amendment's vectors, against the decoder as it
//! stands.
//!
//! `claim.json` freezes the amended contract's results (`code`) beside the
//! results of the decoder as it stood before the amendment (`today`). The
//! amendment lands in parts, so each case asserts `code` once its part is
//! built and `today` until then:
//!
//! - `sync_unit_exists` (C20) is built (Cloud MCP Plan 2.2): the decoder maps
//!   it to itself;
//! - the strict `Start` still refuses every `claim`, a device's start
//!   re-encodes to its exact frozen bytes, and `sync_document_claimed` (C21)
//!   still decodes as an unknown authority refusal. Plan 3.2's decoder, which
//!   accepts `claim` on an app connection's session, moves those cases to
//!   `code` too.

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

/// The cases whose part of the amendment is built, so the decoder answers
/// the amended `code` rather than `today`.
const BUILT: &[&str] = &["C20"];

fn expected<'a>(case: &'a serde_json::Value, id: &str) -> &'a str {
    let field = if BUILT.contains(&id) { "code" } else { "today" };
    case[field].as_str().unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn claim_vectors_match_the_decoder_part_by_part() {
    let vectors = vectors();
    let cases = vectors["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 21);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let json = case["json"].as_str().unwrap().as_bytes();
        let expected = expected(case, id);
        match case["kind"].as_str().unwrap() {
            "control" => {
                let decoded = decode_control(json);
                assert_eq!(code(decoded.clone()), expected, "{id}");
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
                assert_eq!(refusal.code(), expected, "{id}");
                if BUILT.contains(&id) {
                    // The producer's side: the frame a worker writes for this
                    // refusal is the frozen record, byte for byte.
                    let written = encode_record(&Record::Peer(refusal.frame())).unwrap();
                    assert_eq!(hex(&written), case["record_hex"].as_str().unwrap(), "{id}");
                }
            }
            other => panic!("{id}: unknown kind {other}"),
        }
    }
    // Every built case is one the vectors still hold, and none is a case
    // whose amended result equals today's (that would prove nothing).
    for built in BUILT {
        let case = cases.iter().find(|c| c["id"] == *built).unwrap();
        assert_ne!(case["code"], case["today"], "{built}");
    }
}
