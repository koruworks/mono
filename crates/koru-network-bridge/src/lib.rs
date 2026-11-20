// SPDX-License-Identifier: MPL-2.0

//! Private desktop bridge contracts. Payload bytes remain opaque here.

use std::fmt;

use koru_network_edge::TransportSession;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub mod wire;

/// 2 advances with [`wire::WIRE_VERSION`] for the HOST-CONTROL §3 principal
/// and target union. All the internal launch/build-stamp compatibility checks
/// advance together, and a mismatch between a local Agent and gateway is a
/// visible [`BridgeRefusal::Version`] on that machine — never a silent
/// downgrade and never a sync failure attributed to the network.
pub const BRIDGE_VERSION: u32 = 2;
pub const SESSION_CAPACITY: usize = 32;
pub const DATA_FRAME_CAPACITY: usize = 8;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeCapability(String);

impl BridgeCapability {
    pub fn generate() -> Self {
        Self(format!(
            "koru_bridge_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BridgeCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BridgeCapability([REDACTED])")
    }
}

/// The session host's narrow lifecycle secret, as the Agent passes it on.
///
/// It rides the Gateway's launch bootstrap because that is the one channel
/// which is already a parent-to-child pipe rather than a command line: a
/// command line is visible in every process listing on the machine and a pipe
/// between a parent and its child is not — the same reason
/// [`BridgeCapability`] travels this way.
///
/// It authorizes exactly one session-host request, closing the sessions one
/// Koru workspace owns. It is deliberately **not** the host's `StopHost`
/// authority; the Agent mints two values and hands only this one on.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLifecycleAuthority(String);

impl SessionLifecycleAuthority {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionLifecycleAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionLifecycleAuthority([REDACTED])")
    }
}

/// Separate per-Agent-run capability for ordinary Run authority lookup.
/// It is neither bridge ingress nor session lifecycle authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAuthorityCredential(String);
impl RunAuthorityCredential {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for RunAuthorityCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RunAuthorityCredential([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchBootstrap {
    /// Agent-owned device identity. Older Agents omit it; Bots remain unavailable.
    /// This is launch metadata, not part of the bridge handshake identity.
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub run_authority: Option<RunAuthorityCredential>,
    /// Separate engine/gateway control capability. Never handed to providers
    /// or Bot workers; absent only in an unsupervised gateway.
    #[serde(default)]
    pub bots_engine: Option<BotEngineAuthority>,
    pub launch_id: String,
    pub bridge_version: u32,
    pub channel: String,
    pub root_fingerprint: String,
    pub installation_fingerprint: String,
    pub capability: BridgeCapability,
    /// Absent when this Agent supervises no session host — a development
    /// checkout without the sibling binary. The Gateway then serves no
    /// workspace-lifecycle operation rather than guessing a secret.
    #[serde(default)]
    pub session_lifecycle: Option<SessionLifecycleAuthority>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotEngineAuthority(String);
impl BotEngineAuthority {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for BotEngineAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotEngineAuthority([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeIdentity {
    pub launch_id: String,
    pub bridge_version: u32,
    pub channel: String,
    pub root_fingerprint: String,
    pub installation_fingerprint: String,
}

impl LaunchBootstrap {
    pub fn identity(&self) -> BridgeIdentity {
        BridgeIdentity {
            launch_id: self.launch_id.clone(),
            bridge_version: self.bridge_version,
            channel: self.channel.clone(),
            root_fingerprint: self.root_fingerprint.clone(),
            installation_fingerprint: self.installation_fingerprint.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeRefusal {
    Capability,
    Launch,
    Version,
    Channel,
    Root,
    Installation,
    AlreadyRegistered,
    Saturated,
}

pub struct BridgeAuthority {
    expected: BridgeIdentity,
    capability_hash: [u8; 32],
    registered: bool,
}

impl BridgeAuthority {
    pub fn new(bootstrap: &LaunchBootstrap) -> Self {
        Self {
            expected: bootstrap.identity(),
            capability_hash: token_hash(bootstrap.capability.expose()),
            registered: false,
        }
    }

    pub fn register(
        &mut self,
        identity: &BridgeIdentity,
        capability: &BridgeCapability,
    ) -> Result<BridgeIngress, BridgeRefusal> {
        if self.registered {
            return Err(BridgeRefusal::AlreadyRegistered);
        }
        if self.capability_hash != token_hash(capability.expose()) {
            return Err(BridgeRefusal::Capability);
        }
        if identity.launch_id != self.expected.launch_id {
            return Err(BridgeRefusal::Launch);
        }
        if identity.bridge_version != self.expected.bridge_version {
            return Err(BridgeRefusal::Version);
        }
        if identity.channel != self.expected.channel {
            return Err(BridgeRefusal::Channel);
        }
        if identity.root_fingerprint != self.expected.root_fingerprint {
            return Err(BridgeRefusal::Root);
        }
        if identity.installation_fingerprint != self.expected.installation_fingerprint {
            return Err(BridgeRefusal::Installation);
        }
        self.registered = true;
        let (sender, receiver) = mpsc::channel(SESSION_CAPACITY);
        Ok(BridgeIngress { sender, receiver })
    }
}

fn token_hash(value: &str) -> [u8; 32] {
    *blake3::hash(value.as_bytes()).as_bytes()
}

pub struct BridgeIngress {
    sender: mpsc::Sender<TransportSession>,
    receiver: mpsc::Receiver<TransportSession>,
}

impl BridgeIngress {
    pub fn admit(&self, session: TransportSession) -> Result<(), BridgeRefusal> {
        self.sender
            .try_send(session)
            .map_err(|_| BridgeRefusal::Saturated)
    }

    pub async fn accepted(&mut self) -> Option<TransportSession> {
        self.receiver.recv().await
    }

    pub fn close(&mut self) {
        self.receiver.close();
    }
}

pub fn fingerprint(value: impl AsRef<[u8]>) -> String {
    blake3::hash(value.as_ref()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_device::{DeviceId, RelationshipId, ReplicationEndpointSecret};
    use koru_network_edge::{DevicePrincipal, NetworkPrincipal, ProtocolToken};

    fn bootstrap() -> LaunchBootstrap {
        LaunchBootstrap {
            device_id: None,
            run_authority: None,
            bots_engine: None,
            launch_id: uuid::Uuid::new_v4().to_string(),
            bridge_version: BRIDGE_VERSION,
            channel: "internal-debug".into(),
            root_fingerprint: fingerprint("root"),
            installation_fingerprint: fingerprint("installation"),
            capability: BridgeCapability::generate(),
            session_lifecycle: Some(SessionLifecycleAuthority::new(
                uuid::Uuid::new_v4().to_string(),
            )),
        }
    }

    #[test]
    fn old_bootstrap_omits_bot_identity_without_changing_bridge_identity() {
        let original = bootstrap();
        let mut encoded = serde_json::to_value(&original).unwrap();
        encoded.as_object_mut().unwrap().remove("device_id");
        let legacy: LaunchBootstrap = serde_json::from_value(encoded).unwrap();
        assert_eq!(legacy.device_id, None);
        let mut current = legacy.clone();
        current.device_id = Some(uuid::Uuid::new_v4().to_string());
        assert_eq!(legacy.identity(), current.identity());
    }

    #[test]
    fn engine_control_authority_is_optional_redacted_and_not_bridge_identity() {
        let mut current = bootstrap();
        current.bots_engine = Some(BotEngineAuthority::new("private-engine-capability".into()));
        assert!(!format!("{current:?}").contains("private-engine-capability"));
        let mut encoded = serde_json::to_value(&current).unwrap();
        encoded.as_object_mut().unwrap().remove("bots_engine");
        let old: LaunchBootstrap = serde_json::from_value(encoded).unwrap();
        assert!(old.bots_engine.is_none());
        assert_eq!(old.identity(), current.identity());
    }

    fn principal() -> NetworkPrincipal {
        let endpoint = ReplicationEndpointSecret::generate();
        DevicePrincipal {
            device_id: DeviceId::new(),
            relationship_id: RelationshipId::new(),
            endpoint_id: endpoint.endpoint_id(),
            authorization_revision: 1,
        }
        .into()
    }

    #[tokio::test]
    async fn registration_is_instance_bound_one_time_and_credentials_are_redacted() {
        let bootstrap = bootstrap();
        assert!(!format!("{bootstrap:?}").contains(bootstrap.capability.expose()));
        let mut authority = BridgeAuthority::new(&bootstrap);
        let mut wrong = bootstrap.identity();
        wrong.launch_id = "another-launch".into();
        assert!(matches!(
            authority.register(&wrong, &bootstrap.capability),
            Err(BridgeRefusal::Launch)
        ));
        let _bridge = authority
            .register(&bootstrap.identity(), &bootstrap.capability)
            .unwrap();
        assert!(matches!(
            authority.register(&bootstrap.identity(), &bootstrap.capability),
            Err(BridgeRefusal::AlreadyRegistered)
        ));
    }

    /// **A bridge version mismatch is a visible refusal, not a downgrade.**
    /// HOST-CONTROL §3 moves both halves of the bridge to 2 together; an Agent
    /// or gateway still on 1 is refused here, on this machine, before a single
    /// session crosses. ACCEPTANCE.md lists that refusal so it is not read as a
    /// Cloud sync failure.
    #[tokio::test]
    async fn a_bridge_on_the_previous_version_is_refused_rather_than_accepted() {
        assert_eq!(BRIDGE_VERSION, 2);
        assert_eq!(wire::WIRE_VERSION, 2);
        let bootstrap = bootstrap();
        let mut authority = BridgeAuthority::new(&bootstrap);
        let mut previous = bootstrap.identity();
        previous.bridge_version = BRIDGE_VERSION - 1;
        assert!(matches!(
            authority.register(&previous, &bootstrap.capability),
            Err(BridgeRefusal::Version)
        ));
        // The refusal does not consume the one-time registration: the matching
        // half still registers once it is the same build.
        assert!(
            authority
                .register(&bootstrap.identity(), &bootstrap.capability)
                .is_ok()
        );
    }

    #[tokio::test]
    async fn data_is_bounded_ordered_half_closable_and_never_replayed() {
        let bootstrap = bootstrap();
        let mut authority = BridgeAuthority::new(&bootstrap);
        let mut bridge = authority
            .register(&bootstrap.identity(), &bootstrap.capability)
            .unwrap();
        let (remote, gateway) = TransportSession::pair(
            principal(),
            principal(),
            ProtocolToken::parse("koru/replication/1").unwrap(),
            DATA_FRAME_CAPACITY,
        );
        remote.send.send(vec![1]).await.unwrap();
        remote.send.send(vec![2]).await.unwrap();
        bridge.admit(gateway).unwrap();
        let mut accepted = bridge.accepted().await.unwrap();
        assert_eq!(accepted.receive.recv().await.unwrap(), Some(vec![1]));
        assert_eq!(accepted.receive.recv().await.unwrap(), Some(vec![2]));
        remote.reset(77);
        drop(remote);
        assert!(accepted.receive.recv().await.is_err());
        bridge.close();
        assert!(bridge.accepted().await.is_none());
    }
}
