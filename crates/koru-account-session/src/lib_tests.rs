// SPDX-License-Identifier: MPL-2.0

use super::*;
mod cloud_rate;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use koru_account_core::*;
use koru_device::{
    DeviceDescriptorV1, DevicePublicKey, DeviceSigningKey, ReplicationEndpointId,
    ReplicationEndpointSecret,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicI64, Ordering},
};

struct FakeClock(AtomicI64);
impl Clock for FakeClock {
    fn now_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct FakeEntropy(Mutex<u8>);
impl Entropy for FakeEntropy {
    fn fill(&self, out: &mut [u8]) -> Result<(), CryptoError> {
        let mut n = self.0.lock().unwrap();
        for b in out.iter_mut() {
            *b = *n;
            *n = n.wrapping_add(1)
        }
        Ok(())
    }
}
struct FakeCrypto;
impl TransientEncryption for FakeCrypto {
    fn seal(&self, p: &'static str, v: &[u8]) -> Result<SealedValue, CryptoError> {
        let mut o = p.as_bytes().to_vec();
        o.push(0);
        o.extend(v);
        Ok(SealedValue(o))
    }
    fn open(&self, p: &'static str, v: &SealedValue) -> Result<Vec<u8>, CryptoError> {
        let prefix = [p.as_bytes(), &[0]].concat();
        v.0.strip_prefix(&prefix[..])
            .map(|v| v.to_vec())
            .ok_or(CryptoError::VerificationFailed)
    }
    fn digest(&self, p: &'static str, v: &[u8]) -> Result<KeyedDigest, CryptoError> {
        let mut h = Sha256::new();
        h.update(b"test-key");
        h.update(p);
        h.update(v);
        Ok(KeyedDigest(h.finalize().to_vec()))
    }
}
struct FailingSigner;
impl TokenSigner for FailingSigner {
    fn sign_access_token(&self, _: &AccessTokenClaims) -> Result<SignedToken, CryptoError> {
        Err(CryptoError::KeyUnavailable)
    }

    fn public_jwks(&self) -> Result<String, CryptoError> {
        Err(CryptoError::KeyUnavailable)
    }
}
#[derive(Default)]
struct Sessions(Mutex<HashMap<AccountSessionId, AccountSession>>);
impl SessionRepository for Sessions {
    fn find_session<'a>(
        &'a self,
        id: AccountSessionId,
    ) -> PortFuture<'a, Result<Option<AccountSession>, RepositoryError>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().get(&id).cloned()) })
    }
    fn insert_session<'a>(
        &'a self,
        s: AccountSession,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            self.0.lock().unwrap().insert(s.id, s);
            Ok(())
        })
    }
    fn rotate_refresh<'a>(
        &'a self,
        s: AccountSession,
        p: KeyedDigest,
        retry_key: KeyedDigest,
        now: i64,
    ) -> PortFuture<'a, Result<RefreshRotationOutcome, RepositoryError>> {
        Box::pin(async move {
            let mut all = self.0.lock().unwrap();
            let old = all.get_mut(&s.id).ok_or(RepositoryError::NotFound)?;
            let family = old
                .refresh_family
                .as_ref()
                .ok_or(RepositoryError::Corrupt)?;
            if old.revoked_at_millis.is_none()
                && (now >= old.idle_expires_at_millis || now >= old.absolute_expires_at_millis)
            {
                old.revoked_at_millis = Some(now);
                return Ok(RefreshRotationOutcome::ExpiredRevoked);
            }
            if old.revoked_at_millis.is_none() && family.current_digest == p {
                *old = s;
                return Ok(RefreshRotationOutcome::Rotated(old.clone()));
            }
            if old.revoked_at_millis.is_none()
                && family.previous_digest.as_ref() == Some(&p)
                && family.previous_retry_key_digest.as_ref() == Some(&retry_key)
                && family
                    .previous_retry_until_millis
                    .is_some_and(|until| now < until)
            {
                return Ok(RefreshRotationOutcome::ExactRetry(old.clone()));
            }
            old.revoked_at_millis = Some(now);
            Ok(RefreshRotationOutcome::ReplayedRevoked)
        })
    }
    fn revoke_session<'a>(
        &'a self,
        id: AccountSessionId,
        at: i64,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut all = self.0.lock().unwrap();
            let s = all.get_mut(&id).ok_or(RepositoryError::NotFound)?;
            s.revoked_at_millis = Some(at);
            Ok(())
        })
    }
}
#[derive(Default)]
struct Flows(Mutex<HashMap<AuthFlowId, TemporaryAuthFlow>>);
impl AuthFlowRepository for Flows {
    fn insert_flow<'a>(
        &'a self,
        f: TemporaryAuthFlow,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            self.0.lock().unwrap().insert(f.id, f);
            Ok(())
        })
    }
    fn find_flow<'a>(
        &'a self,
        id: AuthFlowId,
    ) -> PortFuture<'a, Result<Option<TemporaryAuthFlow>, RepositoryError>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().get(&id).cloned()) })
    }
    fn consume_flow<'a>(
        &'a self,
        id: AuthFlowId,
        now: i64,
    ) -> PortFuture<'a, Result<TemporaryAuthFlow, RepositoryError>> {
        Box::pin(async move {
            let mut all = self.0.lock().unwrap();
            let f = all.get_mut(&id).ok_or(RepositoryError::NotFound)?;
            if f.status != AuthFlowStatus::Pending || now >= f.expires_at_millis {
                return Err(RepositoryError::Conflict);
            }
            f.status = AuthFlowStatus::Consumed;
            f.consumed_at_millis = Some(now);
            Ok(f.clone())
        })
    }
}
#[derive(Default)]
struct Limits(Mutex<HashMap<Vec<u8>, (i64, u32)>>);
impl RateLimiter for Limits {
    fn check<'a>(
        &'a self,
        _: RateLimitScope,
        d: KeyedDigest,
        _: Option<KoruIdentityId>,
        now: i64,
        limit: u32,
        window: u64,
    ) -> PortFuture<'a, Result<RateLimitDecision, RepositoryError>> {
        Box::pin(async move {
            let mut m = self.0.lock().unwrap();
            let e = m.entry(d.0).or_insert((now, 0));
            if now - e.0 >= window as i64 {
                *e = (now, 0)
            }
            e.1 += 1;
            Ok(RateLimitDecision {
                allowed: e.1 <= limit,
                remaining: limit.saturating_sub(e.1),
                retry_after_millis: (e.1 > limit).then_some(window),
            })
        })
    }
}

struct Harness {
    clock: FakeClock,
    entropy: FakeEntropy,
    sessions: Sessions,
    flows: Flows,
    crypto: FakeCrypto,
    signer: Ed25519KeyRing,
    limits: Limits,
}
impl Harness {
    fn new() -> Self {
        Self {
            clock: FakeClock(AtomicI64::new(1_000_000)),
            entropy: FakeEntropy(Mutex::new(1)),
            sessions: Sessions::default(),
            flows: Flows::default(),
            crypto: FakeCrypto,
            signer: Ed25519KeyRing::new(
                "new".into(),
                [
                    ("old".into(), SigningKey::from_bytes(&[7; 32])),
                    ("new".into(), SigningKey::from_bytes(&[8; 32])),
                ],
            )
            .unwrap(),
            limits: Limits::default(),
        }
    }
    fn service(&self) -> SessionSecurity<'_> {
        SessionSecurity {
            issuer: "https://api.koru.do",
            audience: "koru",
            clock: &self.clock,
            entropy: &self.entropy,
            sessions: &self.sessions,
            flows: &self.flows,
            crypto: &self.crypto,
            signer: &self.signer,
            limiter: &self.limits,
        }
    }
}

fn identity() -> KoruIdentityId {
    KoruIdentityId::from_str("10000000-0000-4000-8000-000000000001").unwrap()
}

#[test]
fn device_ownership_is_fresh_and_bound_to_account_flow_descriptor_and_install_facts() {
    let harness = Harness::new();
    let security = harness.service();
    let key = DeviceSigningKey::from_secret_bytes([17; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([18; 32]);
    let device = DeviceRegistration {
        descriptor: DeviceDescriptorV1::new(
            device_id(),
            "private device label".into(),
            &key,
            endpoint.endpoint_id(),
            1,
        ),
        platform: DevicePlatform::Windows,
        app_version: "private build".into(),
        enrollment_protocol_version: Some(1),
        peer_protocol_version: Some(2),
        ownership_proof: None,
    }
    .account_device(identity(), harness.clock.now_millis());
    let parent = Some(flow_id());
    let challenge = pollster::block_on(security.issue_device_ownership_challenge(
        device.clone(),
        parent,
        b"source",
    ))
    .unwrap();
    let proof = challenge.clone().sign(&key);
    assert!(pollster::block_on(security.verify_device_ownership(&proof, &device, parent)).is_ok());
    let wrong_key = DeviceSigningKey::from_secret_bytes([19; 32]);
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(
            &challenge.clone().sign(&wrong_key),
            &device,
            parent
        )),
        Err(SecurityError::InvalidSecret)
    );
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(&proof, &device, None)),
        Err(SecurityError::InvalidSecret)
    );
    let mut other = device.clone();
    other.registration.koru_identity_id = "10000000-0000-4000-8000-000000000009".parse().unwrap();
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(&proof, &other, parent)),
        Err(SecurityError::InvalidSecret)
    );
    other = device.clone();
    other.registration.peer_protocol_version = Some(3);
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(&proof, &other, parent)),
        Err(SecurityError::InvalidSecret)
    );
    let mut changed = proof.clone();
    changed.challenge.device.registration.app_version = "modified".into();
    assert!(!changed.verify_signature());
    changed = proof.clone();
    changed.challenge.nonce[0] ^= 1;
    assert!(!changed.verify_signature());
    changed = proof.clone();
    changed.challenge.device.descriptor.display_name = "modified".into();
    assert!(!changed.verify_signature());
    let second = pollster::block_on(security.issue_device_ownership_challenge(
        device.clone(),
        parent,
        b"source",
    ))
    .unwrap();
    changed = proof.clone();
    changed.challenge.flow_id = second.flow_id;
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(&changed, &device, parent)),
        Err(SecurityError::InvalidSecret)
    );
    for debug in [
        format!("{challenge:?}"),
        format!("{proof:?}"),
        format!(
            "{:?}",
            pollster::block_on(security.verify_device_ownership(&proof, &device, parent)).unwrap()
        ),
    ] {
        assert!(!debug.contains("private"));
        assert!(!debug.contains(&identity().to_string()));
    }
    harness
        .clock
        .0
        .store(challenge.expires_at_millis, Ordering::SeqCst);
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(&proof, &device, parent)),
        Err(SecurityError::Expired)
    );
    harness
        .clock
        .0
        .store(challenge.expires_at_millis - 1, Ordering::SeqCst);
    pollster::block_on(
        harness
            .flows
            .consume_flow(challenge.flow_id, harness.clock.now_millis()),
    )
    .unwrap();
    assert_eq!(
        pollster::block_on(security.verify_device_ownership(&proof, &device, parent)),
        Err(SecurityError::Replayed)
    );
}
fn auth_identity() -> AuthIdentityId {
    AuthIdentityId::from_str("20000000-0000-4000-8000-000000000002").unwrap()
}
fn session_id() -> AccountSessionId {
    AccountSessionId::from_str("30000000-0000-4000-8000-000000000003").unwrap()
}
fn flow_id() -> AuthFlowId {
    AuthFlowId::from_str("50000000-0000-4000-8000-000000000005").unwrap()
}
fn device_id() -> DeviceId {
    DeviceId::parse("40000000-0000-4000-8000-000000000004").unwrap()
}

fn profile_id() -> ProfileId {
    ProfileId::parse("60000000-0000-4000-8000-000000000006").unwrap()
}

fn attestation_claims(now: i64) -> DeviceAttestationClaimsV1 {
    DeviceAttestationClaimsV1 {
        version: DEVICE_ATTESTATION_VERSION,
        issuer: "https://api.koru.do".into(),
        audience: DEVICE_ATTESTATION_AUDIENCE.into(),
        koru_identity_id: identity(),
        device_id: device_id(),
        signing_public_key: DevicePublicKey::from_bytes([21; 32]),
        endpoint_id: ReplicationEndpointId::from_bytes([22; 32]),
        authorized_profile_ids: vec![profile_id()],
        session_id: session_id(),
        token_id: "attestation-jti".into(),
        issued_at_millis: now,
        expires_at_millis: now + DEVICE_ATTESTATION_MAX_MILLIS,
    }
}

#[test]
fn device_attestations_have_a_distinct_type_audience_and_verifier() {
    let now = 1_000_000;
    let ring = DeviceAttestationKeyRing::new(
        "attestation-key".into(),
        [("attestation-key".into(), SigningKey::from_bytes(&[31; 32]))],
    )
    .unwrap();
    let signed = ring.sign(&attestation_claims(now)).unwrap();
    let verified = verify_device_attestation(
        &signed,
        &ring.verification_keys(),
        "https://api.koru.do",
        now,
    )
    .unwrap();
    assert_eq!(verified.claims().device_id, device_id());
    assert_eq!(verified.claims().authorized_profile_ids, vec![profile_id()]);
    assert_eq!(verified.digest(), signed.digest());
    assert_eq!(
        verify_access_token(
            signed.compact.as_bytes(),
            &ring.verification_keys(),
            "https://api.koru.do",
            DEVICE_ATTESTATION_AUDIENCE,
            now,
        ),
        Err(JwtVerificationError::Claims)
    );

    assert_eq!(
        verify_device_attestation(
            &signed,
            &ring.verification_keys(),
            "https://wrong.invalid",
            now,
        ),
        Err(JwtVerificationError::Claims)
    );
    let segments = signed.compact.split('.').collect::<Vec<_>>();
    let mut payload: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segments[1])
            .unwrap(),
    )
    .unwrap();
    payload["aud"] = serde_json::Value::String("koru".into());
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    let input = format!("{}.{}", segments[0], payload);
    let signature = SigningKey::from_bytes(&[31; 32]).sign(input.as_bytes());
    let wrong_audience = SignedDeviceAttestationV1 {
        compact: format!(
            "{input}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ),
    };
    assert_eq!(
        verify_device_attestation(
            &wrong_audience,
            &ring.verification_keys(),
            "https://api.koru.do",
            now,
        ),
        Err(JwtVerificationError::Claims)
    );
    assert_eq!(
        verify_device_attestation(
            &signed,
            &ring.verification_keys(),
            "https://api.koru.do",
            now + DEVICE_ATTESTATION_MAX_MILLIS,
        ),
        Err(JwtVerificationError::Claims)
    );
    let wrong_ring = DeviceAttestationKeyRing::new(
        "attestation-key".into(),
        [("attestation-key".into(), SigningKey::from_bytes(&[32; 32]))],
    )
    .unwrap();
    assert_eq!(
        verify_device_attestation(
            &signed,
            &wrong_ring.verification_keys(),
            "https://api.koru.do",
            now,
        ),
        Err(JwtVerificationError::Signature)
    );

    let access_ring = Ed25519KeyRing::new(
        "attestation-key".into(),
        [("attestation-key".into(), SigningKey::from_bytes(&[31; 32]))],
    )
    .unwrap();
    let access = access_ring
        .sign_access_token(&AccessTokenClaims {
            issuer: "https://api.koru.do".into(),
            audience: DEVICE_ATTESTATION_AUDIENCE.into(),
            subject: identity(),
            session_id: session_id(),
            token_id: "access-jti".into(),
            issued_at_millis: now,
            expires_at_millis: now + ACCESS_TOKEN_MILLIS,
        })
        .unwrap();
    let substituted = SignedDeviceAttestationV1 {
        compact: String::from_utf8(access.compact.expose_secret().to_vec()).unwrap(),
    };
    assert_eq!(
        verify_device_attestation(
            &substituted,
            &ring.verification_keys(),
            "https://api.koru.do",
            now,
        ),
        Err(JwtVerificationError::Claims)
    );
}

#[test]
fn attestation_issue_skew_is_bounded_and_never_extends_expiry() {
    let issued_at = 1_000_000;
    let ring = DeviceAttestationKeyRing::new(
        "clock-test".into(),
        [("clock-test".into(), SigningKey::from_bytes(&[36; 32]))],
    )
    .unwrap();
    let claims = attestation_claims(issued_at);
    let signed = ring.sign(&claims).unwrap();
    let keys = ring.verification_keys();
    for offset in [0, -1_300, -30_000, DEVICE_ATTESTATION_MAX_MILLIS - 1] {
        let now = issued_at + offset;
        let verified = verify_device_attestation(&signed, &keys, &claims.issuer, now).unwrap();
        assert!(verified.is_current_at(now));
        assert!(!verified.is_current_at(issued_at - 30_001));
        assert!(!verified.is_current_at(claims.expires_at_millis));
    }
    for now in [
        issued_at - 30_001,
        claims.expires_at_millis,
        claims.expires_at_millis + 1,
        i64::MIN,
        i64::MAX,
    ] {
        assert_eq!(
            verify_device_attestation(&signed, &keys, &claims.issuer, now),
            Err(JwtVerificationError::Claims)
        );
    }
    // Historical receipt inspection is non-authorizing and keeps the same
    // bounded future-issue rule; its explicit expired-replay policy is intact.
    assert!(
        inspect_device_attestation(&signed, &keys, &claims.issuer, claims.expires_at_millis)
            .is_ok()
    );
    assert_eq!(
        inspect_device_attestation(&signed, &keys, &claims.issuer, issued_at - 30_001),
        Err(JwtVerificationError::Claims)
    );
}

#[test]
fn expired_attestation_inspection_is_non_authorizing_and_all_debug_is_structural() {
    let now = 1_000_000;
    let ring = DeviceAttestationKeyRing::new(
        "attestation-key".into(),
        [("attestation-key".into(), SigningKey::from_bytes(&[71; 32]))],
    )
    .unwrap();
    let signed = ring.sign(&attestation_claims(now)).unwrap();
    let expired_at = now + DEVICE_ATTESTATION_MAX_MILLIS;
    assert_eq!(
        verify_device_attestation(
            &signed,
            &ring.verification_keys(),
            "https://api.koru.do",
            expired_at,
        ),
        Err(JwtVerificationError::Claims)
    );
    let inspected = inspect_device_attestation(
        &signed,
        &ring.verification_keys(),
        "https://api.koru.do",
        expired_at,
    )
    .unwrap();
    assert_eq!(inspected.digest(), signed.digest());
    assert_eq!(inspected.claims().token_id, "attestation-jti");

    let claims = inspected.claims().clone();
    let output = format!("{claims:?} {signed:?} {inspected:?}");
    let identity = identity().to_string();
    let device = device_id().to_string();
    let profile = profile_id().to_string();
    let session = session_id().to_string();
    for forbidden in [
        signed.compact.as_str(),
        "attestation-jti",
        &identity,
        &device,
        &profile,
        &session,
    ] {
        assert!(!output.contains(forbidden));
    }
    assert!(output.contains("signed-evidence"));
    assert!(output.contains("InspectedDeviceAttestationV1"));
}

#[test]
fn attestation_signing_rejects_wrong_audience_and_clock_boundaries() {
    let ring = DeviceAttestationKeyRing::new(
        "key".into(),
        [("key".into(), SigningKey::from_bytes(&[33; 32]))],
    )
    .unwrap();
    let mut claims = attestation_claims(1_000_000);
    claims.audience = "koru".into();
    assert!(matches!(ring.sign(&claims), Err(CryptoError::InvalidInput)));
    let mut claims = attestation_claims(1_000_000);
    claims.expires_at_millis = claims
        .issued_at_millis
        .checked_add(DEVICE_ATTESTATION_MAX_MILLIS)
        .unwrap()
        .checked_add(1_000)
        .unwrap();
    assert!(matches!(ring.sign(&claims), Err(CryptoError::InvalidInput)));
    claims.issued_at_millis = i64::MIN;
    claims.expires_at_millis = i64::MAX;
    assert!(matches!(ring.sign(&claims), Err(CryptoError::InvalidInput)));

    let first = ProfileId::parse("60000000-0000-4000-8000-000000000006").unwrap();
    let second = ProfileId::parse("70000000-0000-4000-8000-000000000007").unwrap();
    let mut claims = attestation_claims(1_000_000);
    claims.authorized_profile_ids = vec![second, first];
    let signed = ring.sign(&claims).unwrap();
    let verified = verify_device_attestation(
        &signed,
        &ring.verification_keys(),
        "https://api.koru.do",
        1_001_000,
    )
    .unwrap();
    assert_eq!(
        verified.claims().authorized_profile_ids,
        vec![first, second]
    );
    let segments = signed.compact.split('.').collect::<Vec<_>>();
    let mut payload: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segments[1])
            .unwrap(),
    )
    .unwrap();
    payload["profiles"] = serde_json::json!([second.to_storage(), first.to_storage()]);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    let input = format!("{}.{}", segments[0], payload);
    let signature = SigningKey::from_bytes(&[33; 32]).sign(input.as_bytes());
    let noncanonical = SignedDeviceAttestationV1 {
        compact: format!(
            "{input}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ),
    };
    assert_eq!(
        verify_device_attestation(
            &noncanonical,
            &ring.verification_keys(),
            "https://api.koru.do",
            1_001_000,
        ),
        Err(JwtVerificationError::Claims)
    );
    claims.authorized_profile_ids = vec![first, first];
    assert!(matches!(ring.sign(&claims), Err(CryptoError::InvalidInput)));
}

#[test]
fn jwt_has_only_frozen_claims_and_rotation_ready_jwks() {
    let h = Harness::new();
    let now = h.clock.now_millis();
    let identity = identity();
    let session = session_id();
    let token = h
        .signer
        .sign_access_token(&AccessTokenClaims {
            issuer: "https://api.koru.do".into(),
            audience: "koru".into(),
            subject: identity,
            session_id: session,
            token_id: "jti".into(),
            issued_at_millis: now,
            expires_at_millis: now + ACCESS_TOKEN_MILLIS,
        })
        .unwrap();
    assert_eq!(token.key_id, "new");
    let claims = verify_access_token(
        token.compact.expose_secret(),
        &h.signer.verification_keys(),
        "https://api.koru.do",
        "koru",
        now,
    )
    .unwrap();
    assert_eq!(claims.subject, identity);
    let old_signer = Ed25519KeyRing::new(
        "old".into(),
        [("old".into(), SigningKey::from_bytes(&[7; 32]))],
    )
    .unwrap();
    let old_token = old_signer
        .sign_access_token(&AccessTokenClaims {
            issuer: "https://api.koru.do".into(),
            audience: "koru".into(),
            subject: identity,
            session_id: session,
            token_id: "old-jti".into(),
            issued_at_millis: now,
            expires_at_millis: now + ACCESS_TOKEN_MILLIS,
        })
        .unwrap();
    assert!(
        verify_access_token(
            old_token.compact.expose_secret(),
            &h.signer.verification_keys(),
            "https://api.koru.do",
            "koru",
            now
        )
        .is_ok()
    );
    assert_eq!(
        verify_access_token(
            token.compact.expose_secret(),
            &h.signer.verification_keys(),
            "https://wrong.invalid",
            "koru",
            now
        ),
        Err(JwtVerificationError::Claims)
    );
    assert_eq!(
        verify_access_token(
            token.compact.expose_secret(),
            &h.signer.verification_keys(),
            "https://api.koru.do",
            "wrong",
            now
        ),
        Err(JwtVerificationError::Claims)
    );
    let mut parts = std::str::from_utf8(token.compact.expose_secret())
        .unwrap()
        .split('.')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&parts[2])
        .unwrap();
    signature[0] ^= 1;
    parts[2] = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature);
    let tampered = parts.join(".").into_bytes();
    assert_eq!(
        verify_access_token(
            &tampered,
            &h.signer.verification_keys(),
            "https://api.koru.do",
            "koru",
            now
        ),
        Err(JwtVerificationError::Signature)
    );
    let original_parts = std::str::from_utf8(token.compact.expose_secret())
        .unwrap()
        .split('.')
        .collect::<Vec<_>>();
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(original_parts[0])
        .unwrap();
    let duplicate_header = String::from_utf8(header)
        .unwrap()
        .replace("\"alg\":\"EdDSA\"", "\"alg\":\"EdDSA\",\"alg\":\"EdDSA\"");
    let duplicate = format!(
        "{}.{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(duplicate_header),
        original_parts[1],
        original_parts[2]
    );
    assert_eq!(
        verify_access_token(
            duplicate.as_bytes(),
            &h.signer.verification_keys(),
            "https://api.koru.do",
            "koru",
            now
        ),
        Err(JwtVerificationError::Malformed)
    );
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"alg":"EdDSA","typ":"JWT","kid":"new"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({
            "iss":"https://api.koru.do",
            "aud":"koru",
            "sub":identity.to_string(),
            "sid":session.to_string(),
            "jti":"overlong",
            "iat":now / 1000,
            "exp":now / 1000 + 901
        }))
        .unwrap(),
    );
    let signed = format!("{header}.{payload}");
    let signature = SigningKey::from_bytes(&[8; 32]).sign(signed.as_bytes());
    let overlong = format!(
        "{signed}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    assert_eq!(
        verify_access_token(
            overlong.as_bytes(),
            &h.signer.verification_keys(),
            "https://api.koru.do",
            "koru",
            now
        ),
        Err(JwtVerificationError::Claims)
    );
    assert_eq!(
        verify_access_token(
            token.compact.expose_secret(),
            &h.signer.verification_keys(),
            "https://api.koru.do",
            "koru",
            now + ACCESS_TOKEN_MILLIS
        ),
        Err(JwtVerificationError::Claims)
    );
    let jwks: serde_json::Value = serde_json::from_str(&h.signer.public_jwks().unwrap()).unwrap();
    assert_eq!(jwks["keys"].as_array().unwrap().len(), 2);
    let payload = std::str::from_utf8(token.compact.expose_secret())
        .unwrap()
        .split('.')
        .nth(1)
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap();
    let keys: BTreeMap<_, _> = json
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), ()))
        .collect();
    assert_eq!(
        keys.keys().cloned().collect::<Vec<_>>(),
        ["aud", "exp", "iat", "iss", "jti", "sid", "sub"]
    );
}

#[test]
fn symbolic_returns_csrf_redirect_and_rate_policies_fail_closed() {
    assert!(SymbolicReturnTarget::parse("https://evil.invalid").is_err());
    assert_eq!(
        SymbolicReturnTarget::parse("connection_authorize"),
        Ok(SymbolicReturnTarget::ConnectionAuthorize)
    );
    assert!(SymbolicReturnTarget::parse("connection_authorize/../x").is_err());
    assert_eq!(PROVIDER_REDIRECT_POLICY, ProviderRedirectPolicy::Disabled);
    assert!(
        validate_web_request(Some("https://koru.do"), "https://koru.do", b"csrf", b"csrf").is_ok()
    );
    assert_eq!(
        validate_web_request(
            Some("https://evil.invalid"),
            "https://koru.do",
            b"csrf",
            b"csrf"
        ),
        Err(SecurityError::InvalidOrigin)
    );
    assert_eq!(
        validate_web_request(
            Some("https://koru.do"),
            "https://koru.do",
            b"csrf",
            b"other"
        ),
        Err(SecurityError::InvalidCsrf)
    );
    assert_eq!((START_RATE.limit, START_RATE.window_millis), (20, 600_000));
    assert_eq!(
        (
            CALLBACK_RATE.limit,
            LINK_RATE.limit,
            REFRESH_RATE.limit,
            DEVICE_RATE.limit
        ),
        (10, 5, 30, 200)
    );
    let h = Harness::new();
    let service = h.service();
    for _ in 0..START_RATE.limit {
        assert!(
            pollster::block_on(
                service.check_rate(RateLimitSubject::AuthStart { source: b"source" })
            )
            .is_ok()
        );
    }
    assert_eq!(
        pollster::block_on(service.check_rate(RateLimitSubject::AuthStart { source: b"source" })),
        Err(SecurityError::RateLimited)
    );
    h.clock
        .0
        .fetch_add(START_RATE.window_millis as i64, Ordering::SeqCst);
    assert!(
        pollster::block_on(service.check_rate(RateLimitSubject::AuthStart { source: b"source" }))
            .is_ok()
    );
    assert!(
        pollster::block_on(service.check_rate(RateLimitSubject::CallbackOrExchange {
            flow: flow_id(),
            source: b"source"
        }))
        .is_ok()
    );
    for _ in 0..AUTHENTICATION_ATTEMPT_RATE.limit {
        assert!(
            pollster::block_on(service.check_rate(RateLimitSubject::AuthenticationAttempt {
                source: b"attacker-a"
            }))
            .is_ok()
        );
    }
    assert_eq!(
        pollster::block_on(service.check_rate(RateLimitSubject::AuthenticationAttempt {
            source: b"attacker-a"
        })),
        Err(SecurityError::RateLimited)
    );
    assert!(
        pollster::block_on(service.check_rate(RateLimitSubject::AuthenticationAttempt {
            source: b"caller-b"
        }))
        .is_ok()
    );
    h.clock.0.fetch_add(
        AUTHENTICATION_ATTEMPT_RATE.window_millis as i64,
        Ordering::SeqCst,
    );
    assert!(
        pollster::block_on(service.check_rate(RateLimitSubject::AuthenticationAttempt {
            source: b"attacker-a"
        }))
        .is_ok()
    );
}

#[test]
fn enrollment_budgets_are_bounded_and_do_not_starve_completion_or_device_mutations() {
    let h = Harness::new();
    let service = h.service();
    let subject = |operation| RateLimitSubject::Enrollment {
        identity: identity(),
        device: device_id(),
        operation,
    };
    // More than the old ten-minute budget, as in reconnecting two Profiles.
    for _ in 0..31 {
        for operation in [
            EnrollmentRateOperation::Discovery,
            EnrollmentRateOperation::Attestation,
        ] {
            assert!(pollster::block_on(service.check_rate(subject(operation))).is_ok());
        }
    }
    for _ in 31..ENROLLMENT_RATE.limit {
        assert!(
            pollster::block_on(service.check_rate(subject(EnrollmentRateOperation::Discovery)))
                .is_ok()
        );
    }
    let denied =
        pollster::block_on(service.rate_decision(subject(EnrollmentRateOperation::Discovery)))
            .unwrap();
    assert!(!denied.allowed);
    assert_eq!(
        denied.retry_after_millis,
        Some(ENROLLMENT_RATE.window_millis)
    );
    assert!(
        pollster::block_on(service.check_rate(subject(EnrollmentRateOperation::Attestation)))
            .is_ok()
    );
    assert!(
        pollster::block_on(service.check_rate(subject(EnrollmentRateOperation::Provisioning)))
            .is_ok()
    );
    for _ in 0..DEVICE_RATE.limit {
        assert!(
            pollster::block_on(service.check_rate(RateLimitSubject::DeviceOperation {
                identity: identity(),
                device: device_id()
            }))
            .is_ok()
        );
    }
    assert_eq!(
        pollster::block_on(service.check_rate(RateLimitSubject::DeviceOperation {
            identity: identity(),
            device: device_id()
        })),
        Err(SecurityError::RateLimited)
    );
    h.clock
        .0
        .fetch_add(ENROLLMENT_RATE.window_millis as i64, Ordering::SeqCst);
    assert!(
        pollster::block_on(service.check_rate(subject(EnrollmentRateOperation::Discovery))).is_ok()
    );
    assert_eq!(
        pollster::block_on(service.check_rate(RateLimitSubject::DeviceOperation {
            identity: identity(),
            device: device_id()
        })),
        Err(SecurityError::RateLimited)
    );
}

#[test]
fn native_refresh_exact_retry_replay_and_expiry_are_deterministic() {
    let h = Arc::new(Harness::new());
    let s = h.service();
    let issued =
        pollster::block_on(s.issue_native(identity(), auth_identity(), device_id())).unwrap();
    let id = *h.sessions.0.lock().unwrap().keys().next().unwrap();
    let key = OpaqueSecret(vec![1; 32]);
    let barrier = Arc::new(Barrier::new(2));
    let run = |h: Arc<Harness>, barrier: Arc<Barrier>, refresh: OpaqueSecret, key: OpaqueSecret| {
        std::thread::spawn(move || {
            barrier.wait();
            pollster::block_on(h.service().refresh(id, &refresh, &key)).unwrap()
        })
    };
    let left = run(
        h.clone(),
        barrier.clone(),
        issued.refresh.clone(),
        key.clone(),
    );
    let right = run(h.clone(), barrier, issued.refresh.clone(), key.clone());
    let rotated = left.join().unwrap();
    let retry = right.join().unwrap();
    assert_eq!(rotated, retry);
    let other = OpaqueSecret(vec![2; 32]);
    assert_eq!(
        pollster::block_on(s.refresh(id, &issued.refresh, &other)),
        Err(SecurityError::Replayed)
    );
    assert!(
        h.sessions.0.lock().unwrap()[&id]
            .revoked_at_millis
            .is_some()
    );
    let h = Arc::new(Harness::new());
    let issued = pollster::block_on(h.service().issue_native(
        identity(),
        auth_identity(),
        device_id(),
    ))
    .unwrap();
    let id = *h.sessions.0.lock().unwrap().keys().next().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let run = |h: Arc<Harness>, barrier: Arc<Barrier>, key: u8| {
        let refresh = issued.refresh.clone();
        std::thread::spawn(move || {
            barrier.wait();
            pollster::block_on(
                h.service()
                    .refresh(id, &refresh, &OpaqueSecret(vec![key; 32])),
            )
        })
    };
    let left = run(h.clone(), barrier.clone(), 3);
    let right = run(h.clone(), barrier, 4);
    let results = [left.join().unwrap(), right.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(SecurityError::Replayed))
            .count(),
        1
    );
    assert!(
        h.sessions.0.lock().unwrap()[&id]
            .revoked_at_millis
            .is_some()
    );
    let h = Harness::new();
    let s = h.service();
    let issued =
        pollster::block_on(s.issue_native(identity(), auth_identity(), device_id())).unwrap();
    let id = *h.sessions.0.lock().unwrap().keys().next().unwrap();
    h.clock
        .0
        .store(1_000_000 + REFRESH_IDLE_MILLIS, Ordering::SeqCst);
    assert_eq!(
        pollster::block_on(s.refresh(id, &issued.refresh, &key)),
        Err(SecurityError::Expired)
    );
    let h = Harness::new();
    let s = h.service();
    let issued =
        pollster::block_on(s.issue_native(identity(), auth_identity(), device_id())).unwrap();
    let id = *h.sessions.0.lock().unwrap().keys().next().unwrap();
    pollster::block_on(s.refresh(id, &issued.refresh, &key)).unwrap();
    h.clock.0.fetch_add(RETRY_MILLIS, Ordering::SeqCst);
    assert_eq!(
        pollster::block_on(s.refresh(id, &issued.refresh, &key)),
        Err(SecurityError::Replayed)
    );
}

#[test]
fn regional_cutover_preserves_exact_refresh_bytes_then_rotates_to_version_two() {
    let h = Harness::new();
    let original = pollster::block_on(h.service().issue_native(
        identity(),
        auth_identity(),
        device_id(),
    ))
    .unwrap();
    let retry_key = OpaqueSecret(vec![51; 32]);
    let rotated = pollster::block_on(h.service().refresh(
        original.session_id,
        &original.refresh,
        &retry_key,
    ))
    .unwrap();
    let policy = RegionalCredentialPolicy {
        region: koru_account_core::workspace_authorization::Region::Us,
        regional_issuer: "https://us.api.koru.do".into(),
        legacy_issuer: "https://api.koru.do".into(),
        legacy_cutover_millis: 1_000_000,
        issuance: CredentialIssuance::Regional,
        legacy_access_kids: ["old".into(), "new".into()].into(),
        legacy_attestation_kids: ["old-attestation".into()].into(),
    };
    let regional = Ed25519KeyRing::new(
        "regional".into(),
        [("regional".into(), SigningKey::from_bytes(&[9; 32]))],
    )
    .unwrap()
    .with_verification_keys(h.signer.verification_keys())
    .unwrap()
    .with_regional_policy(policy.clone())
    .unwrap();
    h.clock.0.fetch_add(1_000, Ordering::SeqCst);
    let security = SessionSecurity {
        issuer: &policy.regional_issuer,
        signer: &regional,
        ..h.service()
    };
    let exact =
        pollster::block_on(security.refresh(original.session_id, &original.refresh, &retry_key))
            .unwrap();
    assert_eq!(
        exact, rotated,
        "return the sealed original result, including every credential byte and expiry"
    );
    assert!(
        verify_access_token_with_policy(
            exact.access.compact.expose_secret(),
            &regional.verification_keys(),
            &policy.regional_issuer,
            "koru",
            h.clock.now_millis(),
            Some(&policy)
        )
        .is_ok()
    );
    let next = pollster::block_on(security.refresh(
        original.session_id,
        &rotated.refresh,
        &OpaqueSecret(vec![52; 32]),
    ))
    .unwrap();
    assert_eq!(next.session_id, original.session_id);
    assert_eq!(
        next.absolute_expires_at_millis,
        original.absolute_expires_at_millis
    );
    assert!(
        verify_access_token_with_policy(
            next.access.compact.expose_secret(),
            &regional.verification_keys(),
            &policy.regional_issuer,
            "koru",
            h.clock.now_millis(),
            Some(&policy)
        )
        .is_ok()
    );
    let compact = std::str::from_utf8(next.access.compact.expose_secret()).unwrap();
    let payload: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(compact.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["ver"], 2);
    assert_eq!(payload["region"], "US");
}

#[test]
fn native_issue_signer_failure_does_not_persist_an_orphaned_session() {
    let h = Harness::new();
    let service = SessionSecurity {
        issuer: "https://api.koru.do",
        audience: "koru",
        clock: &h.clock,
        entropy: &h.entropy,
        sessions: &h.sessions,
        flows: &h.flows,
        crypto: &h.crypto,
        signer: &FailingSigner,
        limiter: &h.limits,
    };
    assert_eq!(
        pollster::block_on(service.issue_native(identity(), auth_identity(), device_id())),
        Err(SecurityError::Crypto)
    );
    assert!(h.sessions.0.lock().unwrap().is_empty());
}

#[test]
fn web_sessions_are_separate_and_flows_are_encrypted_expiring_single_use() {
    let h = Harness::new();
    let s = h.service();
    let web = pollster::block_on(s.issue_web(identity(), auth_identity())).unwrap();
    assert_ne!(web.session.expose_secret(), web.csrf.expose_secret());
    let web_id = *h.sessions.0.lock().unwrap().keys().next().unwrap();
    assert_eq!(h.sessions.0.lock().unwrap()[&web_id].kind, SessionKind::Web);
    assert!(pollster::block_on(s.validate_web_session(web_id, &web.session)).is_ok());
    assert_eq!(
        pollster::block_on(s.validate_web_session(web_id, &OpaqueSecret(b"stolen".to_vec()))),
        Err(SecurityError::InvalidSecret)
    );
    let flow = pollster::block_on(s.create_flow(
        AuthFlowKind::GoogleNative,
        b"raw-source",
        None,
        SymbolicReturnTarget::Releases,
        b"pkce-secret".to_vec(),
    ))
    .unwrap();
    let stored = h.flows.0.lock().unwrap()[&flow.flow_id].clone();
    let serialized = serde_json::to_string(&stored).unwrap();
    for secret in ["raw-source", "pkce-secret"] {
        assert!(!serialized.contains(secret))
    }
    let nonce = std::str::from_utf8(flow.nonce.expose_secret()).unwrap();
    assert_eq!(nonce.len(), 43);
    assert!(
        nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    );
    assert_eq!(
        pollster::block_on(s.consume_flow(flow.flow_id, &OpaqueSecret(b"stolen".to_vec()))),
        Err(SecurityError::InvalidSecret)
    );
    let payload = pollster::block_on(s.consume_flow(flow.flow_id, &flow.state)).unwrap();
    assert_eq!(payload.return_target, SymbolicReturnTarget::Releases);
    assert_eq!(
        pollster::block_on(s.consume_flow(flow.flow_id, &flow.state)),
        Err(SecurityError::Replayed)
    );
    let grant = pollster::block_on(s.issue_grant(
        AuthFlowKind::GoogleNative,
        identity(),
        SymbolicReturnTarget::Account,
    ))
    .unwrap();
    assert_eq!(
        pollster::block_on(s.exchange_grant(grant.flow_id, &OpaqueSecret(b"stolen".to_vec()))),
        Err(SecurityError::InvalidSecret)
    );
    assert!(pollster::block_on(s.exchange_grant(grant.flow_id, &grant.grant)).is_ok());
    assert_eq!(
        pollster::block_on(s.exchange_grant(grant.flow_id, &grant.grant)),
        Err(SecurityError::Replayed)
    );
    let flow = pollster::block_on(s.create_flow(
        AuthFlowKind::GoogleWeb,
        b"source",
        None,
        SymbolicReturnTarget::Account,
        vec![],
    ))
    .unwrap();
    h.clock.0.fetch_add(FLOW_MILLIS, Ordering::SeqCst);
    assert_eq!(
        pollster::block_on(s.consume_flow(flow.flow_id, &flow.state)),
        Err(SecurityError::Expired)
    );
}

#[test]
fn native_grant_validation_happens_before_one_use_consumption() {
    let harness = Harness::new();
    let security = harness.service();
    let signing = DeviceSigningKey::from_secret_bytes([4; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([5; 32]);
    let verifier = vec![b'a'; 43];
    let challenge =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(&verifier));
    let parent = flow_id();
    let wrong_parent = AuthFlowId::from_str("50000000-0000-4000-8000-000000000006").unwrap();
    let grant = pollster::block_on(security.issue_exchange_grant(
        AuthFlowKind::GoogleNative,
        identity(),
        Some(auth_identity()),
        harness.clock.now_millis(),
        SymbolicReturnTarget::Account,
        Some(NativeExchangeGrantInput {
            registration: DeviceRegistration {
                descriptor: DeviceDescriptorV1::new(
                    device_id(),
                    "Device".into(),
                    &signing,
                    endpoint.endpoint_id(),
                    1,
                ),
                platform: DevicePlatform::Windows,
                app_version: "0.0.3".into(),
                enrollment_protocol_version: None,
                peer_protocol_version: None,
                ownership_proof: None,
            },
            client_state: b"expected-state".to_vec(),
            pkce_challenge: challenge,
        }),
        None,
        None,
        None,
        Some(parent),
        None,
    ))
    .unwrap();
    assert_eq!(
        pollster::block_on(security.exchange_native_grant(
            grant.flow_id,
            &grant.grant,
            b"expected-state",
            &verifier,
            b"source",
            wrong_parent,
        )),
        Err(SecurityError::InvalidSecret)
    );
    assert_eq!(
        pollster::block_on(security.exchange_native_grant(
            grant.flow_id,
            &grant.grant,
            b"wrong-state",
            &verifier,
            b"source",
            parent,
        )),
        Err(SecurityError::InvalidSecret)
    );
    let mut wrong_verifier = verifier.clone();
    wrong_verifier[0] = b'b';
    assert_eq!(
        pollster::block_on(security.exchange_native_grant(
            grant.flow_id,
            &grant.grant,
            b"expected-state",
            &wrong_verifier,
            b"source",
            parent,
        )),
        Err(SecurityError::InvalidSecret)
    );
    assert!(
        pollster::block_on(security.exchange_native_grant(
            grant.flow_id,
            &grant.grant,
            b"expected-state",
            &verifier,
            b"source",
            parent,
        ))
        .is_ok()
    );
}

#[test]
fn link_candidate_is_bound_to_initiating_session_and_exact_expiry() {
    let harness = Harness::new();
    let security = harness.service();
    let parent = pollster::block_on(security.create_flow(
        AuthFlowKind::LinkAuthIdentity,
        b"source",
        Some(identity()),
        SymbolicReturnTarget::Account,
        Vec::new(),
    ))
    .unwrap();
    let initiating_session = session_id();
    let grant = pollster::block_on(security.issue_exchange_grant(
        AuthFlowKind::LinkAuthIdentity,
        identity(),
        Some(auth_identity()),
        harness.clock.now_millis(),
        SymbolicReturnTarget::Account,
        None,
        Some(VerifiedExternalIdentity {
            provider: AuthProvider::Google,
            canonical_issuer: "https://accounts.google.com".into(),
            subject: "provider-subject".into(),
            verified_email: Some("person@example.test".into()),
            display_name_snapshot: None,
        }),
        Some(profile_id()),
        None,
        Some(parent.flow_id),
        Some(initiating_session),
    ))
    .unwrap();
    let wrong_session = AccountSessionId::from_str("30000000-0000-4000-8000-000000000099").unwrap();
    assert_eq!(
        pollster::block_on(security.validate_link_grant(
            grant.flow_id,
            &grant.grant,
            parent.flow_id,
            identity(),
            wrong_session,
        )),
        Err(SecurityError::InvalidSecret)
    );
    let inspected = pollster::block_on(security.validate_link_grant(
        grant.flow_id,
        &grant.grant,
        parent.flow_id,
        identity(),
        initiating_session,
    ))
    .unwrap();
    assert_eq!(inspected.profile_id, Some(profile_id()));
    assert_eq!(inspected.initiating_session_id, Some(initiating_session));
    assert_eq!(
        inspected.expires_at_millis,
        harness.clock.now_millis() + FLOW_MILLIS
    );
    harness.clock.0.fetch_add(FLOW_MILLIS, Ordering::SeqCst);
    assert_eq!(
        pollster::block_on(security.validate_link_grant(
            grant.flow_id,
            &grant.grant,
            parent.flow_id,
            identity(),
            initiating_session,
        )),
        Err(SecurityError::Expired)
    );
}

#[test]
fn errors_and_debug_output_never_disclose_seeded_secrets_or_pii() {
    let sentinels = [
        "token-sentinel",
        "code-sentinel",
        "nonce-sentinel",
        "state-sentinel",
        "source-sentinel",
        "person@example.invalid",
        "https://full.invalid/private",
    ];
    let signing = DeviceSigningKey::from_secret_bytes([4; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([5; 32]);
    let registration = DeviceRegistration {
        descriptor: DeviceDescriptorV1::new(
            device_id(),
            "person@example.invalid".into(),
            &signing,
            endpoint.endpoint_id(),
            1,
        ),
        platform: DevicePlatform::Windows,
        app_version: "https://full.invalid/private".into(),
        enrollment_protocol_version: None,
        peer_protocol_version: None,
        ownership_proof: None,
    };
    let binding = NativeExchangeBinding {
        registration: registration.clone(),
        client_state_digest: KeyedDigest(b"state-sentinel".to_vec()),
        pkce_challenge: "code-sentinel".into(),
    };
    let exchange = ExchangeGrantPayload {
        parent_flow_id: Some(flow_id()),
        koru_identity_id: identity(),
        auth_identity_id: auth_identity(),
        authenticated_at_millis: 1,
        native: Some(binding.clone()),
        verified_candidate: None,
        profile_id: None,
        erased_koru_identity_id: None,
        initiating_session_id: Some(session_id()),
        expires_at_millis: 2,
    };
    let session = AccountSession {
        id: session_id(),
        koru_identity_id: identity(),
        authenticated_by: auth_identity(),
        device_id: Some(device_id()),
        kind: SessionKind::Native,
        refresh_family: None,
        created_at_millis: 1,
        last_seen_at_millis: 1,
        recent_auth_at_millis: 1,
        idle_expires_at_millis: 2,
        absolute_expires_at_millis: 3,
        revoked_at_millis: None,
    };
    let values = vec![
        format!("{:?}", OpaqueSecret(sentinels.join("|").into_bytes())),
        format!(
            "{:?}",
            NativeSessionTokens {
                session_id: session_id(),
                access: SignedToken {
                    key_id: "key".into(),
                    compact: OpaqueSecret(sentinels.join("|").into_bytes())
                },
                refresh: OpaqueSecret(sentinels.join("|").into_bytes()),
                access_expires_at_millis: 1,
                idle_expires_at_millis: 2,
                absolute_expires_at_millis: 3,
            }
        ),
        format!(
            "{:?}",
            FlowSecrets {
                flow_id: flow_id(),
                state: OpaqueSecret(sentinels.join("|").into_bytes()),
                nonce: OpaqueSecret(sentinels.join("|").into_bytes())
            }
        ),
        format!(
            "{:?}",
            FlowPayload {
                return_target: SymbolicReturnTarget::Account,
                nonce: OpaqueSecret(sentinels.join("|").into_bytes()),
                pkce_artifact: OpaqueSecret(vec![]),
                one_use_grant: OpaqueSecret(vec![]),
                exchange: None
            }
        ),
        format!(
            "{:?}",
            NativeExchangeGrantInput {
                registration: registration.clone(),
                client_state: b"state-sentinel".to_vec(),
                pkce_challenge: "code-sentinel".into(),
            }
        ),
        format!("{binding:?}"),
        format!("{exchange:?}"),
        format!(
            "{:?}",
            ExchangeCommitRequest {
                flow_id: flow_id(),
                grant_digest: KeyedDigest(b"token-sentinel".to_vec()),
                session,
                device: Some(registration.account_device(identity(), 1)),
                device_ownership: None,
                proof_commit_digest: None,
                sealed_result: SealedValue(b"secret-sentinel".to_vec()),
                now_millis: 1,
            }
        ),
        SecurityError::InvalidSecret.to_string(),
    ];
    for output in values {
        for sentinel in sentinels {
            assert!(!output.contains(sentinel))
        }
    }
}

#[test]
fn runtime_encryption_is_restart_stable_purpose_bound_and_tamper_evident() {
    fn crypto(seed: u8, encryption: u8, digest: u8) -> AeadTransientEncryption<FakeEntropy> {
        AeadTransientEncryption::new(
            OpaqueSecret(vec![encryption; 32]),
            OpaqueSecret(vec![digest; 32]),
            FakeEntropy(Mutex::new(seed)),
        )
        .unwrap()
    }
    let first = crypto(1, 7, 8);
    let sealed = first.seal("flow", b"restart-stable").unwrap();
    let second_seal = first.seal("flow", b"restart-stable").unwrap();
    assert_ne!(sealed, second_seal, "a nonce must never be reused");
    let reconstructed = crypto(50, 7, 8);
    assert_eq!(
        reconstructed.open("flow", &sealed).unwrap(),
        b"restart-stable"
    );
    assert_eq!(
        reconstructed.open("other-purpose", &sealed),
        Err(CryptoError::VerificationFailed)
    );
    assert_eq!(
        crypto(1, 9, 8).open("flow", &sealed),
        Err(CryptoError::VerificationFailed)
    );
    let mut tampered = sealed.clone();
    *tampered.0.last_mut().unwrap() ^= 1;
    assert_eq!(
        reconstructed.open("flow", &tampered),
        Err(CryptoError::VerificationFailed)
    );
    let mut wrong_version = sealed;
    wrong_version.0[0] = 2;
    assert_eq!(
        reconstructed.open("flow", &wrong_version),
        Err(CryptoError::VerificationFailed)
    );
    assert!(
        AeadTransientEncryption::new(
            OpaqueSecret(vec![1; 31]),
            OpaqueSecret(vec![2; 32]),
            FakeEntropy(Mutex::new(1))
        )
        .is_err()
    );
    assert!(
        AeadTransientEncryption::new(
            OpaqueSecret(vec![3; 32]),
            OpaqueSecret(vec![3; 32]),
            FakeEntropy(Mutex::new(1))
        )
        .is_err()
    );
    let debug = format!("{reconstructed:?}");
    assert!(debug.contains("redacted"));
    assert!(!debug.contains("restart-stable"));
    fn assert_clock<T: Clock>() {}
    assert_clock::<SystemClock>();
}
