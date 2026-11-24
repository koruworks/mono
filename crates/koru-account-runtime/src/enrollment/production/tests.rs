// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::identity::SqliteTrustRepository;
use base64::Engine as _;
use koru_account_client::{HttpRequest, HttpResponse, Transport, TransportError};
use koru_account_session::{DeviceAttestationClaimsV1, DeviceAttestationKeyRing};
use koru_device::TrustRepository;
use std::sync::atomic::AtomicUsize;

mod provisioning;
mod readiness;

const NOW: i64 = 1_001_000;
const EXPIRES: i64 = 1_120_000;

#[test]
fn retry_proof_is_scoped_to_its_session_profile_device_and_lifetime() {
    let fixture = Fixture::new();
    let authority = EnrollmentAuthorization {
        access: koru_account_wire::AccessToken("fixture".into()),
        owner: crate::identity::AccountOwner {
            account: fixture.attempt.verified.claims().koru_identity_id,
            auth_identity: "50000000-0000-4000-8000-000000000001".parse().unwrap(),
        },
        namespace: "fixture".into(),
        record_version: 1,
        session_id: fixture.attempt.verified.claims().session_id.to_string(),
    };
    let usable = |authority: &EnrollmentAuthorization, profile, local: &DeviceDescriptorV1, now| {
        fixture.attempt.usable_for(profile, authority, local, now)
    };
    let issued_at = fixture.attempt.verified.claims().issued_at_millis;
    assert!(usable(
        &authority,
        fixture.profile,
        &fixture.joining,
        issued_at - 1_300
    ));
    assert!(usable(
        &authority,
        fixture.profile,
        &fixture.joining,
        issued_at - 30_000
    ));
    assert!(!usable(
        &authority,
        fixture.profile,
        &fixture.joining,
        issued_at - 30_001
    ));
    assert!(usable(
        &authority,
        fixture.profile,
        &fixture.joining,
        EXPIRES - 1
    ));
    assert!(!usable(
        &authority,
        fixture.profile,
        &fixture.joining,
        EXPIRES
    ));
    assert!(!usable(&authority, fixture.profile, &fixture.joining, 0));
    let mut next_session = authority.clone();
    next_session.session_id = "40000000-0000-4000-8000-000000000002".into();
    assert!(!usable(
        &next_session,
        fixture.profile,
        &fixture.joining,
        NOW
    ));
    let mut next_owner = authority.clone();
    next_owner.owner.account = "30000000-0000-4000-8000-000000000002".parse().unwrap();
    assert!(!usable(&next_owner, fixture.profile, &fixture.joining, NOW));
    assert!(!usable(&authority, ProfileId::new(), &fixture.joining, NOW));
    assert!(!usable(
        &authority,
        fixture.profile,
        &fixture.responder.local,
        NOW
    ));
    let mut renamed = fixture.joining.clone();
    renamed.display_name = "Renamed".into();
    assert!(usable(&authority, fixture.profile, &renamed, NOW));
}

#[test]
fn hosted_conflict_retries_but_denied_profile_access_is_terminal() {
    let error = |status, code| ClientError::Api {
        status,
        code: koru_account_wire::ErrorCode::parse(code).unwrap(),
    };
    assert_eq!(
        map_client(error(409, koru_account_wire::ErrorCode::CONFLICT)),
        Failure::Unavailable
    );
    assert_eq!(
        map_client(error(403, koru_account_wire::ErrorCode::PERMISSION_DENIED)),
        Failure::AccessRevoked
    );
}
struct Time;
impl Clock for Time {
    fn now_millis(&self) -> Option<i64> {
        Some(NOW)
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct Inventory {
    profile: ProfileId,
    owner: koru_account_core::KoruIdentityId,
}
impl LocalProfileInventory for Inventory {
    fn inventory(
        &self,
    ) -> Result<koru_store_wire::ProfileLifecycleInventory, crate::account::ServiceError> {
        Ok(koru_store_wire::ProfileLifecycleInventory {
            profiles: vec![koru_store_wire::ProfileLifecycleInventoryItem {
                profile_id: self.profile,
                account_binding: format!("koru:{}", self.owner),
                removed_at_millis: None,
            }],
        })
    }
}
struct Hosted {
    jwks: JwksResponseV1,
    receipts: Mutex<Vec<ProvisionDeviceRequestV1>>,
}
impl Transport for Hosted {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = if request.path_and_query.ends_with("jwks.json")
            || request.path_and_query.ends_with("/jwks")
        {
            serde_json::to_vec(&self.jwks).unwrap()
        } else if request.path_and_query.ends_with("/provisioning") {
            let value: ProvisionDeviceRequestV1 = serde_json::from_slice(&request.body).unwrap();
            let mut receipts = self.receipts.lock().unwrap();
            if let Some(previous) = receipts.first() {
                assert_eq!(value, *previous);
            }
            let result = if receipts.is_empty() {
                koru_account_wire::ProvisioningResultV1::Provisioned
            } else {
                koru_account_wire::ProvisioningResultV1::ExactRetry
            };
            receipts.push(value);
            serde_json::to_vec(&koru_account_wire::ProvisionDeviceResponseV1 {
                version: Version1,
                result,
            })
            .unwrap()
        } else {
            panic!(
                "unexpected injected enrollment hosted route: {}",
                request.path_and_query
            );
        };
        Ok(HttpResponse {
            document_removal: false,
            status: 200,
            body,
        })
    }
}
struct MemoryInbound(Mutex<SqliteTrustRepository>);
impl InboundRepository for MemoryInbound {
    fn replay(
        &self,
        request: ReplayRequest,
    ) -> repository::RepositoryFuture<'_, Option<PreparedEnrollmentV1>> {
        Box::pin(async move {
            let value = self
                .0
                .lock()
                .unwrap()
                .enrollment_by_jti(&request.jti)
                .map_err(|_| refused("trust_persistence_failed"))?;
            request.validate(value)
        })
    }
    fn persist(
        &self,
        prepared: PreparedEnrollmentV1,
        verified: VerifiedDeviceAttestationV1,
        local: koru_device::DeviceId,
        session: Arc<ResponderSessionGate>,
        ticket: ResponderTicket,
    ) -> repository::RepositoryFuture<'_, (u64, bool)> {
        Box::pin(async move {
            session.commit(ticket, || {
                match self
                    .0
                    .lock()
                    .unwrap()
                    .commit_enrollment(&prepared, &verified, local)
                    .map_err(|_| refused("trust_persistence_failed"))?
                {
                    ApplyOutcome::Applied { effective_revision } => Ok((effective_revision, true)),
                    ApplyOutcome::AlreadyApplied { effective_revision } => {
                        Ok((effective_revision, false))
                    }
                    _ => Err(refused("trust_persistence_failed")),
                }
            })
        })
    }
}
struct Stream {
    remote: ReplicationEndpointId,
    send: tokio::sync::mpsc::Sender<Result<EnrollmentFrameV1, EnrollmentTransportError>>,
    receive: tokio::sync::mpsc::Receiver<Result<EnrollmentFrameV1, EnrollmentTransportError>>,
    sent: Arc<Mutex<Vec<EnrollmentFrameV1>>>,
    lose_complete: bool,
}
impl CeremonySession for Stream {
    fn remote_endpoint(&self) -> ReplicationEndpointId {
        self.remote
    }
    fn send_frame<'a>(&'a mut self, frame: &'a EnrollmentFrameV1) -> io::IoFuture<'a, ()> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(frame.clone());
            if self.lose_complete && matches!(frame, EnrollmentFrameV1::Complete { .. }) {
                self.send
                    .send(Err(EnrollmentTransportError::StreamFailed))
                    .await
                    .unwrap();
                return Err(EnrollmentTransportError::StreamFailed);
            }
            self.send
                .send(Ok(frame.clone()))
                .await
                .map_err(|_| EnrollmentTransportError::StreamFailed)
        })
    }
    fn receive_frame(&mut self) -> io::IoFuture<'_, EnrollmentFrameV1> {
        Box::pin(async move {
            self.receive
                .recv()
                .await
                .unwrap_or(Err(EnrollmentTransportError::StreamFailed))
        })
    }
}
fn streams(
    existing: ReplicationEndpointId,
    joining: ReplicationEndpointId,
    lose_complete: bool,
) -> (Stream, Stream) {
    let (to_existing, from_joining) = tokio::sync::mpsc::channel(4);
    let (to_joining, from_existing) = tokio::sync::mpsc::channel(4);
    (
        Stream {
            remote: existing,
            send: to_existing,
            receive: from_existing,
            sent: Arc::default(),
            lose_complete: false,
        },
        Stream {
            remote: joining,
            send: to_joining,
            receive: from_joining,
            sent: Arc::default(),
            lose_complete,
        },
    )
}
struct Fixture {
    responder: ProductionResponder,
    inbound: MemoryInbound,
    joining_repository: SqliteTrustRepository,
    joining: DeviceDescriptorV1,
    joining_key: DeviceSigningKey,
    profile: ProfileId,
    candidate: EnrollmentCandidateV1,
    attempt: ProductionAttempt,
    durable: Arc<AtomicUsize>,
    hosted: Arc<Hosted>,
}
impl Fixture {
    fn prepared(&self) -> PreparedEnrollmentV1 {
        let transcript = EnrollmentTranscriptV1::new(
            &self.attempt.verified,
            self.profile,
            self.responder.local.clone(),
            self.joining.clone(),
            self.attempt.joining_challenge,
        )
        .unwrap();
        EnrollmentJoinV1::sign(
            self.attempt.evidence.clone(),
            &self.attempt.verified,
            transcript,
            &self.joining_key,
            NOW,
        )
        .unwrap()
        .complete(
            &self.attempt.verified,
            &DeviceSigningKey::from_secret_bytes([41; 32]),
        )
        .unwrap()
    }

    fn new() -> Self {
        let key = DeviceSigningKey::from_secret_bytes([41; 32]);
        let joining_key = DeviceSigningKey::from_secret_bytes([42; 32]);
        let existing = DeviceDescriptorV1::new(
            koru_device::DeviceId::parse("10000000-0000-4000-8000-000000000001").unwrap(),
            "Existing".into(),
            &key,
            koru_device::ReplicationEndpointSecret::from_secret_bytes([51; 32]).endpoint_id(),
            1,
        );
        let joining = DeviceDescriptorV1::new(
            koru_device::DeviceId::parse("10000000-0000-4000-8000-000000000002").unwrap(),
            "Joining".into(),
            &joining_key,
            koru_device::ReplicationEndpointSecret::from_secret_bytes([52; 32]).endpoint_id(),
            1,
        );
        let profile = ProfileId::parse("20000000-0000-4000-8000-000000000001").unwrap();
        let owner = "30000000-0000-4000-8000-000000000001".parse().unwrap();
        let signer = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let public = signer.verifying_key();
        let ring = DeviceAttestationKeyRing::new("test".into(), [("test".into(), signer)]).unwrap();
        let evidence = ring
            .sign(&DeviceAttestationClaimsV1 {
                version: koru_account_session::DEVICE_ATTESTATION_VERSION,
                issuer: "https://account.invalid".into(),
                audience: koru_account_session::DEVICE_ATTESTATION_AUDIENCE.into(),
                koru_identity_id: owner,
                device_id: joining.device_id,
                signing_public_key: joining.signing_public_key,
                endpoint_id: joining.endpoint_id,
                authorized_profile_ids: vec![profile],
                session_id: "40000000-0000-4000-8000-000000000001".parse().unwrap(),
                token_id: "exact-live-attempt".into(),
                issued_at_millis: 1_000_000,
                expires_at_millis: EXPIRES,
            })
            .unwrap();
        let verified = koru_account_session::verify_device_attestation(
            &evidence,
            &ring.verification_keys(),
            "https://account.invalid",
            NOW,
        )
        .unwrap();
        let hosted = Arc::new(Hosted {
            jwks: JwksResponseV1 {
                version: Version1,
                keys: vec![],
                attestation_keys: vec![koru_account_wire::JwkV1 {
                    kty: "OKP".into(),
                    crv: "Ed25519".into(),
                    key_use: "sig".into(),
                    alg: "EdDSA".into(),
                    kid: "test".into(),
                    x: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.as_bytes()),
                }],
                capability_keys: vec![],
            },
            receipts: Mutex::new(vec![]),
        });
        let gate = Arc::new(ResponderSessionGate::new());
        gate.enable_account(owner, "40000000-0000-4000-8000-000000000002".into());
        let durable = Arc::new(AtomicUsize::new(0));
        let notify = durable.clone();
        let responder = ProductionResponder::new(
            AccountClient::with_transport(hosted.clone()),
            "https://account.invalid".into(),
            Arc::new(Inventory { profile, owner }),
            Arc::new(Time),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/unused-injected-enrollment.db"),
            existing.clone(),
            [41; 32],
            gate,
            move |_| {
                notify.fetch_add(1, Ordering::SeqCst);
            },
        );
        let mut inbound = SqliteTrustRepository::in_memory().unwrap();
        inbound.initialize(&existing).unwrap();
        let mut joining_repository = SqliteTrustRepository::in_memory().unwrap();
        joining_repository.initialize(&joining).unwrap();
        let candidate = EnrollmentCandidateV1 {
            version: Version1,
            device_id: existing.device_id,
            signing_public_key: existing.signing_public_key.as_bytes().to_vec(),
            endpoint_id: existing.endpoint_id.as_bytes().to_vec(),
            platform: koru_account_core::DevicePlatform::Windows,
            app_version: "test".into(),
            enrollment_protocol_version: ENROLLMENT_VERSION,
            peer_protocol_version: PEER_PROTOCOL_VERSION,
        };
        Self {
            responder,
            inbound: MemoryInbound(Mutex::new(inbound)),
            joining_repository,
            joining,
            joining_key,
            profile,
            candidate,
            attempt: ProductionAttempt {
                evidence,
                verified,
                joining_challenge: [71; 32],
            },
            durable,
            hosted,
        }
    }
}

struct MutableTime(std::sync::atomic::AtomicI64);
impl Clock for MutableTime {
    fn now_millis(&self) -> Option<i64> {
        Some(self.0.load(Ordering::SeqCst))
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct Scripted {
    remote: ReplicationEndpointId,
    incoming: std::collections::VecDeque<EnrollmentFrameV1>,
    sent: Vec<EnrollmentFrameV1>,
    expire_on_receive: Option<(usize, Arc<MutableTime>)>,
    received: usize,
    fail_join: bool,
}
impl Scripted {
    fn complete(f: &Fixture, prepared: PreparedEnrollmentV1) -> Self {
        Self {
            remote: f.responder.local.endpoint_id,
            incoming: [EnrollmentFrameV1::Complete {
                prepared: Box::new(prepared),
            }]
            .into(),
            sent: vec![],
            expire_on_receive: None,
            received: 0,
            fail_join: false,
        }
    }
}
impl CeremonySession for Scripted {
    fn remote_endpoint(&self) -> ReplicationEndpointId {
        self.remote
    }
    fn send_frame<'a>(&'a mut self, frame: &'a EnrollmentFrameV1) -> io::IoFuture<'a, ()> {
        Box::pin(async move {
            self.sent.push(frame.clone());
            if self.fail_join && matches!(frame, EnrollmentFrameV1::Join { .. }) {
                Err(EnrollmentTransportError::StreamFailed)
            } else {
                Ok(())
            }
        })
    }
    fn receive_frame(&mut self) -> io::IoFuture<'_, EnrollmentFrameV1> {
        Box::pin(async move {
            self.received += 1;
            if let Some((at, clock)) = &self.expire_on_receive {
                if *at == self.received {
                    clock.0.store(EXPIRES, Ordering::SeqCst);
                }
            }
            self.incoming
                .pop_front()
                .ok_or(EnrollmentTransportError::StreamFailed)
        })
    }
}

#[test]
fn saved_complete_rejects_substituted_scope_evidence_devices_endpoint_and_challenges() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for case in 0..11 {
        let mut prepared = f.prepared();
        let mut candidate = f.candidate.clone();
        let mut local = f.joining.clone();
        let mut attempt = f.attempt.clone();
        let mut profile = f.profile;
        let mut remote = f.responder.local.endpoint_id;
        match case {
            0 => candidate.device_id = f.joining.device_id,
            1 => candidate.signing_public_key = f.joining.signing_public_key.as_bytes().to_vec(),
            2 => candidate.endpoint_id = f.joining.endpoint_id.as_bytes().to_vec(),
            3 => local = f.responder.local.clone(),
            4 => attempt.joining_challenge[0] ^= 1,
            5 => remote = f.joining.endpoint_id,
            6 => profile = ProfileId::parse("20000000-0000-4000-8000-000000000099").unwrap(),
            7 => attempt.evidence.compact.push('x'),
            8 => prepared.transcript.existing_challenge[0] ^= 1,
            9 => prepared.joining_proof = prepared.existing_proof.clone(),
            10 => prepared.attestation.compact.push('x'),
            _ => unreachable!(),
        }
        let mut session = Scripted::complete(&f, prepared);
        session.remote = remote;
        let result = runtime.block_on(run_initiator_on(
            &mut session,
            local,
            &f.joining_key,
            profile,
            candidate,
            attempt,
            &AtomicBool::new(false),
            &Time,
        ));
        assert!(
            matches!(result, Err(Failure::BadAttestation)),
            "substitution case {case}: {result:?}"
        );
        assert_eq!(session.sent.len(), 1, "no Join on a saved completion");
        assert!(
            f.joining_repository
                .completed_enrollment(f.profile, f.joining.device_id)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn both_normal_and_saved_complete_recheck_expiry_after_receive() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for normal in [false, true] {
        let prepared = f.prepared();
        let mut stream = Scripted::complete(&f, prepared.clone());
        if normal {
            stream.incoming.push_front(EnrollmentFrameV1::Challenge {
                transcript: prepared.transcript,
            });
        }
        let clock = Arc::new(MutableTime(std::sync::atomic::AtomicI64::new(NOW)));
        stream.expire_on_receive = Some((if normal { 2 } else { 1 }, clock.clone()));
        let result = runtime.block_on(run_initiator_on(
            &mut stream,
            f.joining.clone(),
            &f.joining_key,
            f.profile,
            f.candidate.clone(),
            f.attempt.clone(),
            &AtomicBool::new(false),
            clock.as_ref(),
        ));
        assert!(
            matches!(result, Err(Failure::BadAttestation)),
            "normal={normal}: {result:?}"
        );
        assert_eq!(stream.sent.len(), if normal { 2 } else { 1 });
    }
}

#[test]
fn normal_complete_accepts_exact_challenge_but_refuses_another_valid_ceremony() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for substitute in [false, true] {
        let expected = f.prepared();
        let returned = if substitute {
            f.prepared()
        } else {
            expected.clone()
        };
        let mut stream = Scripted::complete(&f, returned);
        stream.incoming.push_front(EnrollmentFrameV1::Challenge {
            transcript: expected.transcript.clone(),
        });
        let result = runtime.block_on(run_initiator_on(
            &mut stream,
            f.joining.clone(),
            &f.joining_key,
            f.profile,
            f.candidate.clone(),
            f.attempt.clone(),
            &AtomicBool::new(false),
            &Time,
        ));
        if substitute {
            assert!(matches!(result, Err(Failure::BadAttestation)));
        } else {
            assert_eq!(result.unwrap(), expected);
        }
        assert_eq!(stream.sent.len(), 2);
    }
}

#[test]
fn nonconclusive_remote_error_after_join_retains_uncertain_attempt() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for (code, expected) in [
        ("enrollment_unavailable", Failure::Uncertain),
        ("unknown_error", Failure::Uncertain),
        ("trust_persistence_failed", Failure::Persistence),
        ("attestation_invalid", Failure::BadAttestation),
        ("session_revoked", Failure::SessionExpired),
        ("profile_access_revoked", Failure::AccessRevoked),
    ] {
        let mut stream = Scripted::complete(&f, f.prepared());
        stream.incoming = [
            EnrollmentFrameV1::Challenge {
                transcript: f.prepared().transcript,
            },
            EnrollmentFrameV1::Error { code: code.into() },
        ]
        .into();
        let result = runtime.block_on(run_initiator_on(
            &mut stream,
            f.joining.clone(),
            &f.joining_key,
            f.profile,
            f.candidate.clone(),
            f.attempt.clone(),
            &AtomicBool::new(false),
            &Time,
        ));
        assert!(
            matches!(result, Err(error) if error == expected),
            "{code}: {result:?}"
        );
        assert_eq!(stream.sent.len(), 2);
    }
}

#[test]
fn refusal_before_begin_and_uncertain_join_write_are_distinct() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for case in 0..4 {
        let mut stream = Scripted::complete(&f, f.prepared());
        let mut candidate = f.candidate.clone();
        let clock = MutableTime(std::sync::atomic::AtomicI64::new(NOW));
        match case {
            0 => clock.0.store(EXPIRES, Ordering::SeqCst),
            1 => candidate.signing_public_key.clear(),
            2 => candidate.endpoint_id.clear(),
            3 => candidate.peer_protocol_version += 1,
            _ => unreachable!(),
        }
        assert!(
            runtime
                .block_on(run_initiator_on(
                    &mut stream,
                    f.joining.clone(),
                    &f.joining_key,
                    f.profile,
                    candidate,
                    f.attempt.clone(),
                    &AtomicBool::new(false),
                    &clock
                ))
                .is_err()
        );
        assert!(stream.sent.is_empty());
    }
    let prepared = f.prepared();
    let mut stream = Scripted::complete(&f, prepared.clone());
    stream.incoming.push_front(EnrollmentFrameV1::Challenge {
        transcript: prepared.transcript,
    });
    stream.fail_join = true;
    assert!(matches!(
        runtime.block_on(run_initiator_on(
            &mut stream,
            f.joining.clone(),
            &f.joining_key,
            f.profile,
            f.candidate.clone(),
            f.attempt.clone(),
            &AtomicBool::new(false),
            &Time
        )),
        Err(Failure::Uncertain)
    ));
}

#[test]
fn responder_saved_completion_refuses_changed_begin_challenge_without_new_commit() {
    let f = Fixture::new();
    let prepared = f.prepared();
    f.inbound
        .0
        .lock()
        .unwrap()
        .commit_enrollment(&prepared, &f.attempt.verified, f.responder.local.device_id)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut stream = Scripted::complete(&f, prepared.clone());
    stream.remote = f.joining.endpoint_id;
    stream.incoming = [EnrollmentFrameV1::Begin {
        attestation: f.attempt.evidence.clone(),
        profile_id: f.profile,
        existing_device_id: f.responder.local.device_id,
        joining_device: f.joining.clone(),
        joining_challenge: [99; 32],
    }]
    .into();
    assert!(
        runtime
            .block_on(f.responder.respond_on(&mut stream, &f.inbound))
            .is_err()
    );
    assert!(stream.sent.is_empty());
    assert_eq!(f.durable.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.inbound
            .0
            .lock()
            .unwrap()
            .enrollment_by_jti(&f.attempt.verified.claims().token_id)
            .unwrap(),
        Some(prepared)
    );
}

#[test]
fn live_responder_commit_lost_complete_exact_begin_retry_completes_without_second_ceremony() {
    let mut f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let cancel = AtomicBool::new(false);
    let (mut joining, mut existing) =
        streams(f.responder.local.endpoint_id, f.joining.endpoint_id, true);
    let (first, responder) = runtime.block_on(async {
        tokio::join!(
            run_initiator_on(
                &mut joining,
                f.joining.clone(),
                &f.joining_key,
                f.profile,
                f.candidate.clone(),
                f.attempt.clone(),
                &cancel,
                &Time
            ),
            f.responder.respond_on(&mut existing, &f.inbound)
        )
    });
    assert!(
        matches!(first, Err(Failure::Uncertain)),
        "lost Complete: {first:?}"
    );
    assert!(responder.is_err());
    assert_eq!(f.durable.load(Ordering::SeqCst), 1);
    assert!(
        f.joining_repository
            .completed_enrollment(f.profile, f.joining.device_id)
            .unwrap()
            .is_none()
    );
    let original_begin = joining.sent.lock().unwrap()[0].clone();
    // An intervening early transport failure still uses the exact attempt;
    // the scheduler owns the sticky uncertainty latch between port calls.
    let (mut offline, peer) = streams(f.responder.local.endpoint_id, f.joining.endpoint_id, false);
    drop(peer);
    assert!(matches!(
        runtime.block_on(run_initiator_on(
            &mut offline,
            f.joining.clone(),
            &f.joining_key,
            f.profile,
            f.candidate.clone(),
            f.attempt.clone(),
            &cancel,
            &Time
        )),
        Err(Failure::Offline)
    ));
    assert_eq!(
        offline.sent.lock().unwrap().as_slice(),
        &[original_begin.clone()]
    );
    let saved = f
        .inbound
        .0
        .lock()
        .unwrap()
        .enrollment_by_jti(&f.attempt.verified.claims().token_id)
        .unwrap()
        .unwrap();
    let (mut joining, mut existing) =
        streams(f.responder.local.endpoint_id, f.joining.endpoint_id, false);
    let (retried, responder) = runtime.block_on(async {
        tokio::join!(
            run_initiator_on(
                &mut joining,
                f.joining.clone(),
                &f.joining_key,
                f.profile,
                f.candidate.clone(),
                f.attempt.clone(),
                &cancel,
                &Time
            ),
            f.responder.respond_on(&mut existing, &f.inbound)
        )
    });
    responder.unwrap();
    let prepared =
        retried.expect("an exact Begin retry must accept the responder's saved Complete");
    assert_eq!(prepared, saved);
    assert_eq!(joining.sent.lock().unwrap().as_slice(), &[original_begin]);
    assert_eq!(existing.sent.lock().unwrap().len(), 1);
    assert_eq!(f.durable.load(Ordering::SeqCst), 1);
    let revision = match f
        .joining_repository
        .commit_enrollment(&prepared, &f.attempt.verified, f.joining.device_id)
        .unwrap()
    {
        ApplyOutcome::Applied { effective_revision } => effective_revision,
        other => panic!("first local commit: {other:?}"),
    };
    assert_eq!(
        f.joining_repository
            .commit_enrollment(&prepared, &f.attempt.verified, f.joining.device_id)
            .unwrap(),
        ApplyOutcome::AlreadyApplied {
            effective_revision: revision
        }
    );
    assert_eq!(
        f.joining_repository
            .completed_enrollment(f.profile, f.joining.device_id)
            .unwrap()
            .unwrap()
            .0,
        prepared
    );
    let access = koru_account_wire::AccessToken("injected-access".into());
    let authorization = AccountAuthorization::Bearer(&access);
    for _ in 0..2 {
        provision_completion(
            &f.responder.client,
            f.profile,
            f.joining.device_id,
            &saved,
            &authorization,
        )
        .unwrap();
    }
    let receipts = f.hosted.receipts.lock().unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(receipts[0].attestation.compact, saved.attestation.compact);
    assert_eq!(
        receipts[0].completion_receipt.0,
        saved.encode_receipt().unwrap()
    );
}
