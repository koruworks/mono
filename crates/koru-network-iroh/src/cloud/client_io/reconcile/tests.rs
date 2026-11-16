// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn room_interest_negotiates_and_leaves_on_the_same_stream() {
    let (client, mut reader, mut writer, _, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let mut sync = Reconciler::new(
        client,
        |_| Ok(async { Ok(Answer::Ready(VersionVector::default())) }),
        |_| Ok(async { Ok(Answer::Ready(None)) }),
    );
    let unit = Unit::Document {
        workspace_id: koru_device::cloud::CloudId::from_bytes(
            scope.eligibility_workspace.as_uuid().into_bytes(),
        ),
        document_id: koru_device::cloud::CloudId::from_bytes(
            koru_store_core::DocumentId::new().as_uuid().into_bytes(),
        ),
    };
    sync.rooms([unit]).unwrap();
    sync.start(unit).unwrap();
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        let request = records::read(&mut reader, deadline, &budget).await.unwrap();
        let Record::Peer(frame) = request.record() else {
            panic!("expected join")
        };
        let mut messages = metadata::decode(scope, frame.clone(), 4).unwrap();
        let Metadata::Request(request) = messages.remove(0) else {
            panic!("expected join")
        };
        assert!(request.live);
        assert_eq!(request.unit, unit);
        let response = request
            .response(scope, VersionVector::default().encode())
            .unwrap()
            .unwrap();
        records::write(&mut writer, &response, deadline, &budget)
            .await
            .unwrap();
        let leave = records::read(&mut reader, deadline, &budget).await.unwrap();
        let Record::Peer(frame) = leave.record() else {
            panic!("expected leave")
        };
        assert!(
            matches!(metadata::decode(scope, frame.clone(), 4).unwrap().as_slice(),
            [Metadata::Leave(left)] if *left == unit)
        );
    };
    let device = async {
        loop {
            if matches!(sync.next().await.unwrap(), Event::Settled { .. }) {
                break;
            }
        }
        sync.rooms([]).unwrap();
        // Drive the retained writer until the peer observes Leave. No second
        // connection, manifest inventory or simulated durable receipt.
        loop {
            sync.next().await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! { () = peer => {}, () = device => unreachable!() }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_comparison_page_checks_many_documents_without_loro_jobs() {
    use koru_replication::protocol::{CLOUD_COMPARE_LIMIT, CloudDocumentDigest, PeerFrame};
    let (client, mut reader, mut writer, _, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let mut sync = Reconciler::new(
        client,
        |_| -> Result<std::future::Ready<Result<Answer<VersionVector>, Error>>, Error> {
            panic!("inventory must not read document content")
        },
        |_| -> Result<std::future::Ready<Result<Answer<Option<Export>>, Error>>, Error> {
            panic!("inventory must not export document content")
        },
    );
    let documents = (0..CLOUD_COMPARE_LIMIT)
        .map(|_| CloudDocumentDigest {
            document_id: koru_store_core::DocumentId::new(),
            digest: [7; 32],
        })
        .collect::<Vec<_>>();
    let expected = documents
        .iter()
        .take(CLOUD_COMPARE_LIMIT - 1)
        .map(|d| d.document_id)
        .collect::<std::collections::HashSet<_>>();
    let response = expected.iter().copied().collect::<Vec<_>>();
    let peer = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let request = super::super::records::read(&mut reader, deadline, &budget)
            .await
            .unwrap();
        assert!(
            matches!(request.record(), super::super::Record::Peer(PeerFrame::CloudCompare { documents, .. }) if documents.len() == CLOUD_COMPARE_LIMIT)
        );
        super::super::records::write(
            &mut writer,
            &super::super::Record::Peer(PeerFrame::SyncRequest),
            deadline,
            &budget,
        )
        .await
        .unwrap();
        super::super::records::write(
            &mut writer,
            &super::super::Record::Peer(PeerFrame::CloudCompared {
                workspace_id: scope.eligibility_workspace,
                matching: response,
            }),
            deadline,
            &budget,
        )
        .await
        .unwrap();
    };
    let (answer, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(sync.compare_documents(documents), peer)
    })
    .await
    .unwrap();
    let (matching, hinted) = answer.unwrap();
    assert_eq!(matching, expected);
    assert!(
        hinted,
        "a concurrent remote edit must survive inventory comparison"
    );
}

#[tokio::test]
async fn a_room_push_can_be_deferred_during_comparison_without_losing_the_reply() {
    use koru_device::cloud::CloudId;
    use koru_replication::protocol::{CloudDocumentDigest, PeerFrame};
    let (client, mut reader, mut writer, mut stream, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let mut sync = Reconciler::new(
        client,
        |_| Ok(async { Ok(Answer::Ready(VersionVector::default())) }),
        |_| Ok(async { Ok(Answer::Ready(None)) }),
    );
    let document_id = koru_store_core::DocumentId::new();
    let peer = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        let request = records::read(&mut reader, deadline, &budget).await.unwrap();
        assert!(matches!(
            request.record(),
            Record::Peer(PeerFrame::CloudCompare { .. })
        ));
        let unit = Unit::Document {
            workspace_id: CloudId::from_bytes(scope.eligibility_workspace.as_uuid().into_bytes()),
            document_id: CloudId::from_bytes(document_id.as_uuid().into_bytes()),
        };
        let (start, _) = stream
            .begin_outgoing(
                unit,
                [8; 8],
                vec![1; transfer::RESTRICTED_LIMIT + 1],
                0,
                0,
                |_| Ok(()),
            )
            .unwrap();
        records::write(&mut writer, &Record::Control(start), deadline, &budget)
            .await
            .unwrap();
        records::write(
            &mut writer,
            &Record::Peer(PeerFrame::CloudCompared {
                workspace_id: scope.eligibility_workspace,
                matching: vec![document_id],
            }),
            deadline,
            &budget,
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(
        sync.compare_documents(vec![CloudDocumentDigest {
            document_id,
            digest: [3; 32]
        }]),
        peer
    );
    assert!(result.unwrap().0.contains(&document_id));
    assert!(sync.status().transport.transfer.closed.is_none());
}

#[tokio::test]
async fn download_during_retained_verification_is_rechecked_after_original_read() {
    let (client, _reader, _writer, _, scope, budget) = super::super::tests::pair().await;
    let remote = loro::LoroDoc::new();
    remote
        .get_map("root")
        .insert("value", "downloaded")
        .unwrap();
    remote.commit();
    let version = remote.oplog_vv();
    let fresh = version.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    let mut sync = Reconciler::new(
        client,
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            let version = fresh.clone();
            Ok(async move { Ok(Answer::Ready(version)) })
        },
        |_| Ok(async { Ok(Answer::Ready(None)) }),
    );
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    sync.pass = Some(Pass {
        unit,
        step: Step::Verify(Box::pin(async move {
            receive.await.unwrap();
            Ok(Answer::Ready(VersionVector::default()))
        })),
        remote: Some(Arc::new(Remote {
            version,
            _messages: Messages {
                values: vec![],
                _charge: budget.reserve(1).unwrap(),
            },
        })),
        upload_deferred: false,
        download_deferred: false,
        verify_again: false,
        uploaded_version: None,
        upload_acked: false,
        local_version: None,
    });
    sync.transport_event(super::super::Event::Downloaded(unit))
        .unwrap();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "do not replace an unfinished Store read"
    );
    assert_eq!(sync.status().phase, Phase::Verifying);
    send.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match sync.next().await.unwrap() {
                Event::Settled {
                    unit: completed,
                    local_changed,
                    ..
                } => {
                    assert_eq!(completed, unit);
                    assert!(!local_changed);
                    break;
                }
                _ => assert_ne!(
                    sync.status().phase,
                    Phase::AwaitingDownload,
                    "the already consumed download cannot be awaited again"
                ),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_edit_after_the_acknowledged_export_is_not_claimed_as_synced() {
    let (client, _reader, _writer, _, scope, budget) = super::super::tests::pair().await;
    let doc = loro::LoroDoc::new();
    doc.get_map("root").insert("value", "uploaded").unwrap();
    doc.commit();
    let acknowledged = doc.oplog_vv();
    doc.get_map("root").insert("value", "edited again").unwrap();
    doc.commit();
    let current = doc.oplog_vv();
    let mut sync = Reconciler::new(
        client,
        |_| Ok(async { Ok(Answer::Ready(VersionVector::default())) }),
        |_| Ok(async { Ok(Answer::Ready(None)) }),
    );
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    sync.pass = Some(Pass {
        unit,
        step: Step::Verify(Box::pin(async move { Ok(Answer::Ready(current)) })),
        remote: Some(Arc::new(Remote {
            version: acknowledged.clone(),
            _messages: Messages {
                values: vec![],
                _charge: budget.reserve(1).unwrap(),
            },
        })),
        upload_deferred: false,
        download_deferred: false,
        verify_again: false,
        uploaded_version: Some(acknowledged),
        upload_acked: true,
        local_version: None,
    });
    let event = tokio::time::timeout(Duration::from_secs(1), sync.next())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        Event::Settled {
            local_changed: true,
            ..
        }
    ));
}

#[tokio::test]
async fn a_creation_the_worker_refuses_ends_the_pass_with_its_own_code() {
    // The worker refuses an app connection's creation of a unit Cloud already
    // holds with `sync_unit_exists` (Cloud MCP C23), after the whole upload
    // and before it writes anything. The decoder maps the code to itself, so
    // the host can report the effect as not saved; before the amendment it
    // decoded as the unknown-refusal fallback, `handshake_malformed`.
    an_upload_the_worker_refuses_ends_the_pass_with(Error::UnitExists).await;
}

#[tokio::test]
async fn a_claimed_upload_the_worker_refuses_ends_the_pass_with_its_own_code() {
    // The worker refuses an app connection's upload of a document someone
    // else holds a claim on, or under a reference that is not the claim's,
    // with `sync_document_claimed` (Cloud MCP §5.7, DS-D45): before
    // `reserve`, or by rolling its publish back, so the head did not move.
    // The decoder maps it to itself, beside `sync_unit_exists`.
    an_upload_the_worker_refuses_ends_the_pass_with(Error::DocumentClaimed).await;
}

/// One upload at its pass's upload step, refused by the worker with
/// `refusal`'s code after the whole upload arrived.
async fn an_upload_the_worker_refuses_ends_the_pass_with(refusal: Error) {
    use koru_replication::protocol::PeerFrame;
    let (client, mut reader, mut writer, mut stream, scope, budget) =
        super::super::tests::pair_with_workspace(true).await;
    let mut sync = Reconciler::new(
        client,
        |_| Ok(async { Ok(Answer::Ready(VersionVector::default())) }),
        |_| Ok(async { Ok(Answer::Ready(None)) }),
    );
    let workspace_id =
        koru_device::cloud::CloudId::from_bytes(scope.eligibility_workspace.as_uuid().into_bytes());
    let unit = Unit::Document {
        workspace_id,
        document_id: koru_device::cloud::CloudId::from_bytes(
            koru_store_core::DocumentId::new().as_uuid().into_bytes(),
        ),
    };
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, "a second seed").unwrap();
    doc.commit();
    let seed = doc.export(loro::ExportMode::Snapshot).unwrap();
    // The pass at its upload, as `advance` leaves it after a join that
    // answered the empty version.
    sync.pass = Some(Pass {
        unit,
        step: Step::Upload,
        remote: Some(Arc::new(Remote {
            version: VersionVector::default(),
            _messages: Messages {
                values: vec![],
                _charge: budget.reserve(1).unwrap(),
            },
        })),
        upload_deferred: false,
        download_deferred: false,
        verify_again: false,
        uploaded_version: Some(doc.oplog_vv()),
        upload_acked: false,
        local_version: None,
    });
    sync.client.upload(unit, seed.clone(), ()).unwrap();
    let worker = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        let complete = loop {
            let received = records::read(&mut reader, deadline, &budget).await.unwrap();
            match received.into_parts().1 {
                Record::Control(control) => {
                    stream.receive_control(control, 0, 0, |_| Ok(())).unwrap();
                }
                Record::Peer(frame) => {
                    stream.receive_peer(frame, 0, |_| Ok(())).unwrap();
                }
            }
            if stream.status().incoming_complete {
                break stream.handoff(0, |_| Ok(()), |v| v).unwrap();
            }
        };
        assert_eq!(complete.bytes, seed);
        records::write(
            &mut writer,
            &Record::Peer(PeerFrame::Error {
                code: refusal.code().into(),
            }),
            deadline,
            &budget,
        )
        .await
        .unwrap();
    };
    let connection = async {
        loop {
            if let Err(error) = sync.next().await {
                break error;
            }
        }
    };
    let (refused, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(connection, worker)
    })
    .await
    .unwrap();
    assert_eq!(refused, refusal);
    // The pass is gone and the session ended with that code: nothing of
    // this upload is retained, and no later unit starts on it.
    assert_eq!(sync.status().phase, Phase::Idle);
    assert_eq!(sync.start(unit).err(), Some(refusal));
    drop(sync);
    assert_eq!(budget.used(), 0);
}

#[tokio::test]
async fn every_session_sharing_the_holds_hears_a_release_and_a_resource_hold_hears_none() {
    use koru_account_core::cloud_plan::CloudAllowance;
    // A Profile session and its authority Workspace's session are handed the
    // same holds. One stored permit woke only whichever asked first.
    let now = 1_700_000_000_000;
    let holds = UploadAdmissionHolds::with_clock(move || now);
    let (profile, workspace) = (holds.session(), holds.session());
    let unit = Unit::Identity {
        profile_id: koru_device::cloud::CloudId::from_bytes([1; 16]),
    };
    let hold = |reason| HeldUpload {
        head: VersionVector::default(),
        hold: Hold::unobserved(reason, None, now),
    };
    let observation = |usage_revision| Observation {
        binding: koru_account_core::cloud_admission::Binding {
            owner: uuid::Uuid::from_u128(1),
            workspace: uuid::Uuid::from_u128(2),
            incarnation: uuid::Uuid::from_u128(3),
            account_generation: 1,
            authority_generation: 1,
        },
        usage_revision,
        policy_revision: 1,
        generation: Some(uuid::Uuid::from_u128(4)),
        observed_at_millis: now,
        valid_until_millis: now + 60_000,
        allowance: CloudAllowance::Unbounded,
        read: true,
        ordinary_write: true,
        remove: true,
        complete_usage: true,
        used_bytes: Some(0),
        reserved_bytes: Some(0),
    };
    holds.state().held.insert(unit, hold(Reason::AccountQuota));
    holds.observe(observation(1));
    tokio::time::timeout(Duration::from_secs(1), async {
        profile.changed().await;
        workspace.changed().await;
    })
    .await
    .expect("both sessions wake");

    // Logical usage says nothing about a resource: only its cycle retries it.
    holds.state().held.insert(unit, hold(Reason::PhysicalBytes));
    holds.observe(observation(2));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), workspace.changed())
            .await
            .is_err()
    );
    // A session that ended is forgotten, not notified forever.
    drop(profile);
    let _renewed = holds.session();
    assert_eq!(holds.state().wakes.len(), 2);
}
