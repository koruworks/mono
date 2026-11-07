// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::cloud::{Bytes, HandshakeV1, PreparedSession, Version1};
use crate::protocol::PeerFrame;
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_store_core::{DocumentId, WorkspaceId};
use loro::{ExportMode, LoroDoc};
use sha2::{Digest, Sha256};
mod catalog;
mod claim_amendment;
mod claim_reference;
mod metadata;

fn id(n: u8) -> CloudId {
    let mut v = [n; 16];
    v[6] = 0x40 | n;
    v[8] = 0x80 | n;
    CloudId::from_bytes(v)
}
fn ws() -> WorkspaceId {
    serde_json::from_value(serde_json::to_value(id(1)).unwrap()).unwrap()
}
fn doc() -> DocumentId {
    serde_json::from_value(serde_json::to_value(id(2)).unwrap()).unwrap()
}
fn unit() -> Unit {
    Unit::Document {
        workspace_id: id(1),
        document_id: id(2),
    }
}
fn setup(limit: usize) -> (Stream, MemoryBudget) {
    let principal = CloudReplicaPrincipalV1::Workspace {
        workspace_id: id(1),
        incarnation_id: id(3),
        logical_replica_id: id(4),
    };
    setup_scope(
        limit,
        Scope {
            principal,
            eligibility_workspace: ws(),
            profile: id(5),
            manifest: doc(),
        },
    )
}
fn setup_scope(limit: usize, scope: Scope) -> (Stream, MemoryBudget) {
    setup_claims(limit, scope, ClaimReferences::None)
}
/// A stream whose session was admitted with `claims`: which way a claim
/// reference may cross it.
fn setup_claims(limit: usize, scope: Scope, claims: ClaimReferences) -> (Stream, MemoryBudget) {
    let principal = scope.principal;
    let accepted = PreparedSession {
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([0; 32]),
            generation: 1,
            expires_at_seconds: 600,
        },
        principal,
        expires_at_seconds: 600,
        generation: 1,
        claim_references: claims,
    };
    let budget = MemoryBudget::new(1024 * 1024 * 1024);
    (
        Stream::new(&accepted, scope, 0, 0, limit, budget.clone()).unwrap(),
        budget,
    )
}
fn pair(limit: usize) -> (Stream, Stream) {
    let (mut a, _) = setup(limit);
    let (mut b, _) = setup(limit);
    let ap = a.initial_policy(0).unwrap();
    let bp = b.initial_policy(0).unwrap();
    a.receive_control(bp, 0, 0, |_| Ok(())).unwrap();
    b.receive_control(ap, 0, 0, |_| Ok(())).unwrap();
    (a, b)
}
fn start(length: usize) -> Start {
    Start {
        version: Version1,
        policy_generation: 1,
        unit: unit(),
        batch_id: [0; 8],
        total_length: length,
        fragment_count: 1,
        sha256: Bytes(Sha256::digest(vec![0; length]).into()),
        claim: None,
    }
}
fn code<T>(r: Result<T, Error>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.code(),
    }
}

#[test]
fn frozen_production_vectors_decode_at_real_boundary() {
    let errata: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../contracts/vectors/cloud-transfer-v1/errata.json"
    ))
    .unwrap();
    for file in [
        include_str!("../../../../../contracts/vectors/cloud-transfer-v1/cases.json"),
        include_str!("../../../../../contracts/vectors/cloud-transfer-v1/supplement.json"),
        include_str!("../../../../../contracts/vectors/cloud-transfer-v1/profile-request.json"),
    ] {
        let cases: Vec<serde_json::Value> = serde_json::from_str(file).unwrap();
        for case in cases {
            let actual = match case["kind"].as_str().unwrap() {
                "control" => code(decode_control(case["json"].as_str().unwrap().as_bytes())),
                "peer" => code(decode_record(0, case["json"].as_str().unwrap().as_bytes())),
                "header" => code(payload_length(
                    case["length"].as_u64().unwrap() as u32,
                    case["tag"].as_u64().unwrap() as u8,
                )),
                "budget" => {
                    assert_eq!(
                        budget_millis(case["length"].as_u64().unwrap() as usize).unwrap() / 1000,
                        case["seconds"]
                    );
                    continue;
                }
                _ => panic!("unknown vector"),
            };
            let correction = &errata[case["id"].as_str().unwrap()];
            let expected = if correction.is_object() {
                assert_eq!(case["code"], correction["original_expected"]);
                correction["expected"].as_str().unwrap()
            } else {
                case["code"].as_str().unwrap()
            };
            assert_eq!(actual, expected, "{}", case["id"]);
        }
    }
}

#[test]
fn frozen_directional_sequences_run_against_production_state() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../contracts/vectors/cloud-transfer-v1/sequences.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let limit = case["limit"].as_u64().unwrap() as usize;
        let (mut a, mut b) = pair(limit);
        let (mut from_a, mut from_b) = (None, None);
        for step in case["steps"].as_array().unwrap() {
            let arg = step[1].as_u64().unwrap() as usize;
            let result = (|| -> Result<(), Error> {
                match step[0].as_str().unwrap() {
                    "start_a" | "start_b" => {
                        let (sender, receiver, saved) = if step[0] == "start_a" {
                            (&mut a, &mut b, &mut from_a)
                        } else {
                            (&mut b, &mut a, &mut from_b)
                        };
                        let (c, _) = sender.begin_outgoing(
                            unit(),
                            [arg as u8; 8],
                            vec![0; arg],
                            1,
                            1,
                            |_| Ok(()),
                        )?;
                        let Control::TransferStart(start) = &c else {
                            panic!()
                        };
                        *saved = Some(start.clone());
                        receiver.receive_control(c, 1, 1, |_| Ok(()))?;
                    }
                    "accept_a" | "accept_b" | "defer_a" => {
                        let (sender, receiver, saved) = if step[0] == "accept_b" {
                            (&mut b, &mut a, &from_b)
                        } else {
                            (&mut a, &mut b, &from_a)
                        };
                        let decision = if step[0] == "defer_a" {
                            Decision::Defer
                        } else {
                            Decision::Accept
                        };
                        let c =
                            receiver.decide(saved.as_ref().unwrap(), decision, 2, |_| Ok(()))?;
                        sender.receive_control(c, 2, 2, |_| Ok(()))?;
                    }
                    "policy_a" => {
                        a.receive_control(Control::policy(arg as i64, limit), 2, 2, |_| Ok(()))?;
                    }
                    "drain_b" => b.drain(),
                    "send_a" => {
                        // An idle pump has nothing to emit; calling the active
                        // transfer API on it is itself invalid, not an ACK.
                        assert_eq!(a.status().outgoing_bytes, 0);
                        assert_eq!(
                            code(a.next_outgoing(3, |_| Ok(()))),
                            "sync_transfer_invalid"
                        );
                    }
                    _ => panic!("unknown frozen step"),
                }
                Ok(())
            })();
            assert_eq!(code(result), step[2], "{} {step}", case["id"]);
        }
    }
}

#[test]
fn ds_d39_escaped_duplicate_nested_and_unrelated_fields_refuse() {
    for bytes in [
        br#"{"kind":"sync_request","\u006bind":"sync_request"}"#.as_slice(),
        br#"{"kind":"catalog","workspaces":[],"profiles":[],"workspace_id":"11111111-1111-4111-8111-111111111111"}"#,
        br#"{"kind":"profile_request","unit":{"type":"identity","profile":"11111111-1111-4111-8111-111111111111","\u0070rofile":"11111111-1111-4111-8111-111111111111"},"version":[]}"#,
    ] {assert_eq!(code(decode_record(0,bytes)),"sync_transfer_invalid");}
}

#[test]
fn open_blob_attribute_names_do_not_open_typed_attribute_values() {
    let frame = serde_json::json!({"kind":"inventory","workspace_id":ws(),"documents":[],
        "replica":{"profile_id":id(5),"profile_name":"","profile_locale":"","profile_created_at_ms":0,
          "workspace_id":ws(),"workspace_name":"","workspace_created_at_ms":0,"manifest_document":doc()},
        "blobs":[{"hash":"b3:abcd","size":1,"mime":"text/plain","created_at":0,
          "attributes":{"arbitrary extension name":{"type":"text","value":"x"}}}]});
    let valid = serde_json::to_vec(&frame).unwrap();
    assert_eq!(code(decode_record(0, &valid)), "ok");
    let mut unknown = frame.clone();
    unknown["blobs"][0]["attributes"]["arbitrary extension name"]["unexpected"] = true.into();
    assert_eq!(
        code(decode_record(0, &serde_json::to_vec(&unknown).unwrap())),
        "sync_transfer_invalid"
    );
    let duplicate = String::from_utf8(valid)
        .unwrap()
        .replace("\"value\":\"x\"", r#""value":"x","\u0076alue":"x""#);
    assert_eq!(
        code(decode_record(0, duplicate.as_bytes())),
        "sync_transfer_invalid"
    );
}

#[test]
fn small_transfers_are_automatic_but_200001_requires_exact_decision() {
    for length in [199999, 200000, 200001] {
        let (mut a, mut b) = pair(RESTRICTED_LIMIT);
        let (c, admission) = a
            .begin_outgoing(unit(), [1; 8], vec![0; length], 1, 1, |_| Ok(()))
            .unwrap();
        assert_eq!(
            admission,
            if length <= 200000 {
                Admission::Automatic
            } else {
                Admission::AwaitDecision
            }
        );
        let Control::TransferStart(s) = &c else {
            panic!()
        };
        let s = s.clone();
        assert_eq!(b.receive_control(c, 1, 1, |_| Ok(())).unwrap(), admission);
        if length > 200000 {
            let d = b.decide(&s, Decision::Accept, 2, |_| Ok(())).unwrap();
            assert_eq!(
                a.receive_control(d, 2, 2, |_| Ok(())).unwrap(),
                Admission::Accepted
            );
        }
        while let Some(record) = a.next_outgoing(3, |_| Ok(())).unwrap() {
            let Record::Peer(frame) = record.record() else {
                panic!()
            };
            b.receive_peer(frame.clone(), 3, |_| Ok(())).unwrap();
        }
        assert!(b.status().incoming_complete);
    }
}

#[test]
fn directions_and_streams_have_independent_policies_and_decisions() {
    let (mut a, mut b) = pair(RESTRICTED_LIMIT);
    let (ac, _) = a
        .begin_outgoing(unit(), [1; 8], vec![0; 200001], 1, 1, |_| Ok(()))
        .unwrap();
    let (bc, _) = b
        .begin_outgoing(unit(), [2; 8], vec![1; 200001], 1, 1, |_| Ok(()))
        .unwrap();
    let Control::TransferStart(astart) = &ac else {
        panic!()
    };
    let astart = astart.clone();
    let Control::TransferStart(bstart) = &bc else {
        panic!()
    };
    let bstart = bstart.clone();
    a.receive_control(bc, 1, 1, |_| Ok(())).unwrap();
    b.receive_control(ac, 1, 1, |_| Ok(())).unwrap();
    let ad = a.decide(&bstart, Decision::Accept, 2, |_| Ok(())).unwrap();
    let bd = b.decide(&astart, Decision::Accept, 2, |_| Ok(())).unwrap();
    a.receive_control(bd, 2, 2, |_| Ok(())).unwrap();
    b.receive_control(ad, 2, 2, |_| Ok(())).unwrap();
    assert!(a.next_outgoing(3, |_| Ok(())).unwrap().is_some());
    assert!(b.next_outgoing(3, |_| Ok(())).unwrap().is_some());
    let (c, d) = pair(MAX_PAYLOAD);
    assert_eq!(c.status().local_generation, 1);
    assert_eq!(d.status().peer_generation, Some(1));
}

#[test]
fn zero_pause_cannot_be_bypassed_by_manual_accept() {
    let (mut a, mut b) = pair(0);
    assert_eq!(
        code(a.begin_outgoing(unit(), [0; 8], vec![0], 1, 1, |_| Ok(()))),
        "sync_unavailable"
    );
    assert_eq!(
        code(b.receive_control(Control::TransferStart(start(1)), 1, 1, |_| Ok(()))),
        "sync_transfer_invalid"
    );
    assert!(b.status().closed.is_some());
}

#[test]
fn ds_d41_stale_start_differs_from_future_and_idle_equal_policy_is_a_generation_change() {
    for (g, expected) in [(1, "sync_policy_changed"), (3, "sync_transfer_invalid")] {
        let (mut a, _) = pair(MAX_PAYLOAD);
        let _ = a.change_policy(MAX_PAYLOAD, 1).unwrap();
        let mut s = start(1);
        s.policy_generation = g;
        assert_eq!(
            code(a.receive_control(Control::TransferStart(s), 1, 1, |_| Ok(()))),
            expected
        );
    }
}

#[test]
fn identical_peer_policy_is_noop_but_newer_policy_cancels_outbound() {
    let (mut a, _) = pair(MAX_PAYLOAD);
    a.begin_outgoing(unit(), [0; 8], vec![0], 1, 1, |_| Ok(()))
        .unwrap();
    a.receive_control(Control::policy(1, MAX_PAYLOAD), 2, 2, |_| Ok(()))
        .unwrap();
    assert_eq!(a.status().outgoing_bytes, 1);
    assert_eq!(
        code(a.receive_control(Control::policy(2, MAX_PAYLOAD), 3, 3, |_| Ok(()))),
        "sync_policy_changed"
    );
    assert_eq!(a.status().outgoing_bytes, 0);
}

#[test]
fn ds_d40_absolute_budget_does_not_move_with_fragments_and_lifetime_preflight() {
    let (mut a, _) = pair(MAX_PAYLOAD);
    a.receive_control(Control::TransferStart(start(1)), 1, 1, |_| Ok(()))
        .unwrap();
    a.check(31000).unwrap();
    assert_eq!(code(a.check(31001)), "sync_transfer_timeout");
    let (mut a, _) = pair(MAX_PAYLOAD);
    assert_eq!(
        code(a.receive_control(
            Control::TransferStart(start(MAX_PAYLOAD)),
            44001,
            44001,
            |_| Ok(())
        )),
        "sync_unavailable"
    );
}

#[test]
fn ds_d43_shared_memory_is_reserved_before_reassembly_and_released_on_fence() {
    let (mut a, budget) = setup(MAX_PAYLOAD);
    a.initial_policy(0).unwrap();
    a.receive_control(Control::policy(1, MAX_PAYLOAD), 0, 0, |_| Ok(()))
        .unwrap();
    a.receive_control(Control::TransferStart(start(200000)), 1, 1, |_| Ok(()))
        .unwrap();
    assert!(budget.used() >= 200000);
    a.cancel(Error::Authority(Refusal::DeviceRevoked));
    assert_eq!(budget.used(), 0);
    let held = budget.reserve(1024 * 1024 * 1024).unwrap();
    assert_eq!(code(budget.reserve_record(4096)), "sync_unavailable");
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn valid_loro_is_not_delivered_until_complete_hash_checked_handoff() {
    let source = LoroDoc::new();
    source
        .get_text("text")
        .insert(0, "hello العربية 日本語")
        .unwrap();
    source.commit();
    let bytes = source.export(ExportMode::snapshot()).unwrap();
    let (mut a, mut b) = pair(MAX_PAYLOAD);
    let (c, _) = a
        .begin_outgoing(unit(), [0; 8], bytes, 1, 1, |_| Ok(()))
        .unwrap();
    b.receive_control(c, 1, 1, |_| Ok(())).unwrap();
    let destination = LoroDoc::new();
    assert!(destination.oplog_vv().is_empty());
    while let Some(record) = a.next_outgoing(2, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        b.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap();
    }
    assert!(destination.oplog_vv().is_empty());
    b.handoff(
        3,
        |_| Ok(()),
        |done| {
            assert!(matches!(
                done.origin,
                koru_store_core::ports::UpdateOrigin::Cloud { .. }
            ));
            destination.import(&done.bytes).unwrap();
        },
    )
    .unwrap();
    assert_eq!(source.get_deep_value(), destination.get_deep_value());
    // Policy is non-retroactive after the completed handoff.
    b.change_policy(RESTRICTED_LIMIT, 4).unwrap();
    assert_eq!(source.get_deep_value(), destination.get_deep_value());
}

#[test]
fn ds_d41_complete_but_not_handed_off_is_discarded_on_policy_change() {
    let (mut a, mut b) = pair(MAX_PAYLOAD);
    let (c, _) = a
        .begin_outgoing(unit(), [0; 8], vec![0; 10], 1, 1, |_| Ok(()))
        .unwrap();
    b.receive_control(c, 1, 1, |_| Ok(())).unwrap();
    while let Some(record) = a.next_outgoing(2, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        b.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap();
    }
    assert_eq!(
        code(b.change_policy(RESTRICTED_LIMIT, 3)),
        "sync_policy_changed"
    );
    let mut imported = false;
    assert!(b.handoff(3, |_| Ok(()), |_| imported = true).is_err());
    assert!(!imported);
}

#[test]
fn policy_initialization_and_pending_generation_replacement_fail_closed() {
    let (mut a, _) = setup(MAX_PAYLOAD);
    a.initial_policy(0).unwrap();
    assert_eq!(code(a.check(10000)), "sync_transfer_timeout");
    let (mut a, mut b) = pair(RESTRICTED_LIMIT);
    let (c, _) = a
        .begin_outgoing(unit(), [0; 8], vec![0; 200001], 1, 1, |_| Ok(()))
        .unwrap();
    let Control::TransferStart(s) = c.clone() else {
        panic!()
    };
    b.receive_control(c, 1, 1, |_| Ok(())).unwrap();
    a.drain();
    b.drain();
    assert_eq!(a.status().outgoing_bytes, 0);
    assert_eq!(
        code(b.decide(&s, Decision::Accept, 2, |_| Ok(()))),
        "sync_transfer_invalid"
    );
    assert_eq!(
        code(a.receive_control(Control::decision(&s, Decision::Accept), 2, 2, |_| Ok(()))),
        "sync_transfer_invalid"
    );
}

#[test]
fn explicit_accept_rechecks_lifetime_without_moving_original_deadline() {
    let (mut a, mut b) = pair(RESTRICTED_LIMIT);
    // 37s budget +15 reserve leaves exactly52s at start.
    let (c, _) = a
        .begin_outgoing(unit(), [0; 8], vec![0; 200001], 548000, 548000, |_| Ok(()))
        .unwrap();
    let Control::TransferStart(s) = c.clone() else {
        panic!()
    };
    b.receive_control(c, 548000, 548000, |_| Ok(())).unwrap();
    assert_eq!(
        code(b.decide(&s, Decision::Accept, 548001, |_| Ok(()))),
        "sync_unavailable"
    );
    assert_eq!(
        code(a.receive_control(
            Control::decision(&s, Decision::Accept),
            548001,
            548001,
            |_| Ok(())
        )),
        "sync_unavailable"
    );
}

#[test]
fn maximum_payload_crosses_bounded_frames_and_drains_at_original_slow_budget() {
    let (mut a, mut b) = pair(MAX_PAYLOAD);
    let (c, _) = a
        .begin_outgoing(unit(), [0; 8], vec![255; MAX_PAYLOAD], 1000, 1000, |_| {
            Ok(())
        })
        .unwrap();
    b.receive_control(c, 1000, 1000, |_| Ok(())).unwrap();
    a.drain();
    b.drain();
    let mut frames = 0;
    while let Some(record) = a.next_outgoing(500000, |_| Ok(())).unwrap() {
        let encoded = encode_record(record.record()).unwrap();
        assert!(
            encoded.len() <= 1024 * 1024 + 5,
            "one document must be split into bounded batches under the shared host budget"
        );
        let decoded = decode_record(encoded[4], &encoded[5..]).unwrap();
        let Record::Peer(frame) = decoded else {
            panic!()
        };
        b.receive_peer(frame, 500000, |_| Ok(())).unwrap();
        frames += 1;
    }
    assert!(frames >= 4);
    a.outbound_written(542999).unwrap();
    assert_eq!(
        b.handoff(542999, |_| Ok(()), |v| v.bytes.len()).unwrap(),
        MAX_PAYLOAD
    );
    assert_eq!(a.status().outgoing_bytes, 0);
}

#[test]
fn malformed_fragment_matrix_has_exact_refusal_and_zero_handoff() {
    use loro_protocol::{ProtocolMessage as M, encoding};
    for fault in 0..8 {
        let (mut a, mut b) = pair(MAX_PAYLOAD);
        let (c, _) = a
            .begin_outgoing(unit(), [0; 8], vec![5; 10], 1, 1, |_| Ok(()))
            .unwrap();
        b.receive_control(c, 1, 1, |_| Ok(())).unwrap();
        let record = a.next_outgoing(2, |_| Ok(())).unwrap().unwrap();
        let Record::Peer(PeerFrame::LoroBatch {
            workspace_id,
            messages,
        }) = record.record()
        else {
            panic!()
        };
        let mut messages = messages.clone();
        match fault {
            0 => {
                messages.remove(0);
            }
            1 => {
                messages.insert(1, messages[0].clone());
            }
            2 => {
                messages.push(messages[1].clone());
            }
            3 | 4 | 5 => {
                let mut msg = encoding::try_decode(&messages[1]).unwrap();
                let M::DocUpdateFragment {
                    index,
                    fragment,
                    room_id,
                    ..
                } = &mut msg
                else {
                    panic!()
                };
                if fault == 3 {
                    *index = 1;
                } else if fault == 4 {
                    fragment[0] ^= 1;
                } else {
                    room_id.push('x');
                }
                messages[1] = encoding::encode(&msg).unwrap();
            }
            6 => {
                messages[1].push(0);
            }
            7 => {
                let mut msg = encoding::try_decode(&messages[0]).unwrap();
                let M::DocUpdateFragmentHeader {
                    total_size_bytes, ..
                } = &mut msg
                else {
                    panic!()
                };
                *total_size_bytes += 1;
                messages[0] = encoding::encode(&msg).unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            code(b.receive_peer(
                PeerFrame::LoroBatch {
                    workspace_id: *workspace_id,
                    messages
                },
                2,
                |_| Ok(())
            )),
            "sync_transfer_invalid",
            "fault{fault}"
        );
        let mut reached = false;
        assert!(b.handoff(3, |_| Ok(()), |_| reached = true).is_err());
        assert!(!reached);
        assert_eq!(b.status().incoming_bytes, 0);
    }
}

#[test]
fn content_free_loro_ack_uses_existing_room_but_bare_content_never_bypasses_start() {
    use loro_protocol::{BatchId, CrdtType, ProtocolMessage as M, encoding};
    let (mut a, _) = pair(MAX_PAYLOAD);
    let room = format!("workspace:{}:manifest", ws());
    let message = encoding::encode(&M::Leave {
        crdt: CrdtType::Loro,
        room_id: room.clone(),
    })
    .unwrap();
    assert_eq!(
        a.receive_peer(
            PeerFrame::Loro {
                workspace_id: ws(),
                message
            },
            1,
            |_| Ok(())
        )
        .unwrap(),
        Admission::Metadata
    );
    let message = encoding::encode(&M::DocUpdate {
        crdt: CrdtType::Loro,
        room_id: room,
        batch_id: BatchId([0; 8]),
        updates: vec![vec![1]],
    })
    .unwrap();
    assert_eq!(
        code(a.receive_peer(
            PeerFrame::Loro {
                workspace_id: ws(),
                message
            },
            2,
            |_| Ok(())
        )),
        "sync_transfer_invalid"
    );
}

#[test]
fn profile_identity_private_and_personal_use_actual_valid_unit_bytes() {
    use koru_store_core::{
        Locale, Profile, Timestamp,
        profile_units::{IdentityManifest, PersonalManifest, PrivateManifest},
    };
    let profile = Profile::new("Profile", Locale::default(), Timestamp::from_millis(42)).unwrap();
    let pid = CloudId::from_bytes(*profile.id.as_uuid().as_bytes());
    let identity = IdentityManifest::create(&profile)
        .unwrap()
        .export_snapshot()
        .unwrap();
    let private = PrivateManifest::create(profile.id)
        .unwrap()
        .export_snapshot()
        .unwrap();
    let personal = PersonalManifest::create(profile.id, ws())
        .unwrap()
        .export_snapshot()
        .unwrap();
    for (unit, bytes) in [
        (Unit::Identity { profile_id: pid }, identity),
        (Unit::Private { profile_id: pid }, private),
        (
            Unit::Personal {
                profile_id: pid,
                workspace_id: id(1),
            },
            personal,
        ),
    ] {
        let principal = CloudReplicaPrincipalV1::Profile {
            profile_id: pid,
            lineage_id: id(3),
            logical_replica_id: id(4),
        };
        let scope = Scope {
            principal,
            eligibility_workspace: ws(),
            profile: pid,
            manifest: doc(),
        };
        let (mut a, _) = setup_scope(MAX_PAYLOAD, scope);
        let (mut b, _) = setup_scope(MAX_PAYLOAD, scope);
        let ap = a.initial_policy(0).unwrap();
        let bp = b.initial_policy(0).unwrap();
        a.receive_control(bp, 0, 0, |_| Ok(())).unwrap();
        b.receive_control(ap, 0, 0, |_| Ok(())).unwrap();
        let (c, _) = a
            .begin_outgoing(unit, [1; 8], bytes, 1, 1, |_| Ok(()))
            .unwrap();
        b.receive_control(c, 1, 1, |_| Ok(())).unwrap();
        while let Some(record) = a.next_outgoing(2, |_| Ok(())).unwrap() {
            let Record::Peer(frame) = record.record() else {
                panic!()
            };
            b.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap();
        }
        b.handoff(
            3,
            |_| Ok(()),
            |done| match unit {
                Unit::Identity { .. } => {
                    IdentityManifest::from_snapshot(&done.bytes).unwrap();
                }
                Unit::Private { .. } => {
                    PrivateManifest::from_snapshot(&done.bytes).unwrap();
                }
                Unit::Personal { .. } => {
                    PersonalManifest::from_snapshot(&done.bytes).unwrap();
                }
                _ => unreachable!(),
            },
        )
        .unwrap();
    }
}

/// D234: a worker whose Workspace has no retained manifest identity learns it
/// from the device's first manifest transfer, and only from a transfer whose
/// frames then carry the manifest room. Nothing is invented on either side.
#[test]
fn an_unbound_worker_scope_binds_the_manifest_from_the_first_transfer_start() {
    let principal = CloudReplicaPrincipalV1::Workspace {
        workspace_id: id(1),
        incarnation_id: id(3),
        logical_replica_id: id(4),
    };
    let client_scope = Scope {
        principal,
        eligibility_workspace: ws(),
        profile: id(5),
        manifest: doc(),
    };
    let worker_scope = Scope {
        manifest: Scope::unbound_manifest(),
        ..client_scope
    };
    assert!(client_scope.manifest_bound());
    assert!(!worker_scope.manifest_bound());
    // Before the bind the manifest room maps to the placeholder both ways,
    // so a join for the manifest room is authorized as the manifest unit.
    let placeholder = Unit::Document {
        workspace_id: id(1),
        document_id: CloudId::from_bytes(*Scope::unbound_manifest().as_uuid().as_bytes()),
    };
    assert_eq!(
        worker_scope.room(placeholder).unwrap(),
        client_scope.room(unit()).unwrap()
    );
    assert_eq!(
        worker_scope.object_unit(placeholder).unwrap(),
        crate::cloud::object::Unit::WorkspaceManifest
    );

    let (mut client, _) = setup_scope(MAX_PAYLOAD, client_scope);
    let (mut worker, _) = setup_scope(MAX_PAYLOAD, worker_scope);
    let cp = client.initial_policy(0).unwrap();
    let wp = worker.initial_policy(0).unwrap();
    client.receive_control(wp, 0, 0, |_| Ok(())).unwrap();
    worker.receive_control(cp, 0, 0, |_| Ok(())).unwrap();

    let manifest = LoroDoc::new();
    manifest.get_map("meta").insert("name", "Bound").unwrap();
    manifest.commit();
    let bytes = manifest.export(ExportMode::snapshot()).unwrap();
    let (start, _) = client
        .begin_outgoing(unit(), [7; 8], bytes.clone(), 1, 1, |_| Ok(()))
        .unwrap();
    worker.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    // Bound at the Start: the worker's scope now names the device's id.
    assert!(worker.scope().manifest_bound());
    assert_eq!(worker.scope().manifest, doc());
    while let Some(record) = client.next_outgoing(2, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        worker.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap();
    }
    let complete = worker.handoff(3, |_| Ok(()), |done| done).unwrap();
    assert_eq!(complete.unit, unit());
    assert_eq!(complete.bytes, bytes);
    assert_eq!(
        worker.scope().object_unit(complete.unit).unwrap(),
        crate::cloud::object::Unit::WorkspaceManifest
    );
}

/// A transfer the device labels as an ordinary document never binds: its
/// frames carry the document room, which the bound-as-manifest expectation
/// refuses, and the worker's scope is left as it was.
#[test]
fn a_document_transfer_cannot_pose_as_the_manifest_binding() {
    let principal = CloudReplicaPrincipalV1::Workspace {
        workspace_id: id(1),
        incarnation_id: id(3),
        logical_replica_id: id(4),
    };
    let client_scope = Scope {
        principal,
        eligibility_workspace: ws(),
        profile: id(5),
        manifest: doc(),
    };
    let worker_scope = Scope {
        manifest: Scope::unbound_manifest(),
        ..client_scope
    };
    let (mut client, _) = setup_scope(MAX_PAYLOAD, client_scope);
    let (mut worker, _) = setup_scope(MAX_PAYLOAD, worker_scope);
    let cp = client.initial_policy(0).unwrap();
    let wp = worker.initial_policy(0).unwrap();
    client.receive_control(wp, 0, 0, |_| Ok(())).unwrap();
    worker.receive_control(cp, 0, 0, |_| Ok(())).unwrap();
    // The client uploads an ordinary document first (not the manifest).
    let other = Unit::Document {
        workspace_id: id(1),
        document_id: id(6),
    };
    let (start, _) = client
        .begin_outgoing(other, [8; 8], vec![1, 2, 3], 1, 1, |_| Ok(()))
        .unwrap();
    worker.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    // The worker took the Start as the manifest declaration, but the
    // client's frames say `document:<id>`; the first fragment refuses and
    // nothing has been handed off.
    let record = client.next_outgoing(2, |_| Ok(())).unwrap().unwrap();
    let Record::Peer(frame) = record.record() else {
        panic!()
    };
    assert_eq!(
        code(worker.receive_peer(frame.clone(), 2, |_| Ok(()))),
        "sync_transfer_invalid"
    );
    assert!(!worker.status().incoming_complete);
}

/// The device's join for the manifest room reaches an unbound worker scope
/// before any Start: it must decode to the placeholder manifest unit and be
/// admitted as metadata, not refused.
#[test]
fn an_unbound_worker_scope_admits_the_manifest_join_request() {
    let principal = CloudReplicaPrincipalV1::Workspace {
        workspace_id: id(1),
        incarnation_id: id(3),
        logical_replica_id: id(4),
    };
    let client_scope = Scope {
        principal,
        eligibility_workspace: ws(),
        profile: id(5),
        manifest: doc(),
    };
    let worker_scope = Scope {
        manifest: Scope::unbound_manifest(),
        ..client_scope
    };
    let (mut client, _) = setup_scope(MAX_PAYLOAD, client_scope);
    let (mut worker, _) = setup_scope(MAX_PAYLOAD, worker_scope);
    let cp = client.initial_policy(0).unwrap();
    let wp = worker.initial_policy(0).unwrap();
    client.receive_control(wp, 0, 0, |_| Ok(())).unwrap();
    worker.receive_control(cp, 0, 0, |_| Ok(())).unwrap();
    let manifest = LoroDoc::new();
    manifest.get_map("meta").insert("name", "Joined").unwrap();
    manifest.commit();
    let Record::Peer(frame) =
        super::metadata::join_request(client_scope, unit(), manifest.oplog_vv().encode()).unwrap()
    else {
        panic!("a join is a peer frame")
    };
    let (admission, metadata) = worker
        .receive_peer_owned(frame, 1, |_| Ok(()))
        .expect("the manifest join is admitted before the bind");
    assert_eq!(admission, Admission::Metadata);
    let messages = super::metadata::decode(worker.scope(), metadata.unwrap(), 4)
        .expect("the manifest join decodes against the placeholder");
    assert!(matches!(
        messages.as_slice(),
        [super::metadata::Metadata::Request(request)]
            if request.unit
                == Unit::Document {
                    workspace_id: id(1),
                    document_id: CloudId::from_bytes(*Scope::unbound_manifest().as_uuid().as_bytes()),
                }
    ));
}
