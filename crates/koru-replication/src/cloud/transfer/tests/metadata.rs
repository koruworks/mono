// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::cloud::transfer;
use crate::cloud::transfer::metadata::{self, Metadata};
use loro_protocol::{CrdtType, ProtocolMessage as Message, encoding};

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

#[test]
fn uuid_v5_documents_keep_their_identity_across_control_and_loro_metadata() {
    let mut bytes = *id(2).as_bytes();
    bytes[6] = 0x52;
    let document_id = CloudId::from_bytes(bytes);
    let document = Unit::Document {
        workspace_id: id(1),
        document_id,
    };
    let mut transfer = start(1);
    transfer.unit = document;
    let encoded = serde_json::to_vec(&Control::TransferStart(transfer)).unwrap();
    assert!(
        matches!(decode_control(&encoded).unwrap(), Control::TransferStart(value) if value.unit == document)
    );
    assert!(
        matches!(scope().object_unit(document).unwrap(), crate::cloud::object::Unit::WorkspaceDocument { document_id: value } if value == document_id)
    );

    let Record::Peer(join) = metadata::join_request(scope(), document, Vec::new()).unwrap() else {
        panic!()
    };
    assert!(
        matches!(metadata::decode(scope(), join, 1).unwrap().as_slice(), [Metadata::Request(request)] if request.unit == document)
    );
    let Record::Peer(ack) = scope().committed_ack(document, [1; 8]).unwrap() else {
        panic!()
    };
    assert!(
        matches!(metadata::decode(scope(), ack, 1).unwrap().as_slice(), [Metadata::Ack { unit, ok: true, .. }] if *unit == document)
    );

    let wrong_scope = Unit::Document {
        workspace_id: id(3),
        document_id,
    };
    assert!(scope().object_unit(wrong_scope).is_err());
    assert!(
        !Unit::Document {
            workspace_id: document_id,
            document_id
        }
        .valid()
    );
    assert!(
        !Unit::Identity {
            profile_id: document_id
        }
        .valid()
    );
    bytes[8] = 0;
    assert!(
        !Unit::Document {
            workspace_id: id(1),
            document_id: CloudId::from_bytes(bytes)
        }
        .valid()
    );
}

#[test]
fn live_rooms_negotiate_on_existing_loro_joins_and_leave_in_scope() {
    for live in [false, true] {
        let Record::Peer(frame) =
            metadata::join_request_mode(scope(), unit(), Vec::new(), live).unwrap()
        else {
            panic!()
        };
        let mut decoded = metadata::decode(scope(), frame, 1).unwrap();
        let Metadata::Request(request) = decoded.remove(0) else {
            panic!()
        };
        assert_eq!(request.live, live);
        let Some(Record::Peer(reply)) = request
            .response(scope(), transfer::empty_version())
            .unwrap()
        else {
            panic!()
        };
        assert!(
            matches!(metadata::decode(scope(), reply, 1).unwrap().as_slice(),
            [Metadata::Joined { live: supported, .. }] if *supported == live)
        );
    }
    let message = Message::Leave {
        crdt: CrdtType::Loro,
        room_id: scope().room(unit()).unwrap(),
    };
    let frame = PeerFrame::Loro {
        workspace_id: ws(),
        message: encoding::encode(&message).unwrap(),
    };
    assert!(
        matches!(metadata::decode(scope(), frame, 1).unwrap().as_slice(), [Metadata::Leave(left)] if *left == unit())
    );
    let other = Unit::Document {
        workspace_id: id(99),
        document_id: id(100),
    };
    assert!(metadata::join_request_mode(scope(), other, Vec::new(), true).is_err());
}

#[test]
fn room_acknowledgements_merge_concurrent_knowledge_without_inventing_coverage() {
    let a = LoroDoc::new();
    let b = LoroDoc::new();
    a.get_text("text").insert(0, "a").unwrap();
    a.commit();
    b.get_text("text").insert(0, "b").unwrap();
    b.commit();
    let confirmed =
        metadata::confirmed_version(&a.state_vv().encode(), &b.state_vv().encode()).unwrap();
    let vv = loro::VersionVector::decode(&confirmed).unwrap();
    assert!(vv.includes_vv(&a.state_vv()));
    assert!(vv.includes_vv(&b.state_vv()));
    let request = metadata::Request::push(unit(), a.state_vv().encode()).unwrap();
    assert!(!request.needs_payload(&a.state_vv().encode()).unwrap());
    assert!(request.needs_payload(&confirmed).unwrap());
    assert!(
        request.response(scope(), confirmed).unwrap().is_none(),
        "push is not another join reply"
    );
}

#[test]
fn cloud_comparison_is_bounded_and_cannot_escape_the_workspace_grant() {
    use crate::protocol::{CLOUD_COMPARE_LIMIT, CloudDocumentDigest};
    let row = CloudDocumentDigest {
        document_id: doc(),
        digest: [1; 32],
    };
    let frame = |workspace_id, documents| PeerFrame::CloudCompare {
        workspace_id,
        documents,
    };
    assert!(metadata::decode(scope(), frame(ws(), vec![row.clone()]), 1).is_ok());
    assert!(metadata::decode(scope(), frame(WorkspaceId::new(), vec![row.clone()]), 1).is_err());
    assert!(metadata::decode(scope(), frame(ws(), vec![]), 1).is_err());
    assert!(metadata::decode(scope(), frame(ws(), vec![row.clone(); 2]), 1).is_err());
    assert!(
        metadata::decode(
            scope(),
            frame(ws(), vec![row.clone(); CLOUD_COMPARE_LIMIT + 1]),
            1
        )
        .is_err()
    );
    let profile = Scope {
        principal: CloudReplicaPrincipalV1::Profile {
            profile_id: id(5),
            lineage_id: id(6),
            logical_replica_id: id(7),
        },
        ..scope()
    };
    assert!(metadata::decode(profile, frame(ws(), vec![row.clone()]), 1).is_err());
    let bytes = encode_record(&Record::Peer(frame(ws(), vec![row]))).unwrap();
    assert!(decode_record(bytes[4], &bytes[5..]).is_ok());
}
fn join(version: Vec<u8>) -> PeerFrame {
    PeerFrame::Loro {
        workspace_id: ws(),
        message: encoding::encode(&Message::JoinRequest {
            crdt: CrdtType::Loro,
            room_id: scope().room(unit()).unwrap(),
            version,
            auth: Vec::new(),
        })
        .unwrap(),
    }
}

#[test]
fn scoped_requests_normalize_empty_vectors_and_compare_actual_coverage() {
    let document = LoroDoc::new();
    document.get_text("text").insert(0, "cloud").unwrap();
    document.commit();
    for vector in [
        Vec::new(),
        loro::VersionVector::default().encode(),
        document.state_vv().encode(),
    ] {
        let mut decoded = metadata::decode(scope(), join(vector.clone()), 1).unwrap();
        let Metadata::Request(request) = decoded.remove(0) else {
            panic!("request")
        };
        assert_eq!(request.unit, unit());
        assert_eq!(
            request
                .needs_payload(&document.state_vv().encode())
                .unwrap(),
            vector != document.state_vv().encode()
        );
        let Record::Peer(PeerFrame::Loro { message, .. }) = request
            .response(scope(), document.state_vv().encode())
            .unwrap()
            .unwrap()
        else {
            panic!("response")
        };
        assert!(
            matches!(encoding::try_decode(&message), Some(Message::JoinResponseOk { version, .. }) if version == document.state_vv().encode())
        );
    }
    assert!(metadata::decode(scope(), join(vec![255]), 1).is_err());
}

#[test]
fn metadata_capacity_scope_and_discovery_are_not_silently_relaxed() {
    assert!(matches!(
        metadata::decode(scope(), join(Vec::new()), 0),
        Err(Error::Unavailable)
    ));
    let PeerFrame::Loro { message, .. } = join(Vec::new()) else {
        unreachable!()
    };
    assert!(matches!(
        metadata::decode(
            scope(),
            PeerFrame::LoroBatch {
                workspace_id: ws(),
                messages: vec![message.clone(), message.clone()]
            },
            1
        ),
        Err(Error::Unavailable)
    ));
    let wrong = WorkspaceId::new();
    assert!(matches!(
        metadata::decode(
            scope(),
            PeerFrame::Loro {
                workspace_id: wrong,
                message
            },
            1
        ),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        metadata::decode(scope(), PeerFrame::CatalogRequest, 1),
        Err(Error::Unavailable)
    ));
    let Record::Peer(ack) = scope().committed_ack(unit(), [42; 8]).unwrap() else {
        unreachable!()
    };
    let values = metadata::decode(scope(), ack, 1).unwrap();
    assert!(
        matches!(values[0], Metadata::Ack { unit: u, batch: [42,42,42,42,42,42,42,42], ok: true } if u == unit())
    );
}

#[test]
fn owner_receives_metadata_without_payload_clones_and_original_deadlines_remain_visible() {
    let (mut sender, _) = setup(MAX_PAYLOAD);
    assert_eq!(sender.next_deadline(), CONTROL_WAIT_MS);
    sender.initial_policy(0).unwrap();
    sender
        .receive_control(Control::policy(1, MAX_PAYLOAD), 0, 0, |_| Ok(()))
        .unwrap();
    assert_eq!(sender.next_deadline(), sender.expires_at());
    let (admission, frame) = sender
        .receive_peer_owned(join(Vec::new()), 1, |_| Ok(()))
        .unwrap();
    assert_eq!(admission, Admission::Metadata);
    assert!(frame.is_some());
    sender
        .begin_outgoing(unit(), [1; 8], vec![0; 100], 2, 2, |_| Ok(()))
        .unwrap();
    assert_eq!(
        sender.outgoing_deadline(),
        Some(2 + budget_millis(100).unwrap())
    );
    assert_eq!(sender.next_deadline(), sender.outgoing_deadline().unwrap());
    assert_eq!(sender.check(sender.next_deadline()), Err(Error::Timeout));
}

// ------------------------------------- root frames on the carrier (3.1)

#[test]
fn root_and_personal_frames_are_admitted_by_permit_scope_and_refused_on_content_sessions() {
    use crate::protocol::{CatalogUnitRef, PersonalUnitState, RootUnitState};
    use metadata::{CarrierAdmission, RootMetadata, decode_root};

    let workspace = ws();
    let unit = CatalogUnitRef::Workspace { workspace };
    let empty = transfer::empty_version();
    let root_frames = [
        PeerFrame::RootCatalogRequest,
        PeerFrame::RootCatalog {
            digest: vec![1],
            units: vec![RootUnitState {
                unit,
                version: empty.clone(),
            }],
            personal: vec![PersonalUnitState {
                workspace,
                version: empty.clone(),
            }],
        },
        PeerFrame::RootUnitRequest {
            unit,
            version: Vec::new(),
        },
        PeerFrame::RootUnitUpdates {
            unit,
            updates: vec![vec![9]],
        },
    ];
    let personal_frames = [
        PeerFrame::PersonalRequest {
            workspace,
            version: empty.clone(),
        },
        PeerFrame::PersonalUpdates {
            workspace,
            updates: vec![vec![9]],
        },
    ];
    let refused = |result: Result<Option<RootMetadata>, transfer::Error>| {
        matches!(
            result,
            Err(transfer::Error::Authority(
                crate::cloud::Refusal::PrincipalNotAllowed
            ))
        )
    };
    // A Workspace dataset session and a Profile dataset session admit
    // neither family: the v1 decoder refuses them typed.
    let profile_scope = Scope {
        principal: CloudReplicaPrincipalV1::Profile {
            profile_id: id(5),
            lineage_id: id(6),
            logical_replica_id: id(7),
        },
        ..scope()
    };
    for frame in root_frames.iter().chain(personal_frames.iter()) {
        for session in [scope(), profile_scope] {
            let admission = CarrierAdmission::of(session.principal);
            assert!(!admission.admits_root_frames());
            assert!(!admission.admits_personal_frames());
            assert!(refused(decode_root(admission, frame)));
            assert!(
                matches!(
                    metadata::decode(session, frame.clone(), 4),
                    Err(transfer::Error::Authority(
                        crate::cloud::Refusal::PrincipalNotAllowed
                    ))
                ),
                "{frame:?}"
            );
        }
    }
    // A user-catalog permit admits root frames and not personal ones; a
    // user-personal permit the reverse.
    for frame in &root_frames {
        assert!(matches!(
            decode_root(CarrierAdmission::UserCatalog, frame),
            Ok(Some(_))
        ));
        assert!(refused(decode_root(CarrierAdmission::UserPersonal, frame)));
    }
    for frame in &personal_frames {
        assert!(matches!(
            decode_root(CarrierAdmission::UserPersonal, frame),
            Ok(Some(_))
        ));
        assert!(refused(decode_root(CarrierAdmission::UserCatalog, frame)));
    }
    assert!(matches!(
        decode_root(
            CarrierAdmission::UserCatalog,
            &PeerFrame::RootUnitRequest {
                unit,
                version: Vec::new()
            }
        ),
        Ok(Some(RootMetadata::RootUnitRequest { unit: got, version })) if got == unit && version.is_empty()
    ));
    // A content frame is not a root frame: `decode_root` leaves it alone.
    assert_eq!(
        decode_root(CarrierAdmission::UserCatalog, &PeerFrame::SyncRequest).unwrap(),
        None
    );
    // Malformed root frames are invalid, not refused: a version that is not
    // a version vector, an empty update list, an empty frame.
    assert!(matches!(
        decode_root(
            CarrierAdmission::UserCatalog,
            &PeerFrame::RootUnitRequest {
                unit,
                version: vec![0xff, 0xff, 0xff]
            }
        ),
        Err(transfer::Error::Invalid)
    ));
    assert!(matches!(
        decode_root(
            CarrierAdmission::UserCatalog,
            &PeerFrame::RootUnitUpdates {
                unit,
                updates: Vec::new()
            }
        ),
        Err(transfer::Error::Invalid)
    ));
    assert!(matches!(
        decode_root(
            CarrierAdmission::UserPersonal,
            &PeerFrame::PersonalUpdates {
                workspace,
                updates: vec![Vec::new()]
            }
        ),
        Err(transfer::Error::Invalid)
    ));
    // The peer-mesh content catalog is still unavailable on every Cloud
    // session: root admission did not open it.
    assert!(matches!(
        metadata::decode(scope(), PeerFrame::CatalogRequest, 4),
        Err(transfer::Error::Unavailable)
    ));
}
