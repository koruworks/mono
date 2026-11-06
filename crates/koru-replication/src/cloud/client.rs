// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_device::DeviceSigningKey;

/// Owned client-side handshake state for one stream, never persisted or reused.
pub struct ClientHandshake {
    credential: VerifiedTokenCapability<SessionClaims>,
    compact: String,
    dataset: CloudReplicaPrincipalV1,
    client: ReplicationEndpointId,
    worker: ReplicationEndpointId,
    nonce: [u8; 32],
}

impl ClientHandshake {
    /// Endpoints must come from the existing authenticated transport, not from
    /// an unsigned routing response. The network wrapper supplies a fresh nonce.
    ///
    /// `route_endpoint` is `endpoint_id` of the `CloudSyncRouteV1` the API
    /// issued. D231 withdrew the worker's signed route attestation, so this one
    /// comparison is the device's whole assurance that it reached the worker the
    /// region assigns: the authenticated remote endpoint must be the endpoint
    /// the API named (DS-D10), and it is checked before `ClientHelloV1` leaves.
    pub fn begin(
        verifier: &TokenCapabilityVerifier,
        compact: String,
        dataset: CloudReplicaPrincipalV1,
        client: ReplicationEndpointId,
        worker: ReplicationEndpointId,
        route_endpoint: ReplicationEndpointId,
        nonce: [u8; 32],
        now_millis: i64,
    ) -> Result<(Self, HandshakeV1), Refusal> {
        if route_endpoint != worker {
            return Err(Refusal::EndpointMismatch);
        }
        let credential = verify_session_credential(verifier, compact.as_bytes(), now_millis)?;
        if credential.claims().client_endpoint_id != client {
            return Err(Refusal::EndpointMismatch);
        }
        scope(credential.claims(), dataset)?;
        let hello = HandshakeV1::ClientHello {
            version: Version1,
            dataset,
            client_nonce: Bytes(nonce),
            credential: compact.clone(),
        };
        Ok((
            Self {
                credential,
                compact,
                dataset,
                client,
                worker,
                nonce,
            },
            hello,
        ))
    }

    /// The endpoint binding was settled before the hello; the device signs the
    /// two-nonce transcript over its own credential and nothing the worker
    /// asserts about itself. Whether this worker may serve the dataset is the
    /// worker's own live lease and authority check, not a claim the device reads.
    ///
    /// `device_key` is the device's signing key. An app connection's replica,
    /// which has no device key, passes its endpoint's key: the one its
    /// credential names as `client_endpoint_id`.
    pub fn challenge(
        self,
        challenge: HandshakeV1,
        device_key: &DeviceSigningKey,
        now_millis: i64,
    ) -> Result<(ClientReceipt, HandshakeV1), Refusal> {
        if let HandshakeV1::Refused { code, .. } = &challenge {
            let refusal = Refusal::from_code(code);
            if refusal == Refusal::Malformed {
                // The worker named its reason and this build does not know
                // the name. Say the name, or the person reads "malformed".
                tracing::warn!(
                    code,
                    "cloud handshake: the worker refused with a code this build does not know"
                );
            }
            return Err(refusal);
        }
        let HandshakeV1::WorkerChallenge { server_nonce, .. } = challenge else {
            tracing::warn!("cloud handshake: the challenge was not a worker challenge");
            return Err(Refusal::Malformed);
        };
        self.credential.verify_time_at(now_millis)?;
        let proof = transcript(
            self.client,
            self.worker,
            self.dataset,
            self.nonce,
            server_nonce.0,
            &self.compact,
        );
        let signature = Bytes(*device_key.sign_message(&proof).as_bytes());
        Ok((
            ClientReceipt {
                dataset: self.dataset,
                credential: self.credential,
            },
            HandshakeV1::ClientFinish {
                version: Version1,
                signature,
            },
        ))
    }
}

/// Awaiting acknowledgement is not an accepted generation. It owns the verified
/// credential expiry so a worker cannot advertise a later one in its receipt.
pub struct ClientReceipt {
    dataset: CloudReplicaPrincipalV1,
    credential: VerifiedTokenCapability<SessionClaims>,
}

impl ClientReceipt {
    pub fn accepted(
        self,
        accepted: HandshakeV1,
        now_millis: i64,
        previous_generation: u64,
    ) -> Result<PreparedSession, Refusal> {
        if let HandshakeV1::Refused { code, .. } = &accepted {
            let refusal = Refusal::from_code(code);
            if refusal == Refusal::Malformed {
                // The worker named its reason and this build does not know
                // the name. Say the name, or the person reads "malformed".
                tracing::warn!(
                    code,
                    "cloud handshake: the worker refused with a code this build does not know"
                );
            }
            return Err(refusal);
        }
        self.credential.verify_time_at(now_millis)?;
        let HandshakeV1::SessionAccepted {
            generation,
            expires_at_seconds,
            ..
        } = &accepted
        else {
            tracing::warn!("cloud handshake: the answer was not a session acceptance");
            return Err(Refusal::Malformed);
        };
        if *generation <= previous_generation || *generation == 0 {
            return Err(Refusal::Replay);
        }
        if *expires_at_seconds != self.credential.claims().exp {
            tracing::warn!("cloud handshake: the worker echoed a different credential expiry");
            return Err(Refusal::Malformed);
        }
        Ok(PreparedSession {
            principal: self.dataset,
            generation: *generation,
            expires_at_seconds: *expires_at_seconds,
            accepted,
        })
    }
}
