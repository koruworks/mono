// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_api_client::routine_bridge::AfterSend;

#[test]
fn routine_network_four_routes_use_real_preparation_and_exact_claim_link_identity() {
    let h = Harness::new();
    let client = h.server.client();
    let prepared = h.prepare();
    assert!(prepared.encode().unwrap().len() <= ROUTINE_BRIDGE_BODY_LIMIT);
    let PrepareOccurrenceReply::Prepared { preparation } = &prepared.payload else {
        unreachable!()
    };
    let read = RoutineBridgeEnvelope::new(ReadPreparation {
        slot: preparation.slot,
        semantic_hash: preparation.semantic_hash,
    })
    .unwrap();
    let observed = client.read_preparation(&read).unwrap();
    observed.validate_reply_to(&read).unwrap();
    assert_eq!(
        serde_json::to_value(&observed.payload).unwrap()["preparation"],
        serde_json::to_value(preparation).unwrap()
    );
    assert_eq!(h.prepare().payload, prepared.payload);
    assert_eq!(h.fixture().capture_count(), 1);
    assert_eq!(h.fixture().catalog_reads(), 1);
    let finalize = h.peer.claim(&prepared.payload);
    let request = RoutineBridgeEnvelope::new(finalize.clone()).unwrap();
    let linked = client.finalize(&request).unwrap();
    linked.validate_reply_to(&request).unwrap();
    assert_eq!(
        linked.payload,
        FinalizeOccurrenceReply::Linked {
            claim: finalize.claim.clone()
        }
    );
    let request = RoutineBridgeEnvelope::new(ReadOccurrenceLink {
        claim: finalize.claim.clone(),
    })
    .unwrap();
    let linked = client.read_link(&request).unwrap();
    linked.validate_reply_to(&request).unwrap();
    assert_eq!(
        linked.payload,
        ReadOccurrenceLinkReply::Linked {
            claim: finalize.claim.clone()
        }
    );
    assert_eq!(h.prepare().payload, prepared.payload);
    assert_eq!(h.fixture().capture_count(), 1);
    let repo = h.finish();
    let retained = repo
        .routine_preparation_for_claim(&finalize.claim)
        .unwrap()
        .unwrap();
    assert_eq!(retained.prepared.ids, preparation.ids);
    assert_eq!(retained.prepared.submission, preparation.submission);
}

#[test]
fn routine_network_absent_and_stopped_backend_never_claim_repository_missing() {
    let mut h = Harness::new();
    let request = h.read_absent();
    let absent = Server::new(None);
    assert_eq!(
        absent.client().read_preparation(&request).unwrap().payload,
        ReadPreparationReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
    );
    assert!(matches!(
        h.server
            .client()
            .read_preparation(&request)
            .unwrap()
            .payload,
        ReadPreparationReply::Missing { .. }
    ));
    assert_eq!(h.fixture().capture_count(), 0);
    let repo = h.fixture.take().unwrap().finish().unwrap();
    assert_eq!(
        h.server
            .client()
            .read_preparation(&request)
            .unwrap()
            .payload,
        ReadPreparationReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
    );
    assert!(
        repo.read_routine_preparation(&request.payload.slot, request.payload.semantic_hash)
            .unwrap()
            .is_none()
    );
    drop(absent);
    drop(h);
}

#[test]
fn routine_network_http_timeout_retains_physical_capacity_and_precommit_absence() {
    let h = Harness::new();
    let hold = h.fixture().hold_next_capture();
    let request = RoutineBridgeEnvelope::new(h.seed.clone()).unwrap();
    let encoded = request.encode().unwrap();
    let addr = h.server.addr;
    let task = Task::start(move || {
        raw(
            addr,
            ROUTINE_PREPARE_ROUTE,
            &authority().authorization_header(),
            encoded.len(),
            &encoded,
        )
    });
    assert!(hold.wait_entered(Duration::from_secs(4)));
    assert_eq!(h.fixture().capture_count(), 1);
    let (status, body) = task.join();
    assert_eq!(status, 504);
    assert!(body.is_empty());
    // The timed-out waiter owns no physical permit. The still-running capture
    // occupies one slot; an admitted queued operation occupies the other.
    let queued = h
        .fixture()
        .client()
        .prepare_routine_occurrence(context(&h.seed), h.seed.clone())
        .unwrap();
    let refused = h
        .server
        .client()
        .prepare(&RoutineBridgeEnvelope::new(h.seed.clone()).unwrap())
        .unwrap();
    assert_eq!(
        refused.payload,
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Capacity
        }
    );
    assert_eq!(h.fixture().capture_count(), 1);
    // Independent read lane progresses while real capture is still held.
    h.ready();
    drop(queued);
    hold.release();
    h.ready();
    let prepared = h.prepare();
    let PrepareOccurrenceReply::Prepared { preparation } = prepared.payload else {
        unreachable!()
    };
    assert_eq!(preparation.slot, h.seed.slot);
    assert_eq!(h.fixture().capture_count(), 2);
    let repo = h.finish();
    assert_eq!(
        repo.read_routine_preparation(&preparation.slot, preparation.semantic_hash)
            .unwrap()
            .unwrap()
            .prepared
            .ids,
        preparation.ids
    );
}

#[test]
fn routine_network_client_timeout_is_unknown_and_retries_only_the_same_semantics() {
    let h = Harness::new();
    let hold = h.fixture().hold_next_capture();
    let client = h.server.client();
    let request = RoutineBridgeEnvelope::new(h.seed.clone()).unwrap();
    let task = Task::start(move || client.prepare(&request));
    assert!(hold.wait_entered(Duration::from_secs(4)));
    let result = task.join();
    assert!(
        matches!(
            result,
            Err(RoutineBridgeTransportError::Unknown(
                AfterSend::Deadline | AfterSend::UnexpectedStatus(504)
            ))
        ),
        "{result:?}"
    );
    // No draft committed while its capture phase was held. This positive owner
    // read is distinct from treating the earlier transport failure as Missing.
    h.ready();
    hold.release();
    let prepared = h.prepare();
    let PrepareOccurrenceReply::Prepared { preparation } = prepared.payload else {
        unreachable!()
    };
    assert_eq!(preparation.slot, h.seed.slot);
    assert_eq!(h.fixture().capture_count(), 2);
    let repo = h.finish();
    assert_eq!(
        repo.read_routine_preparation(&preparation.slot, preparation.semantic_hash)
            .unwrap()
            .unwrap()
            .prepared,
        preparation
    );
}

#[test]
fn routine_network_postcommit_timeout_keeps_exact_ids_and_allows_independent_reads() {
    let h = Harness::new();
    let prepared = h.prepare();
    let PrepareOccurrenceReply::Prepared { preparation } = &prepared.payload else {
        unreachable!()
    };
    let finalize = h.peer.claim(&prepared.payload);
    let hold = h.peer.hold_ack();
    let completed = hold.0.clone();
    let encoded = RoutineBridgeEnvelope::new(finalize.clone())
        .unwrap()
        .encode()
        .unwrap();
    let addr = h.server.addr;
    let task = Task::start(move || {
        raw(
            addr,
            ROUTINE_FINALIZE_ROUTE,
            &authority().authorization_header(),
            encoded.len(),
            &encoded,
        )
    });
    hold.entered();
    // The writer committed local materialization before asking this independent
    // peer to acknowledge. Its gate holds no peer state or Store mutex.
    let read = RoutineBridgeEnvelope::new(ReadPreparation {
        slot: preparation.slot,
        semantic_hash: preparation.semantic_hash,
    })
    .unwrap();
    let observed = h.server.client().read_preparation(&read).unwrap();
    assert_eq!(
        observed.payload,
        ReadPreparationReply::Linked {
            slot: preparation.slot,
            semantic_hash: preparation.semantic_hash,
            claim: finalize.claim.clone()
        }
    );
    let (status, body) = task.join();
    assert_eq!(status, 504);
    assert!(body.is_empty());
    hold.release();
    completed.wait_complete();
    let request = RoutineBridgeEnvelope::new(ReadOccurrenceLink {
        claim: finalize.claim.clone(),
    })
    .unwrap();
    let linked = h.server.client().read_link(&request).unwrap();
    assert_eq!(
        linked.payload,
        ReadOccurrenceLinkReply::Linked {
            claim: finalize.claim.clone()
        }
    );
    assert_eq!(h.prepare().payload, prepared.payload);
    assert_eq!(h.fixture().capture_count(), 1);
    let repo = h.finish();
    let retained = repo
        .routine_preparation_for_claim(&finalize.claim)
        .unwrap()
        .unwrap();
    assert_eq!(retained.prepared.ids, preparation.ids);
}

#[test]
fn routine_network_auth_and_body_bounds_refuse_before_model_or_capture_effects() {
    let h = Harness::new();
    let bytes = RoutineBridgeEnvelope::new(h.seed.clone())
        .unwrap()
        .encode()
        .unwrap();
    let (status, body) = raw(
        h.server.addr,
        ROUTINE_PREPARE_ROUTE,
        "Bearer ordinary-session",
        bytes.len(),
        &bytes,
    );
    assert_eq!(status, 401);
    assert!(body.is_empty());
    let (status, body) = raw(
        h.server.addr,
        ROUTINE_PREPARE_ROUTE,
        &authority().authorization_header(),
        ROUTINE_BRIDGE_BODY_LIMIT + 1,
        &[],
    );
    assert_eq!(status, 413);
    assert!(body.is_empty());
    assert_eq!(h.fixture().capture_count(), 0);
    assert_eq!(h.fixture().catalog_reads(), 0);
    let request = h.read_absent();
    let mut encoded = request.encode().unwrap();
    encoded.resize(ROUTINE_BRIDGE_BODY_LIMIT, b' ');
    let (status, body) = raw(
        h.server.addr,
        ROUTINE_PREPARATION_ROUTE,
        &authority().authorization_header(),
        encoded.len(),
        &encoded,
    );
    assert_eq!(status, 200);
    let reply = RoutineBridgeEnvelope::<ReadPreparationReply>::decode(&body).unwrap();
    reply.validate_reply_to(&request).unwrap();
    assert!(matches!(
        reply.payload,
        ReadPreparationReply::Missing { .. }
    ));
    assert_eq!(h.fixture().capture_count(), 0);
    assert_eq!(h.fixture().catalog_reads(), 0);
    h.finish();
}
