// SPDX-License-Identifier: MPL-2.0

use super::*;

struct ChallengeTransport(GoogleNativeTokenStartResponseV1);
impl Transport for ChallengeTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        assert!(request.path_and_query.ends_with("/native/token/start"));
        Ok(HttpResponse {
            document_removal: false,
            status: 200,
            body: serde_json::to_vec(&self.0).unwrap(),
        })
    }
}

fn attempt(start: GoogleNativeTokenStartResponseV1) -> (ServiceError, Vec<NativeSignInRequest>) {
    let delivery = Arc::new(TokenDelivery {
        cancel: true,
        ..Default::default()
    });
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(ChallengeTransport(start))),
        signed_descriptor(),
        "test".into(),
    );
    service.clock = Arc::new(CreationClock(Instant::now()));
    service.delivery = delivery.clone();
    let error = execute_native(&mut service).err().unwrap();
    let prompts = delivery.prompts.lock().unwrap().clone();
    (error, prompts)
}

#[test]
fn full_server_lifetime_opens_native_sheet_despite_client_clock_lag() {
    // The API issues a full ten minutes. Even one millisecond of lag used to
    // reject the challenge before the native provider could be called.
    for lag in [0, 1, 30_000, i64::MAX - CREATION_NOW - 600_000] {
        let mut start = native_start(false);
        start.expires_at_millis = CREATION_NOW + 600_000 + lag;
        let (error, prompts) = attempt(start);
        assert_eq!(error.code, "cancelled");
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].expires_at_millis, CREATION_NOW + 600_000);
    }
}

#[test]
fn shorter_challenge_keeps_server_expiry() {
    let mut start = native_start(false);
    start.expires_at_millis = CREATION_NOW + 60_000;
    let (_, prompts) = attempt(start);
    assert_eq!(prompts[0].expires_at_millis, CREATION_NOW + 60_000);
}

#[test]
fn expired_or_malformed_challenges_never_open_native_sheet() {
    for remaining in [-1, 0] {
        let mut start = native_start(false);
        start.expires_at_millis = CREATION_NOW + remaining;
        let (error, prompts) = attempt(start);
        assert_eq!(error.code, "native_challenge_expired");
        assert!(prompts.is_empty());
    }
    for malformed_nonce in [true, false] {
        let mut start = native_start(false);
        start.expires_at_millis = CREATION_NOW + 600_000;
        if malformed_nonce {
            start.nonce.0.clear();
        } else {
            start.flow_state.0 = "x".repeat(513);
        }
        let (error, prompts) = attempt(start);
        assert_eq!(error.code, "native_challenge_invalid");
        assert!(prompts.is_empty());
    }
}
