// SPDX-License-Identifier: MPL-2.0

//! Cloud application admission, separate from bilateral device replication.
//! No Loro transfer is enabled until the D199 transfer-carrier gate closes.

mod client;
pub mod client_store;
pub mod object;
pub mod transfer;
mod wire;
pub use client::{ClientHandshake, ClientReceipt};
pub use wire::{Bytes, HandshakeV1, MAX_HANDSHAKE_BYTES, Version1, decode, encode};

pub use koru_account_session::cloud::session::{SessionClaims, SessionPrincipal};
use koru_account_session::{
    TokenCapabilityVerifier, VerifiedTokenCapability, cloud::session::verify_session_credential,
};
use koru_device::{
    DevicePublicKey, ReplicationEndpointId, SignatureBytes,
    cloud::{CloudId, CloudReplicaPrincipalV1},
};
use sha2::{Digest, Sha256};

pub const CLOUD_ALPN_V1: &[u8] = b"koru/cloud-sync/1";
pub const HANDSHAKE_DEADLINE_MILLIS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Credential(koru_account_session::TokenCapabilityVerificationError),
    Malformed,
    /// The peer ended the stream before saying anything. Not a message that
    /// would not parse: a worker that refuses before it can answer resets
    /// the stream, and for a day that read as "malformed" on the desktop.
    Closed,
    Timeout,
    EndpointMismatch,
    Replay,
    PrincipalNotAllowed,
    DeviceRevoked,
    WorkspaceNotAuthorized,
    ReplicaMismatch,
    IncarnationStale,
    MembershipEpoch,
    CloudAccessEpoch,
    ReplicaFenced,
    AuthorizationStale,
    AuthorizationUnavailable,
    CloudDisabled,
    CloudDeleting,
    SyncUnavailable,
}

impl Refusal {
    /// Recognized peer codes retain their stable meaning; an unknown code is
    /// never promoted to authority and fails closed as a malformed handshake.
    pub fn from_code(code: &str) -> Self {
        use koru_account_session::TokenCapabilityVerificationError as E;
        [
            Self::Malformed,
            Self::Closed,
            Self::Timeout,
            Self::EndpointMismatch,
            Self::Replay,
            Self::PrincipalNotAllowed,
            Self::DeviceRevoked,
            Self::WorkspaceNotAuthorized,
            Self::ReplicaMismatch,
            Self::IncarnationStale,
            Self::MembershipEpoch,
            Self::CloudAccessEpoch,
            Self::ReplicaFenced,
            Self::AuthorizationStale,
            Self::AuthorizationUnavailable,
            Self::CloudDisabled,
            Self::CloudDeleting,
            Self::SyncUnavailable,
            Self::Credential(E::Malformed),
            Self::Credential(E::UnknownKey),
            Self::Credential(E::Signature),
            Self::Credential(E::InvalidClaims),
            Self::Credential(E::UnsupportedVersion),
            Self::Credential(E::WrongPurpose),
            Self::Credential(E::WrongAudience),
            Self::Credential(E::WrongRegion),
            Self::Credential(E::WrongIssuer),
            Self::Credential(E::CredentialNotYetValid),
            Self::Credential(E::CredentialExpired),
        ]
        .into_iter()
        .find(|known| known.code() == code)
        .unwrap_or(Self::Malformed)
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Credential(error) => error.code(),
            Self::Malformed => "handshake_malformed",
            Self::Closed => "handshake_closed",
            Self::Timeout => "handshake_timeout",
            Self::EndpointMismatch => "endpoint_mismatch",
            Self::Replay => "handshake_replay",
            Self::PrincipalNotAllowed => "principal_not_allowed",
            Self::DeviceRevoked => "device_revoked",
            Self::WorkspaceNotAuthorized => "workspace_not_authorized",
            Self::ReplicaMismatch => "replica_mismatch",
            Self::IncarnationStale => "cloud_incarnation_stale",
            Self::MembershipEpoch => "membership_epoch_mismatch",
            Self::CloudAccessEpoch => "cloud_access_epoch_stale",
            Self::ReplicaFenced => "replica_fenced",
            Self::AuthorizationStale => "authorization_stale",
            Self::AuthorizationUnavailable => "authorization_unavailable",
            Self::CloudDisabled => "cloud_disabled",
            Self::CloudDeleting => "cloud_deleting",
            Self::SyncUnavailable => "sync_unavailable",
        }
    }
}

impl From<koru_account_session::TokenCapabilityVerificationError> for Refusal {
    fn from(error: koru_account_session::TokenCapabilityVerificationError) -> Self {
        Self::Credential(error)
    }
}

/// Fresh hosted facts, never derived from the unverified request. The provider
/// owns live authority freshness and lifecycle/refusal policy; no SQL here.
#[derive(Clone, PartialEq, Eq)]
pub struct CurrentAuthority {
    /// The key that signs the handshake transcript. For a device, its
    /// registered signing key. An app connection has no device key: its
    /// credential's endpoint key signs (the key the authenticated transport
    /// proves), and admission refuses a provider that names any other.
    pub device_key: DevicePublicKey,
    pub registered_endpoint: ReplicationEndpointId,
    pub dataset: CloudReplicaPrincipalV1,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    pub worker_endpoint: ReplicationEndpointId,
    pub worker_id: CloudId,
    pub lease_id: CloudId,
    pub lease_epoch: i64,
}

pub trait Authority: Send + Sync {
    /// Must require current active device, Profile/Workspace authorization,
    /// enabled lifecycle, live lease and relevant freshness. No positive cache
    /// inferred from token validity. Called again immediately before acceptance.
    /// This synchronous port reads a prepared, nonblocking authority snapshot;
    /// the host performs refresh I/O during bounded preparation, before this
    /// synchronous port is called.
    fn current(
        &self,
        credential: &SessionClaims,
        dataset: CloudReplicaPrincipalV1,
    ) -> Result<CurrentAuthority, Refusal>;
}

/// One allocator per live QUIC connection, discarded on disconnect. Failed
/// writes may consume a number; a number is never reused for that dataset.
#[derive(Default)]
pub struct ConnectionGenerations(std::collections::BTreeMap<CloudReplicaPrincipalV1, u64>);

impl ConnectionGenerations {
    pub fn next(&mut self, dataset: CloudReplicaPrincipalV1) -> Result<u64, Refusal> {
        let generation = self.0.entry(dataset).or_default();
        *generation = generation.checked_add(1).ok_or(Refusal::SyncUnavailable)?;
        Ok(*generation)
    }
}

fn scope(credential: &SessionClaims, requested: CloudReplicaPrincipalV1) -> Result<(), Refusal> {
    let expected = match requested {
        CloudReplicaPrincipalV1::Workspace { workspace_id, .. } => {
            if workspace_id != credential.workspace_id {
                return Err(Refusal::WorkspaceNotAuthorized);
            }
            CloudReplicaPrincipalV1::Workspace {
                workspace_id,
                incarnation_id: credential.workspace_incarnation_id,
                logical_replica_id: credential.workspace_replica_id,
            }
        }
        CloudReplicaPrincipalV1::Profile { profile_id, .. } => {
            if profile_id != credential.profile_id {
                return Err(Refusal::WorkspaceNotAuthorized);
            }
            CloudReplicaPrincipalV1::Profile {
                profile_id,
                lineage_id: credential.profile_lineage_id,
                logical_replica_id: credential.profile_replica_id,
            }
        }
    };
    same_dataset(requested, expected)
}

fn same_dataset(
    requested: CloudReplicaPrincipalV1,
    current: CloudReplicaPrincipalV1,
) -> Result<(), Refusal> {
    if requested.logical_replica_id() != current.logical_replica_id() {
        return Err(Refusal::ReplicaMismatch);
    }
    if requested != current {
        return Err(Refusal::IncarnationStale);
    }
    Ok(())
}

fn authority(
    credential: &SessionClaims,
    dataset: CloudReplicaPrincipalV1,
    current: &CurrentAuthority,
    client: ReplicationEndpointId,
) -> Result<(), Refusal> {
    if current.registered_endpoint != client {
        return Err(Refusal::EndpointMismatch);
    }
    // A connection's transcript is signed by its credential's endpoint key,
    // never by a key a provider chose (cloud-sync-admission, the app
    // connection amendment's handshake).
    if matches!(credential.principal, SessionPrincipal::AppConnection { .. })
        && current.device_key.as_bytes() != credential.client_endpoint_id.as_bytes()
    {
        return Err(Refusal::EndpointMismatch);
    }
    scope(credential, dataset)?;
    same_dataset(dataset, current.dataset)?;
    if credential.authorization_version != current.authorization_version {
        return Err(Refusal::AuthorizationStale);
    }
    if credential.membership_epoch != current.membership_epoch {
        return Err(Refusal::MembershipEpoch);
    }
    if credential.cloud_access_epoch != current.cloud_access_epoch {
        return Err(Refusal::CloudAccessEpoch);
    }
    Ok(())
}

/// Verification precedence step 7, from the worker's own rows only: the
/// assignment this worker serves must name the endpoint it is actually bound
/// to. D231 withdrew the signed route attestation, so nothing here reads a
/// worker claim; lease id and epoch are pinned by the authority provider,
/// which fences the session on any change rather than comparing a proof.
fn serving_endpoint(
    current: &CurrentAuthority,
    worker: ReplicationEndpointId,
) -> Result<(), Refusal> {
    if current.worker_endpoint != worker {
        return Err(Refusal::EndpointMismatch);
    }
    Ok(())
}

/// No JSON reserialization: bind the exact signed ASCII credential bytes.
pub fn transcript(
    client: ReplicationEndpointId,
    worker: ReplicationEndpointId,
    dataset: CloudReplicaPrincipalV1,
    client_nonce: [u8; 32],
    worker_nonce: [u8; 32],
    credential: &str,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(244);
    bytes.extend_from_slice(b"koru/cloud-sync/client-finish/v1\0");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(client.as_bytes());
    bytes.extend_from_slice(worker.as_bytes());
    match dataset {
        CloudReplicaPrincipalV1::Workspace {
            workspace_id,
            incarnation_id,
            logical_replica_id,
        } => {
            bytes.push(1);
            for id in [workspace_id, incarnation_id, logical_replica_id] {
                bytes.extend_from_slice(id.as_bytes());
            }
        }
        CloudReplicaPrincipalV1::Profile {
            profile_id,
            lineage_id,
            logical_replica_id,
        } => {
            bytes.push(2);
            for id in [profile_id, lineage_id, logical_replica_id] {
                bytes.extend_from_slice(id.as_bytes());
            }
        }
    }
    bytes.extend_from_slice(&client_nonce);
    bytes.extend_from_slice(&worker_nonce);
    bytes.extend_from_slice(&Sha256::digest(credential.as_bytes()));
    bytes
}

/// Cryptographically verified first-message claims bound to the authenticated
/// client endpoint. This is preparation input, not current hosted authority.
/// No public constructor can turn an unverified request into this value.
pub struct VerifiedClientHello {
    credential: VerifiedTokenCapability<SessionClaims>,
    dataset: CloudReplicaPrincipalV1,
}

impl VerifiedClientHello {
    pub fn verify(
        verifier: &TokenCapabilityVerifier,
        alpn: &[u8],
        hello: &HandshakeV1,
        client: ReplicationEndpointId,
        now_millis: i64,
    ) -> Result<Self, Refusal> {
        if alpn != CLOUD_ALPN_V1 {
            return Err(Refusal::PrincipalNotAllowed);
        }
        let HandshakeV1::ClientHello {
            dataset,
            credential,
            ..
        } = hello
        else {
            return Err(Refusal::Malformed);
        };
        let credential = verify_session_credential(verifier, credential.as_bytes(), now_millis)?;
        if credential.claims().client_endpoint_id != client {
            return Err(Refusal::EndpointMismatch);
        }
        Ok(Self {
            credential,
            dataset: *dataset,
        })
    }

    pub fn claims(&self) -> &SessionClaims {
        self.credential.claims()
    }

    pub fn dataset(&self) -> CloudReplicaPrincipalV1 {
        self.dataset
    }
}

/// One owned stream challenge; cannot be cloned, serialized or resumed.
pub struct PendingHandshake {
    credential: VerifiedTokenCapability<SessionClaims>,
    dataset: CloudReplicaPrincipalV1,
    client: ReplicationEndpointId,
    worker: ReplicationEndpointId,
    transcript: Vec<u8>,
    started_millis: u64,
}

impl PendingHandshake {
    pub fn begin(
        verifier: &TokenCapabilityVerifier,
        provider: &dyn Authority,
        alpn: &[u8],
        hello: HandshakeV1,
        client: ReplicationEndpointId,
        worker: ReplicationEndpointId,
        worker_nonce: [u8; 32],
        now_millis: i64,
        started_millis: u64,
    ) -> Result<(Self, HandshakeV1), Refusal> {
        let verified =
            VerifiedClientHello::verify(verifier, alpn, &hello, client, now_millis)?.credential;
        let HandshakeV1::ClientHello {
            dataset,
            client_nonce,
            credential,
            ..
        } = hello
        else {
            return Err(Refusal::Malformed);
        };
        let current = provider.current(verified.claims(), dataset)?;
        authority(verified.claims(), dataset, &current, client)?;
        serving_endpoint(&current, worker)?;
        let transcript = transcript(
            client,
            worker,
            dataset,
            client_nonce.0,
            worker_nonce,
            &credential,
        );
        Ok((
            Self {
                credential: verified,
                dataset,
                client,
                worker,
                transcript,
                started_millis,
            },
            HandshakeV1::WorkerChallenge {
                version: wire::Version1,
                server_nonce: Bytes(worker_nonce),
            },
        ))
    }

    pub fn finish(
        self,
        provider: &dyn Authority,
        finish: HandshakeV1,
        now_millis: i64,
        monotonic_millis: u64,
        session_id: [u8; 32],
        generation: u64,
    ) -> Result<PreparedSession, Refusal> {
        if monotonic_millis
            .checked_sub(self.started_millis)
            .is_none_or(|elapsed| elapsed >= HANDSHAKE_DEADLINE_MILLIS)
        {
            return Err(Refusal::Timeout);
        }
        let HandshakeV1::ClientFinish { signature, .. } = finish else {
            return Err(Refusal::Malformed);
        };
        self.credential.verify_time_at(now_millis)?;
        let current = provider.current(self.credential.claims(), self.dataset)?;
        authority(
            self.credential.claims(),
            self.dataset,
            &current,
            self.client,
        )?;
        serving_endpoint(&current, self.worker)?;
        if !current
            .device_key
            .verify_message(&self.transcript, SignatureBytes::from_bytes(signature.0))
        {
            return Err(Refusal::Replay);
        }
        let latest = provider.current(self.credential.claims(), self.dataset)?;
        authority(self.credential.claims(), self.dataset, &latest, self.client)?;
        serving_endpoint(&latest, self.worker)?;
        if generation == 0 {
            return Err(Refusal::Malformed);
        }
        // D231: the credential expiry is the whole advertised upper bound. The
        // shorter live lease, readiness and authority deadlines still close the
        // session independently; none of them is extended to the credential.
        let expires_at_seconds = self.credential.claims().exp;
        Ok(PreparedSession {
            accepted: HandshakeV1::SessionAccepted {
                version: wire::Version1,
                session_id: Bytes(session_id),
                generation,
                expires_at_seconds,
            },
            principal: self.dataset,
            expires_at_seconds,
            generation,
        })
    }
}

/// Not a data-plane permit. The transport must finish writing the acknowledgement
/// before handing this state to its generation owner. Transfer remains gated.
pub struct PreparedSession {
    pub accepted: HandshakeV1,
    pub principal: CloudReplicaPrincipalV1,
    pub expires_at_seconds: i64,
    pub generation: u64,
}

impl PreparedSession {
    pub fn admit_transfer(&self) -> Result<(), Refusal> {
        Err(Refusal::SyncUnavailable)
    }
}

#[cfg(test)]
mod tests;
