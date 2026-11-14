// SPDX-License-Identifier: MPL-2.0
//! Actual fifth-route composition; scripted delegate receipts are not SQL authority.
use super::*;

#[derive(Default)]
pub(super) struct PreclaimPeer {
    mode: Option<&'static str>,
    pub(super) held: Option<Arc<Gate>>,
    pub(super) protect_draft: bool,
    admission: Option<Value>,
    global: Option<Value>,
    routine: Option<Value>,
    newer_global: Option<Value>,
}
fn reply(value: Value) -> BotReply {
    serde_json::from_value(value).unwrap()
}
impl Peer {
    pub(super) fn answer_preclaim(
        &self,
        command: &BotCommand,
        within: Duration,
    ) -> Option<Result<BotReply, koru_delegate_api::Failure>> {
        let phase = match command {
            BotCommand::ReadAdmissionBarrier { .. } => "hint",
            BotCommand::Barrier { .. } => "admission",
            BotCommand::SynchronizePolicy { .. } => "global",
            BotCommand::SynchronizeRoutinePolicy { .. } => "routine",
            _ => return None,
        };
        assert!(!within.is_zero() && within <= ROUTINE_BRIDGE_DEADLINE);
        self.log(phase);
        let (mode, held) = {
            let peer = self.preclaim.lock().unwrap();
            (
                peer.mode,
                if phase == "routine" {
                    peer.held.clone()
                } else {
                    None
                },
            )
        };
        // The gate intentionally outlives HTTP's deadline. It holds no peer
        // mutex; independent owner reads and worker cancellation still progress.
        if let Some(held) = &held {
            if !held.enter() {
                return Some(Err(koru_delegate_api::Failure::unavailable()));
            }
        }
        if mode == Some(phase) {
            return Some(Err(koru_delegate_api::Failure::unavailable()));
        }
        let answer = match command {
            BotCommand::ReadAdmissionBarrier { owner, bot } => {
                assert_eq!(*owner, self.seed.slot.owner);
                assert_eq!(*bot, self.seed.slot.bot);
                if mode == Some("generic_missing") {
                    return Some(Ok(BotReply::Missing));
                }
                let admission = self
                    .preclaim
                    .lock()
                    .unwrap()
                    .admission
                    .clone()
                    .map(|barrier| json!({"state":"present","barrier":barrier}))
                    .unwrap_or_else(|| json!({"state":"missing"}));
                reply(
                    json!({"result":"admission_barrier","owner":owner,"bot":bot,"admission":admission}),
                )
            }
            BotCommand::Barrier { barrier } => {
                assert_eq!(barrier.owner, self.seed.slot.owner);
                assert_eq!(barrier.bot, self.seed.slot.bot);
                assert!(barrier.epoch > 0 && !barrier.paused);
                self.preclaim.lock().unwrap().admission = Some(json!(barrier));
                let mut echo = json!(barrier);
                if mode == Some("wrong_admission") {
                    echo["epoch"] = json!(barrier.epoch + 1);
                }
                reply(json!({"result":"barrier","barrier":echo}))
            }
            BotCommand::SynchronizePolicy { barrier } => {
                assert_eq!(barrier.key.owner, self.seed.slot.owner);
                assert_eq!(barrier.key.bot, self.seed.slot.bot);
                assert_eq!(barrier.policy.workspace, self.seed.workspace);
                assert_eq!(
                    json!(barrier.config_revision),
                    self.preclaim.lock().unwrap().admission.as_ref().unwrap()["epoch"]
                );
                self.preclaim.lock().unwrap().global = Some(json!(barrier));
                if mode == Some("historical_effective") {
                    let mut newer = json!(barrier);
                    newer["config_revision"] = json!(barrier.config_revision + 1);
                    newer["policy_revision"] = json!(barrier.policy_revision + 1);
                    newer["key"]["request"] = json!(
                        RoutineBridgeEnvelope::new(self.seed.clone())
                            .unwrap()
                            .request_id
                    );
                    self.preclaim.lock().unwrap().newer_global = Some(newer);
                    // Simulate the real owner's ability to return an exact
                    // historical Effective receipt while retaining a newer row.
                }
                let mut echo = json!(barrier);
                if mode == Some("wrong_global") {
                    echo["policy_revision"] = json!(barrier.policy_revision + 1);
                }
                reply(json!({"result":"policy_barrier","receipt":{"barrier":echo,
                    "state":if mode==Some("pending_global") {"pending"} else {"effective"}}}))
            }
            BotCommand::SynchronizeRoutinePolicy { barrier } => {
                assert_eq!(barrier.owner, self.seed.slot.owner);
                assert_eq!(barrier.bot, self.seed.slot.bot);
                assert_eq!(barrier.schedule, self.seed.slot.schedule);
                assert_eq!(barrier.policy.workspace, self.seed.workspace);
                self.preclaim.lock().unwrap().routine = Some(json!(barrier));
                let mut echo = json!(barrier);
                if mode == Some("wrong_routine") {
                    echo["grant_revision"] = json!(barrier.grant_revision + 1);
                }
                reply(json!({"result":"routine_policy_barrier","barrier":echo,
                    "state":if mode==Some("pending_routine") {"pending"} else {"effective"}}))
            }
            _ => unreachable!(),
        };
        if let Some(held) = held {
            held.complete();
        }
        Some(Ok(answer))
    }
    fn policy_calls(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|call| matches!(*call, "hint" | "admission" | "global" | "routine"))
            .collect()
    }
    fn hold_policy(&self) -> AckHold {
        let held = Arc::new(Gate::default());
        let mut peer = self.preclaim.lock().unwrap();
        assert!(peer.held.is_none());
        peer.held = Some(held.clone());
        AckHold(held)
    }
}
fn prepared(
    h: &Harness,
) -> (
    RoutineBridgeEnvelope<SynchronizeRoutinePreclaim>,
    PrepareOccurrenceReply,
) {
    h.peer.preclaim.lock().unwrap().protect_draft = true;
    let p = h.prepare().payload;
    let PrepareOccurrenceReply::Prepared { preparation } = &p else {
        unreachable!()
    };
    (
        RoutineBridgeEnvelope::new(SynchronizeRoutinePreclaim {
            slot: preparation.slot,
            semantic_hash: preparation.semantic_hash,
            preparation_hash: preparation.preparation_hash,
        })
        .unwrap(),
        p,
    )
}
fn synchronize(
    h: &Harness,
    request: &RoutineBridgeEnvelope<SynchronizeRoutinePreclaim>,
) -> RoutineBridgeEnvelope<SynchronizeRoutinePreclaimReply> {
    let reply = h.server.client().synchronize_preclaim(request).unwrap();
    reply.validate_reply_to(request).unwrap();
    reply
}
fn refused(
    h: &Harness,
    request: &RoutineBridgeEnvelope<SynchronizeRoutinePreclaim>,
    code: RoutineBridgeRefusal,
) {
    assert_eq!(
        synchronize(h, request).payload,
        SynchronizeRoutinePreclaimReply::Refused { code }
    );
}

#[test]
fn routine_network_preclaim_ready_order_exact_replay_and_historical_receipt_boundary() {
    let h = Harness::new();
    let (request, original) = prepared(&h);
    let first = synchronize(&h, &request);
    let SynchronizeRoutinePreclaimReply::Ready {
        admission, proof, ..
    } = &first.payload
    else {
        panic!("ready required")
    };
    assert_eq!(
        h.peer.policy_calls(),
        ["hint", "admission", "global", "routine"]
    );
    {
        let peer = h.peer.preclaim.lock().unwrap();
        assert_eq!(json!(admission), *peer.admission.as_ref().unwrap());
        assert_eq!(json!(proof.global), *peer.global.as_ref().unwrap());
        assert_eq!(
            json!(proof.routine.as_ref().unwrap()),
            *peer.routine.as_ref().unwrap()
        );
    }
    assert_eq!(synchronize(&h, &request).payload, first.payload);
    h.peer.preclaim.lock().unwrap().mode = Some("historical_effective");
    let historical = synchronize(&h, &request);
    assert_eq!(historical.payload, first.payload);
    assert_ne!(
        h.peer
            .preclaim
            .lock()
            .unwrap()
            .newer_global
            .as_ref()
            .unwrap()["config_revision"],
        json!(proof.global.config_revision)
    );
    // Ready is synchronization evidence only; this fixture does not exercise
    // the native actor/SQL check that independently rejects stale latest proof.
    assert_eq!(h.prepare().payload, original);
    assert_eq!(h.fixture().capture_count(), 1);
    assert_eq!(h.fixture().catalog_reads(), 1);
    h.finish();
}

#[test]
fn routine_network_preclaim_missing_and_linked_are_stale_hash_mismatch_is_conflict() {
    let h = Harness::new();
    let missing = RoutineBridgeEnvelope::new(SynchronizeRoutinePreclaim {
        slot: h.seed.slot,
        semantic_hash: h.read_absent().payload.semantic_hash,
        preparation_hash: h.read_absent().payload.semantic_hash,
    })
    .unwrap();
    refused(&h, &missing, RoutineBridgeRefusal::Stale);
    assert!(h.peer.policy_calls().is_empty());
    let (request, original) = prepared(&h);
    for semantic in [false, true] {
        let mut wrong = request.payload.clone();
        if semantic {
            wrong.semantic_hash = missing.payload.semantic_hash;
        } else {
            wrong.preparation_hash = missing.payload.preparation_hash;
        }
        refused(
            &h,
            &RoutineBridgeEnvelope::new(wrong).unwrap(),
            RoutineBridgeRefusal::Conflict,
        );
    }
    assert!(h.peer.policy_calls().is_empty());
    let finalize = h.peer.claim(&original);
    assert!(matches!(
        h.server
            .client()
            .finalize(&RoutineBridgeEnvelope::new(finalize).unwrap())
            .unwrap()
            .payload,
        FinalizeOccurrenceReply::Linked { .. }
    ));
    refused(&h, &request, RoutineBridgeRefusal::Stale);
    assert!(h.peer.policy_calls().is_empty());
    assert_eq!(h.fixture().capture_count(), 1);
    h.finish();
}

#[test]
fn routine_network_preclaim_pending_mismatched_and_unknown_receipts_never_ready() {
    for (mode, code, phases) in [
        ("generic_missing", RoutineBridgeRefusal::Unavailable, 1),
        ("hint", RoutineBridgeRefusal::Unavailable, 1),
        ("wrong_admission", RoutineBridgeRefusal::Conflict, 2),
        ("pending_global", RoutineBridgeRefusal::Unavailable, 3),
        ("wrong_global", RoutineBridgeRefusal::Conflict, 3),
        ("global", RoutineBridgeRefusal::Unavailable, 3),
        ("pending_routine", RoutineBridgeRefusal::Unavailable, 4),
        ("wrong_routine", RoutineBridgeRefusal::Conflict, 4),
        ("routine", RoutineBridgeRefusal::Unavailable, 4),
    ] {
        let h = Harness::new();
        let (request, _) = prepared(&h);
        h.peer.preclaim.lock().unwrap().mode = Some(mode);
        refused(&h, &request, code);
        assert_eq!(
            h.peer.policy_calls(),
            ["hint", "admission", "global", "routine"][..phases],
            "{mode}"
        );
        assert_eq!(h.fixture().capture_count(), 1);
        h.finish();
    }
}

#[test]
fn routine_network_preclaim_timeout_retains_physical_capacity_and_immutable_draft() {
    let h = Harness::new();
    let (request, original) = prepared(&h);
    let held = h.peer.hold_policy();
    let completed = held.0.clone();
    let bytes = request.encode().unwrap();
    let addr = h.server.addr;
    let task = Task::start(move || {
        raw(
            addr,
            ROUTINE_PRECLAIM_ROUTE,
            &authority().authorization_header(),
            bytes.len(),
            &bytes,
        )
    });
    held.entered();
    let (status, body) = task.join();
    assert_eq!(status, 504);
    assert!(body.is_empty());
    let queued = h
        .fixture()
        .client()
        .synchronize_routine_preclaim(context(&h.seed), request.payload.clone())
        .unwrap();
    refused(&h, &request, RoutineBridgeRefusal::Capacity);
    let PrepareOccurrenceReply::Prepared { preparation } = &original else {
        unreachable!()
    };
    let read = RoutineBridgeEnvelope::new(ReadPreparation {
        slot: preparation.slot,
        semantic_hash: preparation.semantic_hash,
    })
    .unwrap();
    let observed = h.server.client().read_preparation(&read).unwrap();
    observed.validate_reply_to(&read).unwrap();
    assert_eq!(json!(observed.payload)["preparation"], json!(preparation));
    drop(queued);
    held.release();
    completed.wait_complete();
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        let reply = synchronize(&h, &request);
        if matches!(reply.payload, SynchronizeRoutinePreclaimReply::Ready { .. }) {
            break;
        }
        assert!(matches!(
            reply.payload,
            SynchronizeRoutinePreclaimReply::Refused {
                code: RoutineBridgeRefusal::Capacity | RoutineBridgeRefusal::Unavailable
            }
        ));
        retry_wait(end);
    }
    assert_eq!(h.prepare().payload, original);
    assert_eq!(h.fixture().capture_count(), 1);
    h.finish();
}

#[test]
fn routine_network_preclaim_backend_auth_and_body_bounds_are_truthful_and_finite() {
    let mut h = Harness::new();
    let (request, _) = prepared(&h);
    let before = (h.fixture().capture_count(), h.fixture().catalog_reads());
    let bytes = request.encode().unwrap();
    for (auth, length, body, expected) in [
        (
            "Bearer ordinary-session".to_owned(),
            bytes.len(),
            bytes.as_slice(),
            401,
        ),
        (
            authority().authorization_header(),
            ROUTINE_BRIDGE_BODY_LIMIT + 1,
            &[][..],
            413,
        ),
    ] {
        let (status, body) = raw(h.server.addr, ROUTINE_PRECLAIM_ROUTE, &auth, length, body);
        assert_eq!(status, expected);
        assert!(body.is_empty());
        assert!(h.peer.policy_calls().is_empty());
        assert_eq!(
            (h.fixture().capture_count(), h.fixture().catalog_reads()),
            before
        );
    }
    let mut exact = bytes;
    exact.resize(ROUTINE_BRIDGE_BODY_LIMIT, b' ');
    let (status, body) = raw(
        h.server.addr,
        ROUTINE_PRECLAIM_ROUTE,
        &authority().authorization_header(),
        exact.len(),
        &exact,
    );
    assert_eq!(status, 200);
    let reply = RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(&body).unwrap();
    reply.validate_reply_to(&request).unwrap();
    assert!(matches!(
        reply.payload,
        SynchronizeRoutinePreclaimReply::Ready { .. }
    ));
    let absent = Server::new(None);
    assert_eq!(
        absent
            .client()
            .synchronize_preclaim(&request)
            .unwrap()
            .payload,
        SynchronizeRoutinePreclaimReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
    );
    h.fixture.take().unwrap().finish().unwrap();
    refused(&h, &request, RoutineBridgeRefusal::Unavailable);
    drop(absent);
    drop(h);
}
