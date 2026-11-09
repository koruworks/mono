// SPDX-License-Identifier: MPL-2.0

//! The narrow control contract between the device-owning Agent and Store.
//!
//! Workspace bytes never appear here. The same state machine is usable through
//! desktop IPC or directly by a mobile host.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use koru_device::cloud::{CloudId, CloudPrincipalV1, CloudReplicaPrincipalV1};
use koru_device::{DeviceId, RelationshipId, ReplicationEndpointId};
use koru_store_core::{DocumentId, ProfileId, WorkspaceId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;
use zeroize::Zeroize;

use cloud::{
    CloudAccountBindingRef, CloudBlobAccessV1, CloudNetworkCost, CloudPermitScopeV2, CloudPermitV1,
    CloudPermitV2, CloudRootExport, CloudRootJoin, CloudRootUnitState, CloudStage, CloudStatus,
    CloudStatusRecord, CloudTransferDecision, MAX_CLOUD_PERMIT_PAGE, MAX_CLOUD_PERMITS,
    MAX_CLOUD_ROOT_FRAMES,
};
use koru_store_core::service::JoinUnit;

/// 6 fenced mixed builds before Cloud provenance/principal state could cross
/// control. 7 carries HOST-CONTROL §3's seven Cloud operations and the
/// principal union at the edge below it; the bridge advances to 2 in the same
/// step. Framing stays 1 and bilateral peer protocol stays 5 (D180).
///
/// A local Agent and gateway on different numbers refuse each other visibly,
/// on that machine, through [`ControlGreeting::Refused`]. That is not a sync
/// failure and it is not a remote compatibility rule: the Cloud ALPN, the
/// handshake version and [`PEER_PROTOCOL_VERSION`] govern what crosses a
/// network (D233).
pub const CONTROL_PROTOCOL_VERSION: u32 = 7;
pub const CONTROL_FRAME_VERSION: u8 = 1;
pub mod cloud;
pub const CONTROL_SERVICE: &str = "koru-store-sync";

/// The peer-to-peer protocol number — the fourth schema version in the tree,
/// beside [`CONTROL_PROTOCOL_VERSION`] (agent ↔ store), the agent's
/// `PROTOCOL_VERSION` (window ↔ agent) and the identity database's
/// `DB_VERSION`. It is declared here rather than in `koru-replication`
/// because the agent, which signs the build record that advertises it, links
/// this crate and not iroh; `koru_replication::protocol` re-exports it.
///
/// Every device advertises the number it speaks in its signed build record,
/// and two devices on different numbers **do not sync**: the replication
/// runtime starts no sync pass toward such a peer and answers its data-plane
/// requests with `profile_units_required` for older/unknown peer protocols or
/// `incompatible_protocol` for newer protocols, while the connection stays up for
/// trust gossip — which is how the older side learns that it is the one that
/// has to update. The device lists say *Not syncing · update …*.
///
/// **Bump it for any change after which an older peer cannot sync
/// correctly.** The 256 KB fragmentation, and the `device_build` record kind
/// landing before `TrustRecord::Unknown` existed, would both have been bumps.
/// It is deliberately *not* the iroh ALPN (`koru/replication/1`): that names
/// the framing, and changing it refuses the connection itself — a break even
/// trust gossip cannot cross, with nothing left to tell the user why. Bump
/// the ALPN only for a change to the frames.
///
/// 1 is the first number; builds from before it advertise none and are
/// compared on their versions alone. 2 adds profile icon/colour fields to the
/// catalog and workspace bootstrap frames; an older decoder cannot safely
/// consume those postcard structs, so mixed builds stay linked but do not sync.
/// 3 makes catalog digests scope-bound and enforces Profile grants throughout
/// catalog, Profile, Workspace, Loro, and blob replication. Older peers stay
/// connected for trust gossip but cannot safely exchange data-plane frames.
/// 4 adds Profile kind to full Profile/bootstrap metadata. Scoped peers still
/// receive no Profile metadata, including kind.
/// 5 requires independent identity, private, personal and legacy Profile unit
/// addresses. Exact Profile grants admit fresh identity/private state; legacy
/// bytes still require all-data. Older peers receive profile_units_required.
/// 6 adds the root-catalog exchange (root-catalog TECHNICAL.final §4, §7.2,
/// §11.4): per-subject Profile/Workspace catalog units and personal v2 units
/// travel as their own frames through the native-redaction export boundary,
/// a descriptor's Profile is a bootstrap placement hint rather than
/// authority, and placement is read from the root unit. A 5 peer still
/// writes `workspaces.owner` from descriptors and exports personal units on
/// the v1 address, so it receives `root_catalog_required`; 0..=4 keep
/// `profile_units_required`.
pub const PEER_PROTOCOL_VERSION: u32 = 6;

/// A sanity bound on one gossip batch, not a capacity limit: a trust store
/// holds a handful of records per paired device, so anything near this is a
/// bug or an attack, not a large mesh.
pub const MAX_TRUST_GOSSIP_RECORDS: usize = 4096;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ControlSessionId(Uuid);

impl ControlSessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// `CloudPrincipalV1` carries this id as a plain UUID, because
    /// `koru-device` sits below this crate. Conversion happens here, at the
    /// sync-control layer, and nowhere else.
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ControlSessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlHello {
    pub protocol_version: u32,
    pub session_id: ControlSessionId,
    pub local_device: DeviceId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlGreeting {
    Welcome {
        protocol_version: u32,
    },
    Refused {
        ours: u32,
        theirs: u32,
        code: String,
    },
}

pub struct EndpointSecretBytes([u8; 32]);

impl EndpointSecretBytes {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn into_bytes(mut self) -> [u8; 32] {
        let bytes = self.0;
        self.0.zeroize();
        bytes
    }
}

impl fmt::Debug for EndpointSecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EndpointSecretBytes([REDACTED])")
    }
}

impl Drop for EndpointSecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl Serialize for EndpointSecretBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for EndpointSecretBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let bytes = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("endpoint secret must be 32 bytes"))?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationSnapshot {
    pub session_id: ControlSessionId,
    pub sequence: u64,
    pub local_device: DeviceId,
    pub peers: Vec<AuthorizedPeer>,
    /// What the local device runs, beside each peer's `build`, so the
    /// runtime can say `build_mismatch` on connect without linking build
    /// info itself — it is shared with a phone that has none.
    #[serde(default)]
    pub local_build: Option<koru_device::BuildPayload>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedPeer {
    pub relationship_id: RelationshipId,
    pub device_id: DeviceId,
    pub endpoint_id: ReplicationEndpointId,
    #[serde(default)]
    pub all_data: bool,
    /// Profiles authorized by reciprocal Profile grants. Kept separate from
    /// explicit workspace grants so one representation cannot imply another.
    #[serde(default)]
    pub authorized_profiles: Vec<ProfileId>,
    pub workspaces: Vec<WorkspaceId>,
    /// The directly paired device that introduced this peer, when the local
    /// device has no pairing of its own with it. Provenance only: the data
    /// plane authorizes on the endpoint, never on the path.
    #[serde(default)]
    pub via: Option<DeviceId>,
    /// The peer's display name from the trust records, for logs and status
    /// surfaces. Carries no authority — identity is the endpoint.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The peer's latest self-signed build description, for the connect log
    /// line. Carries no authority either.
    #[serde(default)]
    pub build: Option<koru_device::BuildPayload>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PairingAttemptId(Uuid);

impl PairingAttemptId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrustDeliveryId(Uuid);

impl TrustDeliveryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TrustDeliveryId {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for PairingAttemptId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingEvent {
    JoinRequested {
        attempt_id: PairingAttemptId,
        join: Box<koru_pairing::PairingJoinV1>,
    },
    Completed {
        prepared: Box<koru_pairing::PreparedPairingV1>,
    },
    Failed {
        code: String,
    },
    TrustRecordReceived {
        delivery_id: TrustDeliveryId,
        from: DeviceId,
        record: Box<koru_device::TrustRecord>,
    },
    /// A gossip batch: the sender's whole trust-record set in canonical
    /// [`koru_device::sort_for_gossip`] order, so the receiving Agent can
    /// verify it in one pass. One delivery id covers the batch.
    TrustRecordsReceived {
        delivery_id: TrustDeliveryId,
        from: DeviceId,
        records: Vec<koru_device::TrustRecord>,
    },
}

/// **An unknown field or an unknown variant closes admission.** Control 7 is
/// exact on this wire: both halves are one local build, so a field this
/// decoder does not know is a mismatch to refuse rather than a forward
/// compatibility affordance to ignore (HOST-CONTROL §3).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequest {
    ProvisionEndpoint {
        secret: EndpointSecretBytes,
    },
    ReplaceAuthorization {
        snapshot: AuthorizationSnapshot,
    },
    ConnectPeer {
        peer: DeviceId,
    },
    DisconnectPeer {
        peer: DeviceId,
    },
    BeginPairing {
        invitation: Box<koru_pairing::PairingInvitationV1>,
    },
    CancelPairing,
    JoinPairing {
        join: Box<koru_pairing::PairingJoinV1>,
    },
    CompletePairing {
        attempt_id: PairingAttemptId,
        prepared: Box<koru_pairing::PreparedPairingV1>,
    },
    SendTrustRecord {
        peer: DeviceId,
        record: Box<koru_device::TrustRecord>,
    },
    /// Gossip the device list itself: the sender's whole trust-record set,
    /// in canonical order, so the mesh converges on one graph. Unlike
    /// [`ControlRequest::SendTrustRecord`], which carries a revocation and
    /// tears the peer down, a batch never interrupts the connection.
    SendTrustRecords {
        peer: DeviceId,
        records: Vec<koru_device::TrustRecord>,
    },
    CompleteTrustRecord {
        delivery_id: TrustDeliveryId,
        accepted: bool,
    },
    PairingEvents,
    StateHistory {
        peer: DeviceId,
        before: Option<i64>,
    },
    Status,
    // ---- Control 7: the seven Cloud operations (HOST-CONTROL §3) ----
    /// An upsert page of at most [`MAX_CLOUD_PERMIT_PAGE`] permits for one
    /// account. Omitted permits in a page are **not** revocations; an explicit
    /// `fence_cloud` or an account-generation replacement revokes.
    ReplaceCloudContext {
        control_session_id: ControlSessionId,
        sequence: u64,
        account_id: CloudId,
        account_generation: u64,
        /// The v1 shape, kept on the wire for the compatibility window of
        /// root-catalog Plan 3.2 and read as v2 at the boundary
        /// ([`CloudPermitV2::from_v1`]). The host holds v2 permits only.
        permits: Vec<CloudPermitV1>,
        /// The v2 permits (root-catalog Plan 3.2): one scope each, every one
        /// with a dataset — the two catalog scopes never reach the host,
        /// because no bridged stream runs under them.
        #[serde(default)]
        permits_v2: Vec<CloudPermitV2>,
    },
    /// The explicit barrier before any content stream attaches. Two IPC
    /// channels are not assumed ordered, so nothing may be forwarded until
    /// this is acknowledged.
    InstallCloudSession {
        principal: Box<CloudPrincipalV1>,
        connection_id: u64,
        stream_session_id: u64,
    },
    /// `permit_ids: None` fences every permit for the account; an empty list
    /// is a no-op; a foreign-account id is refused.
    FenceCloud {
        control_session_id: ControlSessionId,
        sequence: u64,
        account_id: CloudId,
        account_generation: u64,
        permit_ids: Option<Vec<Uuid>>,
    },
    CloudStatus {
        control_session_id: ControlSessionId,
    },
    DecideCloudTransfer {
        control_session_id: ControlSessionId,
        consent_id: Uuid,
        decision: CloudTransferDecision,
        expected_network_generation: i64,
    },
    /// Where the blob **control** plane is and the account bearer token that
    /// opens it (`cloud-blob-object-v1` §3).
    ///
    /// The account lives in the Agent and the Store lives here, and a blob
    /// transfer needs both. Sequenced like every other Cloud command, so a
    /// replay is a repeat and a reordering is refused; replaced rather than
    /// merged, because a token is one value and half of one is nothing.
    ReplaceCloudBlobAccess {
        control_session_id: ControlSessionId,
        sequence: u64,
        account_id: CloudId,
        account_generation: u64,
        access: Box<CloudBlobAccessV1>,
    },
    ReplaceCloudNetworkPolicy {
        control_session_id: ControlSessionId,
        sequence: u64,
        network_generation: i64,
        cost: CloudNetworkCost,
        /// The device-local "Sync only on Wi-Fi" setting of TECHNICAL §4.5,
        /// carried as the approved `cloud_sync_wifi_only` boolean rather than
        /// a newly invented preference enum. A desktop, which has no such
        /// setting, always reports `false`.
        wifi_only: bool,
    },
    /// The originating device's own manifest identity, for a first upload.
    /// A single Workspace dataset, never an inventory; a Profile dataset is
    /// an invalid request.
    ReadCloudBootstrap {
        control_session_id: ControlSessionId,
        account_id: CloudId,
        profile_id: CloudId,
        dataset: CloudReplicaPrincipalV1,
    },
    // ---- Root-catalog Plan 3.2: the Store half of the account path ----
    /// `read_cloud_bootstrap` without a Profile (TECHNICAL.final §7.3 "The
    /// former Profile is not a prerequisite"): the originating device's
    /// manifest identity and whether its own transport policy for the
    /// Workspace admits the Cloud path.
    ReadCloudBootstrapV2 {
        control_session_id: ControlSessionId,
        account_id: CloudId,
        dataset: CloudReplicaPrincipalV1,
    },
    /// `Store::bind_account_scope` with the service's verified binding. A
    /// device bound to another account answers `catalog_binding_conflict`
    /// and nothing is written.
    BindCloudAccountScope {
        control_session_id: ControlSessionId,
        account_id: CloudId,
        catalog_scope_id: CloudId,
        bound_at_ms: i64,
    },
    /// The binding this device holds, to declare to the service.
    ReadCloudAccountBinding {
        control_session_id: ControlSessionId,
    },
    /// The publication job, then the eligible root inventory over the Cloud
    /// path (`RootGrant::account()`): units with their frontiers and the
    /// count of units held back.
    ReadCloudRootInventory {
        control_session_id: ControlSessionId,
    },
    /// The frontier this device holds for one unit, for a pull request.
    ReadCloudRootFrontier {
        control_session_id: ControlSessionId,
        unit: JoinUnit,
    },
    /// One unit through the export boundary for the account: prepare under
    /// the lock, derive off it, recheck right before the answer.
    ExportCloudRootUnit {
        control_session_id: ControlSessionId,
        unit: JoinUnit,
        /// The service's frontier for the unit; `None` asks for a snapshot.
        recipient_frontier: Option<Vec<u8>>,
    },
    /// The staged join of the service's frames for one unit, with the
    /// account id as the relationship.
    JoinCloudRootUnit {
        control_session_id: ControlSessionId,
        account_id: CloudId,
        unit: JoinUnit,
        frames: Vec<Vec<u8>>,
        /// The frontier the service advertised, so the receipt can say
        /// `reconciled`.
        advertised: Option<Vec<u8>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlResponse {
    Ok,
    AuthorizationApplied {
        session_id: ControlSessionId,
        sequence: u64,
    },
    Status(ControlStatus),
    PairingEvents {
        events: Vec<PairingEvent>,
    },
    StateHistory {
        peer: DeviceId,
        events: Vec<StateChangeEvent>,
        next_before: Option<i64>,
    },
    CloudContextApplied {
        control_session_id: ControlSessionId,
        sequence: u64,
    },
    CloudSessionInstalled {
        permit_id: Uuid,
        session_generation: u64,
    },
    CloudFenced {
        control_session_id: ControlSessionId,
        sequence: u64,
    },
    CloudStatus(CloudStatus),
    CloudTransferDecided {
        consent_id: Uuid,
        decision: CloudTransferDecision,
        network_generation: i64,
    },
    CloudNetworkPolicyApplied {
        control_session_id: ControlSessionId,
        sequence: u64,
        network_generation: i64,
    },
    CloudBlobAccessApplied {
        control_session_id: ControlSessionId,
        sequence: u64,
        expires_at_millis: i64,
    },
    CloudBootstrap {
        control_session_id: ControlSessionId,
        /// Null means **not ready**, never empty content: the daemon returns
        /// an id only when a real durable local manifest identity exists.
        manifest_document_id: Option<CloudId>,
        /// Whether this device's own transport policy for the Workspace
        /// admits the Cloud path (`ReadCloudBootstrapV2` only; `None` for a
        /// v1 read, or when the Store holds no mode row for it).
        #[serde(default)]
        allows_cloud: Option<bool>,
    },
    CloudAccountScopeBound {
        control_session_id: ControlSessionId,
    },
    CloudAccountBinding {
        control_session_id: ControlSessionId,
        binding: Option<CloudAccountBindingRef>,
    },
    CloudRootInventory {
        control_session_id: ControlSessionId,
        units: Vec<CloudRootUnitState>,
        held: u32,
    },
    CloudRootFrontier {
        control_session_id: ControlSessionId,
        frontier: Option<Vec<u8>>,
    },
    CloudRootUnitExport {
        control_session_id: ControlSessionId,
        export: CloudRootExport,
    },
    CloudRootUnitJoined {
        control_session_id: ControlSessionId,
        join: CloudRootJoin,
    },
    Rejected {
        code: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlStatus {
    pub replication_ready: bool,
    pub endpoint_id: Option<ReplicationEndpointId>,
    pub authorization_sequence: u64,
    pub authorized_peers: usize,
    pub authorized_workspaces: usize,
    pub desired_connections: usize,
    #[serde(default)]
    pub peers: Vec<PeerStatus>,
    #[serde(default)]
    pub sync_event: Option<SyncEvent>,
    /// The organization (root-catalog) status per subject and peer, as the
    /// last sync pass with each connected peer left it (root-catalog
    /// TECHNICAL.final §4.4, §7.2, §9; Plan 2.2). Identifiers and states
    /// only: no name, no mode row, no hidden subject.
    #[serde(default)]
    pub root_sync: Vec<RootSyncSubject>,
}

/// **What a person is told about one Profile's or Workspace's organization
/// sync with one peer.** Read from the Store's receipts and admissions by
/// the sync pass, never from a document's hydration: a Workspace whose root
/// unit has not arrived is pending however many of its documents have.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootSyncState {
    /// Every eligible unit the peer advertised is applied here.
    UpToDate,
    /// Root units, a placement or an overlay publication are still owed:
    /// `placement_pending`, `root_unit_pending`, a reconciled-short join, a
    /// private overlay waiting to be published.
    Pending,
    /// This device's own mode row closes the peer path for the subject
    /// (`mode_refuses_transport`): nothing moves until the person changes
    /// the subject's sync setting here.
    PausedBySettings,
    /// The Workspace's stored choice is above its Profile's ceiling
    /// (`ceiling_mismatch`): the transport is stopped until the existing
    /// correction path changes a row.
    BlockedByMismatch,
    /// The peer holds the unit back for privacy (`root_unit_held`). The
    /// subject this device asked about is named; what hides it never is.
    Held,
}

impl RootSyncState {
    /// Which of two readings a person should see when both are true of one
    /// subject: a stop beats a wait, a wait beats done.
    pub fn worse(self, other: Self) -> Self {
        fn rank(state: RootSyncState) -> u8 {
            match state {
                RootSyncState::UpToDate => 0,
                RootSyncState::Pending => 1,
                RootSyncState::Held => 2,
                RootSyncState::PausedBySettings => 3,
                RootSyncState::BlockedByMismatch => 4,
            }
        }
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// One subject's reading with one peer. Exactly one of `profile` and
/// `workspace` is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSyncSubject {
    pub peer: DeviceId,
    #[serde(default)]
    pub profile: Option<ProfileId>,
    #[serde(default)]
    pub workspace: Option<WorkspaceId>,
    pub state: RootSyncState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncEvent {
    pub sequence: u64,
    pub at_millis: i64,
    pub peer: DeviceId,
    pub stage: String,
    pub workspace: Option<WorkspaceId>,
    pub items: Option<usize>,
    pub error_code: Option<String>,
}

/// One remote event that crossed a state-changing boundary. This intentionally
/// excludes transient progress stages and carries identifiers, never content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChangeEvent {
    pub event_id: Option<i64>,
    pub sequence: u64,
    pub at_millis: i64,
    pub peer: DeviceId,
    pub kind: String,
    pub profile: Option<ProfileId>,
    pub workspace: Option<WorkspaceId>,
    pub document: Option<DocumentId>,
    pub document_name: Option<String>,
    pub blob_hash: Option<String>,
    pub detail: Option<String>,
    pub items: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStatus {
    pub device_id: DeviceId,
    pub state: PeerState,
    pub last_seen_millis: Option<i64>,
    #[serde(default)]
    pub last_state_change_millis: Option<i64>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerState {
    Disconnected,
    Connecting,
    ConnectedDirect,
    ConnectedRelay,
    ConnectedUnknown,
    BackingOff,
    Unauthorized,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: u64,
    pub request: ControlRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub id: u64,
    pub response: ControlResponse,
}

#[derive(Default)]
pub struct ControlHost {
    session: Option<ControlSessionId>,
    local_device: Option<DeviceId>,
    endpoint_secret: Option<EndpointSecretBytes>,
    endpoint_id: Option<ReplicationEndpointId>,
    authorization: Option<AuthorizationSnapshot>,
    desired_connections: BTreeSet<DeviceId>,
    cloud: CloudBookkeeping,
}

/// Everything control 7 holds for Cloud, and all of it is process memory.
/// A restart takes a new [`ControlSessionId`], which invalidates the sequence
/// space, every permit, every installed session generation and the status
/// snapshot. There is no session database (HOST-CONTROL §5).
#[derive(Default)]
struct CloudBookkeeping {
    /// The strictly increasing sequence space, shared by every sequenced Cloud
    /// command within one control session, with the body that claimed it so a
    /// retry can be told apart from a conflict.
    last_sequenced: Option<(u64, CloudSequenced)>,
    /// The current account generation per account. Other held accounts retain
    /// their own context and generation under existing selection rules.
    account_generations: BTreeMap<CloudId, u64>,
    live: BTreeMap<Uuid, LivePermit>,
    network: Option<CloudNetworkPolicy>,
    /// The account's reach into the blob control plane, for the account this
    /// host currently holds. Memory only, like everything else here: a bearer
    /// token that survived a restart would be a credential this process
    /// stored, and this process stores none.
    blob_access: Option<(CloudId, u64, CloudBlobAccessV1)>,
    revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LivePermit {
    permit: CloudPermitV2,
    /// 0 until `install_cloud_session` acknowledges one. The generation is the
    /// connection's (the worker allocates it from a per-connection counter), so
    /// a later install on the **same** connection must name a strictly higher
    /// one and a replay is refused, while a new connection starts again at 1.
    installed_session_generation: u64,
    /// The connection the installed generation belongs to. 0 until one is
    /// installed. A different connection is a different counter, never a
    /// replay of this one.
    installed_connection_id: u64,
    stage: CloudStage,
    /// The last stable code the driver reported for this permit; cleared by
    /// the next install. Never a message, a title or a path.
    error_code: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CloudNetworkPolicy {
    generation: i64,
    cost: CloudNetworkCost,
    wifi_only: bool,
}

/// The body of a sequenced Cloud command, kept only to answer "is this the
/// same message again, or a different one claiming the same number?". Every
/// arm is bounded by [`MAX_CLOUD_PERMIT_PAGE`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum CloudSequenced {
    Context {
        account_id: CloudId,
        account_generation: u64,
        permits: Vec<CloudPermitV2>,
    },
    Fence {
        account_id: CloudId,
        account_generation: u64,
        permit_ids: Option<Vec<Uuid>>,
    },
    NetworkPolicy {
        network_generation: i64,
        cost: CloudNetworkCost,
        wifi_only: bool,
    },
    BlobAccess {
        account_id: CloudId,
        account_generation: u64,
        access: Box<CloudBlobAccessV1>,
    },
}

enum SequenceAdmission {
    Apply,
    Repeat,
    Refused(HostError),
}

impl CloudBookkeeping {
    fn classify(&self, sequence: u64, command: &CloudSequenced) -> SequenceAdmission {
        if sequence == 0 {
            return SequenceAdmission::Refused(HostError::OutOfOrder);
        }
        match &self.last_sequenced {
            None => SequenceAdmission::Apply,
            Some((last, body)) => match sequence.cmp(last) {
                std::cmp::Ordering::Greater => SequenceAdmission::Apply,
                std::cmp::Ordering::Equal if body == command => SequenceAdmission::Repeat,
                std::cmp::Ordering::Equal => {
                    SequenceAdmission::Refused(HostError::CloudSequenceConflict)
                }
                std::cmp::Ordering::Less => {
                    SequenceAdmission::Refused(HostError::StaleCloudSequence)
                }
            },
        }
    }

    fn record(&mut self, sequence: u64, command: CloudSequenced) {
        self.last_sequenced = Some((sequence, command));
        self.revision = self.revision.saturating_add(1);
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl ControlHost {
    pub fn begin_session(&mut self, hello: &ControlHello) -> Result<(), HostError> {
        if hello.protocol_version != CONTROL_PROTOCOL_VERSION {
            return Err(HostError::Protocol);
        }
        self.clear();
        self.session = Some(hello.session_id);
        self.local_device = Some(hello.local_device);
        Ok(())
    }

    pub fn end_session(&mut self, session_id: ControlSessionId) {
        if self.session == Some(session_id) {
            self.clear();
        }
    }

    pub fn handle(
        &mut self,
        session_id: ControlSessionId,
        request: ControlRequest,
    ) -> ControlResponse {
        if self.session != Some(session_id) {
            return rejected(HostError::StaleSession);
        }
        match request {
            ControlRequest::ProvisionEndpoint { secret } => {
                let endpoint =
                    koru_device::ReplicationEndpointSecret::from_secret_bytes(*secret.expose())
                        .endpoint_id();
                self.endpoint_secret = Some(secret);
                self.endpoint_id = Some(endpoint);
                ControlResponse::Ok
            }
            ControlRequest::ReplaceAuthorization { mut snapshot } => {
                if snapshot.session_id != session_id
                    || Some(snapshot.local_device) != self.local_device
                {
                    return rejected(HostError::StaleSession);
                }
                let previous = self
                    .authorization
                    .as_ref()
                    .map_or(0, |value| value.sequence);
                if snapshot.sequence <= previous {
                    return rejected(HostError::OutOfOrder);
                }
                normalize_snapshot(&mut snapshot);
                self.desired_connections
                    .retain(|device| snapshot.peers.iter().any(|peer| peer.device_id == *device));
                let sequence = snapshot.sequence;
                self.authorization = Some(snapshot);
                ControlResponse::AuthorizationApplied {
                    session_id,
                    sequence,
                }
            }
            ControlRequest::ConnectPeer { peer } => {
                if self.peer(peer).is_none() {
                    return rejected(HostError::UnauthorizedPeer);
                }
                self.desired_connections.insert(peer);
                ControlResponse::Ok
            }
            ControlRequest::DisconnectPeer { peer } => {
                self.desired_connections.remove(&peer);
                ControlResponse::Ok
            }
            ControlRequest::BeginPairing { invitation } => {
                if Some(invitation.inviter.device_id) != self.local_device
                    || invitation.inviter.endpoint_id != invitation.pairing_target.endpoint_id
                {
                    return rejected(HostError::WrongPairingDevice);
                }
                ControlResponse::Ok
            }
            ControlRequest::CancelPairing => ControlResponse::Ok,
            ControlRequest::JoinPairing { join } => {
                if Some(join.transcript.joiner.device_id) != self.local_device {
                    return rejected(HostError::WrongPairingDevice);
                }
                ControlResponse::Ok
            }
            ControlRequest::CompletePairing { prepared, .. } => {
                if Some(prepared.transcript.inviter.device_id) != self.local_device
                    || prepared.verify().is_err()
                {
                    return rejected(HostError::WrongPairingDevice);
                }
                ControlResponse::Ok
            }
            ControlRequest::SendTrustRecord { peer, record } => {
                let valid = match record.as_ref() {
                    koru_device::TrustRecord::Revocation(revocation) => {
                        Some(revocation.issued_by) == self.local_device
                            && revocation.revoked_device_id == peer
                    }
                    koru_device::TrustRecord::ProfileLifecycle(lifecycle) => {
                        self.peer(peer).is_some_and(|target| {
                            Some(lifecycle.issued_by) == self.local_device
                                && (target.all_data
                                    || target.authorized_profiles.contains(&lifecycle.profile_id))
                        })
                    }
                    _ => false,
                };
                if !valid {
                    return rejected(HostError::WrongPairingDevice);
                }
                ControlResponse::Ok
            }
            ControlRequest::SendTrustRecords { records, .. } => {
                // Gossip batches name third parties by design — that is the
                // mesh. The receiving Agent is the signature-checking
                // boundary; this side only keeps the batch sane. The peer is
                // deliberately not checked against the snapshot: a batch can
                // legitimately race a replacement that just authorized it.
                if records.is_empty() || records.len() > MAX_TRUST_GOSSIP_RECORDS {
                    return rejected(HostError::MalformedTrustGossip);
                }
                ControlResponse::Ok
            }
            ControlRequest::CompleteTrustRecord { .. } => ControlResponse::Ok,
            // Events live in the Store-hosted runtime, not in this validation
            // state. The daemon replaces this placeholder with the drained
            // runtime response.
            ControlRequest::PairingEvents => ControlResponse::PairingEvents { events: Vec::new() },
            ControlRequest::StateHistory { peer, .. } => ControlResponse::StateHistory {
                peer,
                events: Vec::new(),
                next_before: None,
            },
            ControlRequest::Status => ControlResponse::Status(self.status()),
            ControlRequest::ReplaceCloudContext {
                control_session_id,
                sequence,
                account_id,
                account_generation,
                permits,
                permits_v2,
            } => {
                // The v1 shape is read as v2 at the boundary, so the host
                // holds one permit type. A v1 permit that cannot be read as
                // v2 refuses the whole page, as a malformed v2 one does.
                let mut all = Vec::with_capacity(permits.len() + permits_v2.len());
                for permit in &permits {
                    match CloudPermitV2::from_v1(permit) {
                        Ok(converted) => all.push(converted),
                        Err(error) => return rejected_code(error.code()),
                    }
                }
                all.extend(permits_v2);
                self.replace_cloud_context(
                    session_id,
                    control_session_id,
                    sequence,
                    account_id,
                    account_generation,
                    all,
                )
            }
            ControlRequest::InstallCloudSession {
                principal,
                connection_id,
                ..
            } => self.install_cloud_session(session_id, *principal, connection_id),
            ControlRequest::FenceCloud {
                control_session_id,
                sequence,
                account_id,
                account_generation,
                permit_ids,
            } => self.fence_cloud(
                session_id,
                control_session_id,
                sequence,
                account_id,
                account_generation,
                permit_ids,
            ),
            ControlRequest::CloudStatus { control_session_id } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                ControlResponse::CloudStatus(self.cloud_status(session_id))
            }
            // The next three are validated for shape and session here and
            // answered by a placeholder the daemon replaces with the real
            // runtime result — the same pattern `PairingEvents` and
            // `StateHistory` have used since control 5. The placeholder is
            // always the *safe* answer: a deferred decision, the sequence just
            // acknowledged, and a null manifest id, which reads as not ready.
            ControlRequest::DecideCloudTransfer {
                control_session_id,
                consent_id,
                decision,
                expected_network_generation,
            } => self.decide_cloud_transfer(
                session_id,
                control_session_id,
                consent_id,
                decision,
                expected_network_generation,
            ),
            ControlRequest::ReplaceCloudBlobAccess {
                control_session_id,
                sequence,
                account_id,
                account_generation,
                access,
            } => self.replace_cloud_blob_access(
                session_id,
                control_session_id,
                sequence,
                account_id,
                account_generation,
                access,
            ),
            ControlRequest::ReplaceCloudNetworkPolicy {
                control_session_id,
                sequence,
                network_generation,
                cost,
                wifi_only,
            } => self.replace_cloud_network_policy(
                session_id,
                control_session_id,
                sequence,
                network_generation,
                cost,
                wifi_only,
            ),
            ControlRequest::ReadCloudBootstrap {
                control_session_id,
                account_id,
                profile_id,
                dataset,
            } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                if !account_id.is_v4() || !profile_id.is_v4() {
                    return rejected(HostError::InvalidCloudRequest);
                }
                // §3.1: a single Workspace dataset, not an inventory. A
                // Profile dataset has no manifest to read and is refused
                // rather than answered with null.
                if !dataset.is_workspace() {
                    return rejected(HostError::InvalidCloudRequest);
                }
                ControlResponse::CloudBootstrap {
                    control_session_id: session_id,
                    manifest_document_id: None,
                    allows_cloud: None,
                }
            }
            ControlRequest::ReadCloudBootstrapV2 {
                control_session_id,
                account_id,
                dataset,
            } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                if !account_id.is_v4() || !dataset.is_workspace() {
                    return rejected(HostError::InvalidCloudRequest);
                }
                ControlResponse::CloudBootstrap {
                    control_session_id: session_id,
                    manifest_document_id: None,
                    allows_cloud: None,
                }
            }
            // The Store half of the account path (root-catalog Plan 3.2).
            // The host validates the session and the shape; the daemon
            // answers from the Store, the only valid source, and replaces
            // these placeholders. A placeholder that reaches an Agent is a
            // host with no Store behind it, which the Agent reads as
            // unavailable, never as an empty catalog.
            ControlRequest::BindCloudAccountScope {
                control_session_id,
                account_id,
                catalog_scope_id,
                bound_at_ms,
            } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                if !account_id.is_v4() || !catalog_scope_id.is_v4() || bound_at_ms <= 0 {
                    return rejected(HostError::InvalidCloudRequest);
                }
                ControlResponse::CloudAccountScopeBound {
                    control_session_id: session_id,
                }
            }
            ControlRequest::ReadCloudAccountBinding { control_session_id } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                ControlResponse::CloudAccountBinding {
                    control_session_id: session_id,
                    binding: None,
                }
            }
            ControlRequest::ReadCloudRootInventory { control_session_id } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                ControlResponse::CloudRootInventory {
                    control_session_id: session_id,
                    units: Vec::new(),
                    held: 0,
                }
            }
            ControlRequest::ReadCloudRootFrontier {
                control_session_id, ..
            } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                ControlResponse::CloudRootFrontier {
                    control_session_id: session_id,
                    frontier: None,
                }
            }
            ControlRequest::ExportCloudRootUnit {
                control_session_id, ..
            } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                ControlResponse::CloudRootUnitExport {
                    control_session_id: session_id,
                    export: CloudRootExport::NotFound,
                }
            }
            ControlRequest::JoinCloudRootUnit {
                control_session_id,
                account_id,
                frames,
                ..
            } => {
                if control_session_id != session_id {
                    return rejected(HostError::StaleSession);
                }
                if !account_id.is_v4() || frames.is_empty() || frames.len() > MAX_CLOUD_ROOT_FRAMES
                {
                    return rejected(HostError::InvalidCloudRequest);
                }
                ControlResponse::CloudRootUnitJoined {
                    control_session_id: session_id,
                    join: CloudRootJoin::Pending {
                        frontier: Vec::new(),
                    },
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_cloud_context(
        &mut self,
        session_id: ControlSessionId,
        control_session_id: ControlSessionId,
        sequence: u64,
        account_id: CloudId,
        account_generation: u64,
        permits: Vec<CloudPermitV2>,
    ) -> ControlResponse {
        if control_session_id != session_id {
            return rejected(HostError::StaleSession);
        }
        let command = CloudSequenced::Context {
            account_id,
            account_generation,
            permits: permits.clone(),
        };
        match self.cloud.classify(sequence, &command) {
            SequenceAdmission::Refused(error) => return rejected(error),
            SequenceAdmission::Repeat => {
                return ControlResponse::CloudContextApplied {
                    control_session_id: session_id,
                    sequence,
                };
            }
            SequenceAdmission::Apply => {}
        }
        if let Err(error) = self.validate_cloud_page(account_id, account_generation, &permits) {
            return rejected_code(error);
        }
        let current = self
            .cloud
            .account_generations
            .get(&account_id)
            .copied()
            .unwrap_or(0);
        if account_generation < current {
            return rejected(HostError::StaleCloudGeneration);
        }
        // Build the whole post-state before touching anything: a page that
        // would not fit is refused outright rather than applied in part.
        let mut live = self.cloud.live.clone();
        if account_generation > current {
            // Account-generation replacement fences every older permit for
            // that account without enumerating them. Other accounts keep
            // their own context and generation.
            live.retain(|_, held| {
                held.permit.account_id != account_id
                    || held.permit.account_generation >= account_generation
            });
        }
        for permit in &permits {
            let permit_id = permit.permit_id;
            match live.entry(permit_id) {
                std::collections::btree_map::Entry::Occupied(mut held) => {
                    if held.get().permit.account_id != account_id {
                        return rejected(HostError::ForeignCloudPermit);
                    }
                    // Per-permit replacement: the authority context is
                    // replaced, the installed session generation is not
                    // forgotten, so a replayed install is still refused.
                    held.get_mut().permit = permit.clone();
                }
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(LivePermit {
                        permit: permit.clone(),
                        installed_session_generation: 0,
                        installed_connection_id: 0,
                        stage: CloudStage::AwaitingAuthority,
                        error_code: None,
                    });
                }
            }
        }
        if live.len() > MAX_CLOUD_PERMITS {
            return rejected(HostError::CloudPermitCapacity);
        }
        self.cloud.live = live;
        self.cloud
            .account_generations
            .insert(account_id, account_generation);
        self.cloud.record(sequence, command);
        ControlResponse::CloudContextApplied {
            control_session_id: session_id,
            sequence,
        }
    }

    fn validate_cloud_page(
        &self,
        account_id: CloudId,
        account_generation: u64,
        permits: &[CloudPermitV2],
    ) -> Result<(), &'static str> {
        if permits.len() > MAX_CLOUD_PERMIT_PAGE {
            return Err(HostError::CloudPermitPage.code());
        }
        if !account_id.is_v4() {
            return Err(HostError::InvalidCloudRequest.code());
        }
        if account_generation == 0 {
            return Err(HostError::StaleCloudGeneration.code());
        }
        let mut seen = BTreeSet::new();
        for permit in permits {
            permit.validate().map_err(|error| error.code())?;
            // Only a scope with a dataset has a stream this host can admit;
            // the user-catalog and user-personal scopes run over the account
            // API in the Agent and never come here.
            if permit.dataset().is_none() {
                return Err(cloud::HostCloudError::PermitScope.code());
            }
            if permit.account_id != account_id {
                return Err(HostError::ForeignCloudPermit.code());
            }
            if permit.account_generation != account_generation {
                return Err(HostError::StaleCloudGeneration.code());
            }
            if !seen.insert(permit.permit_id) {
                return Err(HostError::CloudPermitPage.code());
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn fence_cloud(
        &mut self,
        session_id: ControlSessionId,
        control_session_id: ControlSessionId,
        sequence: u64,
        account_id: CloudId,
        account_generation: u64,
        permit_ids: Option<Vec<Uuid>>,
    ) -> ControlResponse {
        if control_session_id != session_id {
            return rejected(HostError::StaleSession);
        }
        let command = CloudSequenced::Fence {
            account_id,
            account_generation,
            permit_ids: permit_ids.clone(),
        };
        match self.cloud.classify(sequence, &command) {
            SequenceAdmission::Refused(error) => return rejected(error),
            SequenceAdmission::Repeat => {
                return ControlResponse::CloudFenced {
                    control_session_id: session_id,
                    sequence,
                };
            }
            SequenceAdmission::Apply => {}
        }
        if !account_id.is_v4() || account_generation == 0 {
            return rejected(HostError::InvalidCloudRequest);
        }
        if account_generation
            < self
                .cloud
                .account_generations
                .get(&account_id)
                .copied()
                .unwrap_or(0)
        {
            return rejected(HostError::StaleCloudGeneration);
        }
        match permit_ids.as_deref() {
            // Null fences every permit for the account, without enumerating.
            None => self
                .cloud
                .live
                .retain(|_, held| held.permit.account_id != account_id),
            // An empty list is a no-op. It still consumes the sequence, so a
            // later message cannot reuse the number with a different body.
            Some([]) => {}
            Some(ids) => {
                if ids.len() > MAX_CLOUD_PERMIT_PAGE {
                    return rejected(HostError::CloudPermitPage);
                }
                // A live permit belonging to another account is foreign and
                // refused. An id that names nothing live was already fenced;
                // repeating a fence is idempotent, not an error.
                if ids.iter().any(|id| {
                    self.cloud
                        .live
                        .get(id)
                        .is_some_and(|held| held.permit.account_id != account_id)
                }) {
                    return rejected(HostError::ForeignCloudPermit);
                }
                for id in ids {
                    self.cloud.live.remove(id);
                }
            }
        }
        self.cloud.record(sequence, command);
        ControlResponse::CloudFenced {
            control_session_id: session_id,
            sequence,
        }
    }

    fn install_cloud_session(
        &mut self,
        session_id: ControlSessionId,
        principal: CloudPrincipalV1,
        connection_id: u64,
    ) -> ControlResponse {
        if principal.control_session_id != session_id.as_uuid() {
            return rejected(HostError::StaleSession);
        }
        if let Err(code) = principal.validate() {
            return rejected_code(code);
        }
        let Some(held) = self.cloud.live.get_mut(&principal.permit_id) else {
            return rejected(HostError::UnknownCloudPermit);
        };
        if held.permit.account_id != principal.account_id
            || held.permit.dataset() != Some(principal.dataset)
            || held.permit.account_generation != principal.account_generation
        {
            return rejected(HostError::CloudPermitMismatch);
        }
        // The generation counts within its own connection. The same number on
        // a new connection is the worker's next session, not a replay of the
        // old one; the same or a lower number on the connection that already
        // installed one is.
        if connection_id == held.installed_connection_id
            && principal.session_generation <= held.installed_session_generation
        {
            return rejected(HostError::CloudSessionReplay);
        }
        held.installed_session_generation = principal.session_generation;
        held.installed_connection_id = connection_id;
        held.stage = CloudStage::Connecting;
        held.error_code = None;
        self.cloud.revision = self.cloud.revision.saturating_add(1);
        ControlResponse::CloudSessionInstalled {
            permit_id: principal.permit_id,
            session_generation: principal.session_generation,
        }
    }

    pub fn cloud_status(&self, session_id: ControlSessionId) -> CloudStatus {
        CloudStatus {
            version: cloud::CLOUD_STATUS_VERSION,
            revision: self.cloud.revision,
            control_session_id: session_id,
            // `live` is a BTreeMap keyed by permit_id, so this is already the
            // sort order §3 requires; the bound is the map's own bound.
            records: self
                .cloud
                .live
                .values()
                .filter_map(|held| held.permit.dataset().map(|dataset| (held, dataset)))
                .map(|(held, dataset)| CloudStatusRecord {
                    permit_id: held.permit.permit_id,
                    account_id: held.permit.account_id,
                    account_generation: held.permit.account_generation,
                    dataset,
                    stage: held.stage,
                    error_code: held.error_code.clone(),
                    pending_consent_ids: Vec::new(),
                    bytes_held: 0,
                    queued_jobs: 0,
                })
                .collect(),
        }
    }

    /// Which Profiles and Workspaces this control session holds a **live Cloud
    /// authority** for.
    ///
    /// This is the Agent's already-verified hosted enablement with its account
    /// context, as B8's transport-mode initializer reads it: a permit exists
    /// only because the Agent checked the account, the subscription state and
    /// the hosted enablement before pushing it here, and re-deriving any of
    /// that in the Store would be a second authority for one question.
    ///
    /// **Identity only.** No permit id, no account id, no generation, no
    /// expiry and no credential leaves this call — the initializer needs to
    /// know *which subjects* the account covers and nothing else about them.
    /// A Workspace permit names its Workspace and, as a placement hint, the
    /// Profile it is currently placed under (none when unassigned); a
    /// Profile-units permit names only its Profile.
    pub fn cloud_authorized_subjects(&self) -> (BTreeSet<ProfileId>, BTreeSet<WorkspaceId>) {
        let mut profiles = BTreeSet::new();
        let mut workspaces = BTreeSet::new();
        for held in self.cloud.live.values() {
            if let Some(profile) = held.permit.placement_profile() {
                profiles.insert(ProfileId::from_uuid(profile.as_uuid()));
            }
            if let CloudPermitScopeV2::Workspace { descriptor } = &held.permit.scope {
                workspaces.insert(WorkspaceId::from_uuid(descriptor.workspace_id.as_uuid()));
            }
        }
        (profiles, workspaces)
    }

    /// What Cloud sync is doing for one Workspace, for a surface that has a
    /// Workspace in front of it and no permit id.
    ///
    /// **`None` is "nothing is installed for it"** — which from here is
    /// indistinguishable from signed out, from Cloud sync switched off, and
    /// from an Agent that has not finished selection yet, because all three
    /// look the same to a host whose permits are pushed to it. A caller that
    /// must tell those apart asks the Agent, which is where the choice lives.
    ///
    /// Only Workspace datasets answer: a Profile permit carries Identity,
    /// Private and Personal units, and none of them is the Workspace a
    /// document sits in.
    pub fn cloud_workspace_stage(&self, workspace: Uuid) -> Option<(CloudStage, Option<String>)> {
        self.cloud
            .live
            .values()
            .find(|held| match &held.permit.scope {
                CloudPermitScopeV2::Workspace { descriptor } => {
                    descriptor.workspace_id.as_uuid() == workspace
                }
                _ => false,
            })
            .map(|held| (held.stage, held.error_code.clone()))
    }

    /// The permit an arriving Cloud content stream may run under: live,
    /// naming this principal's account, dataset and account generation, and
    /// installed at exactly this session generation. The driver calls this
    /// at attachment, after the install acknowledgement it does not see
    /// (HOST-CONTROL §4 step 3): the two IPC channels are not ordered, so the
    /// stream proves itself against the installed context rather than the
    /// other way round.
    pub fn installed_cloud_permit(
        &self,
        principal: &CloudPrincipalV1,
    ) -> Result<CloudPermitV2, HostError> {
        if Some(principal.control_session_id) != self.session.map(ControlSessionId::as_uuid) {
            return Err(HostError::StaleSession);
        }
        let held = self
            .cloud
            .live
            .get(&principal.permit_id)
            .ok_or(HostError::UnknownCloudPermit)?;
        if held.permit.account_id != principal.account_id
            || held.permit.dataset() != Some(principal.dataset)
            || held.permit.account_generation != principal.account_generation
        {
            return Err(HostError::CloudPermitMismatch);
        }
        if held.installed_session_generation != principal.session_generation {
            return Err(HostError::CloudSessionReplay);
        }
        Ok(held.permit.clone())
    }

    /// Is this permit still live at this installed generation? Evaluated
    /// inside every Store job and at every unit start, so a fence or a
    /// replacement refuses work admitted after it (HOST-CONTROL §4).
    pub fn cloud_permit_live(&self, permit_id: Uuid, session_generation: u64) -> bool {
        self.cloud
            .live
            .get(&permit_id)
            .is_some_and(|held| held.installed_session_generation == session_generation)
    }

    /// The driver's in-memory progress for one permit, read back through
    /// `cloud_status`. It is status, never a receipt: `Settled` here is not
    /// coverage. Returns false when the permit is no longer live.
    pub fn set_cloud_stage(
        &mut self,
        permit_id: Uuid,
        stage: CloudStage,
        error_code: Option<&str>,
    ) -> bool {
        let Some(held) = self.cloud.live.get_mut(&permit_id) else {
            return false;
        };
        held.stage = stage;
        held.error_code = error_code.map(str::to_owned);
        self.cloud.revision = self.cloud.revision.saturating_add(1);
        true
    }

    fn decide_cloud_transfer(
        &mut self,
        session_id: ControlSessionId,
        control_session_id: ControlSessionId,
        consent_id: Uuid,
        decision: CloudTransferDecision,
        expected_network_generation: i64,
    ) -> ControlResponse {
        if control_session_id != session_id {
            return rejected(HostError::StaleSession);
        }
        if consent_id.get_version_num() != 4 || expected_network_generation <= 0 {
            return rejected(HostError::InvalidCloudRequest);
        }
        // A decision naming a generation this host never advertised is stale
        // by definition, and so is one naming a superseded generation. No
        // consent ticket survives a restart or a generation invalidation.
        match self.cloud.network {
            Some(policy) if policy.generation == expected_network_generation => {}
            _ => return rejected(HostError::StaleCloudDecision),
        }
        ControlResponse::CloudTransferDecided {
            consent_id,
            decision,
            network_generation: expected_network_generation,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_cloud_blob_access(
        &mut self,
        session_id: ControlSessionId,
        control_session_id: ControlSessionId,
        sequence: u64,
        account_id: CloudId,
        account_generation: u64,
        access: Box<CloudBlobAccessV1>,
    ) -> ControlResponse {
        if control_session_id != session_id {
            return rejected(HostError::StaleSession);
        }
        let expires_at_millis = access.expires_at_millis;
        let command = CloudSequenced::BlobAccess {
            account_id,
            account_generation,
            access: access.clone(),
        };
        match self.cloud.classify(sequence, &command) {
            SequenceAdmission::Refused(error) => return rejected(error),
            SequenceAdmission::Repeat => {
                return ControlResponse::CloudBlobAccessApplied {
                    control_session_id: session_id,
                    sequence,
                    expires_at_millis,
                };
            }
            SequenceAdmission::Apply => {}
        }
        // Shape only. Whether the token *works* is the control plane's
        // answer; what is refused here is a value that could never be one.
        if access.validate().is_err() {
            return rejected(HostError::InvalidCloudRequest);
        }
        if account_generation == 0 || !account_id.is_v4() {
            return rejected(HostError::InvalidCloudRequest);
        }
        // The same generation rule every other Cloud command follows: an
        // authority that has been replaced does not get to keep writing.
        if let Some(current) = self.cloud.account_generations.get(&account_id)
            && account_generation < *current
        {
            return rejected(HostError::StaleCloudGeneration);
        }
        self.cloud.blob_access = Some((account_id, account_generation, *access));
        self.cloud.record(sequence, command);
        ControlResponse::CloudBlobAccessApplied {
            control_session_id: session_id,
            sequence,
            expires_at_millis,
        }
    }

    /// What the gateway's blob engine reads: the account's reach into the
    /// control plane, or `None` when there is none or it has expired.
    ///
    /// **Expiry is checked here rather than discovered from a `401`.** A token
    /// this host knows is dead is a request it should not make: the refusal
    /// would look like a fenced account to everything downstream.
    pub fn cloud_blob_access(&self, now_millis: i64) -> Option<&CloudBlobAccessV1> {
        let (account, generation, access) = self.cloud.blob_access.as_ref()?;
        // An authority that has been replaced takes its token with it.
        if self
            .cloud
            .account_generations
            .get(account)
            .is_some_and(|current| *current > *generation)
        {
            return None;
        }
        access.live_at(now_millis).then_some(access)
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_cloud_network_policy(
        &mut self,
        session_id: ControlSessionId,
        control_session_id: ControlSessionId,
        sequence: u64,
        network_generation: i64,
        cost: CloudNetworkCost,
        wifi_only: bool,
    ) -> ControlResponse {
        if control_session_id != session_id {
            return rejected(HostError::StaleSession);
        }
        let command = CloudSequenced::NetworkPolicy {
            network_generation,
            cost,
            wifi_only,
        };
        match self.cloud.classify(sequence, &command) {
            SequenceAdmission::Refused(error) => return rejected(error),
            SequenceAdmission::Repeat => {
                return ControlResponse::CloudNetworkPolicyApplied {
                    control_session_id: session_id,
                    sequence,
                    network_generation,
                };
            }
            SequenceAdmission::Apply => {}
        }
        if network_generation <= 0 {
            return rejected(HostError::InvalidCloudRequest);
        }
        if self
            .cloud
            .network
            .is_some_and(|policy| network_generation <= policy.generation)
        {
            return rejected(HostError::StaleCloudNetworkGeneration);
        }
        self.cloud.network = Some(CloudNetworkPolicy {
            generation: network_generation,
            cost,
            wifi_only,
        });
        self.cloud.record(sequence, command);
        ControlResponse::CloudNetworkPolicyApplied {
            control_session_id: session_id,
            sequence,
            network_generation,
        }
    }

    pub fn status(&self) -> ControlStatus {
        let peers = self
            .authorization
            .as_ref()
            .map_or(&[][..], |value| value.peers.as_slice());
        ControlStatus {
            replication_ready: self.session.is_some()
                && self.endpoint_secret.is_some()
                && self.authorization.is_some(),
            endpoint_id: self.endpoint_id,
            authorization_sequence: self
                .authorization
                .as_ref()
                .map_or(0, |value| value.sequence),
            authorized_peers: peers.len(),
            authorized_workspaces: peers.iter().map(|peer| peer.workspaces.len()).sum(),
            desired_connections: self.desired_connections.len(),
            peers: Vec::new(),
            sync_event: None,
            root_sync: Vec::new(),
        }
    }

    pub fn authorize(
        &self,
        endpoint: ReplicationEndpointId,
        workspace: WorkspaceId,
    ) -> Option<(RelationshipId, DeviceId)> {
        self.authorization
            .as_ref()?
            .peers
            .iter()
            .find(|peer| {
                peer.endpoint_id == endpoint
                    && (peer.all_data || peer.workspaces.binary_search(&workspace).is_ok())
            })
            .map(|peer| (peer.relationship_id, peer.device_id))
    }

    pub fn snapshot(&self) -> Option<&AuthorizationSnapshot> {
        self.authorization.as_ref()
    }

    fn peer(&self, device: DeviceId) -> Option<&AuthorizedPeer> {
        self.authorization
            .as_ref()?
            .peers
            .iter()
            .find(|peer| peer.device_id == device)
    }

    fn clear(&mut self) {
        self.session = None;
        self.local_device = None;
        self.endpoint_secret = None;
        self.endpoint_id = None;
        self.authorization = None;
        self.desired_connections.clear();
        // A restart invalidates the Cloud sequence space and every permit.
        self.cloud.clear();
    }
}

impl AuthorizationSnapshot {
    /// True when the two snapshots grant the same thing, ignoring session
    /// bookkeeping (`session_id`, `sequence`) and cosmetics (`display_name`,
    /// `via`). `build` and `local_build` ARE significant: the build-drift
    /// sync-refusal gate is frozen into each peer session when it spawns and
    /// is only re-evaluated by a reconnect, so treating build as cosmetic
    /// would leave a stale gate on a live connection.
    ///
    /// Both sides are normalized internally — the Store's runtime receives
    /// snapshots *before* [`ControlHost::handle`] normalizes them.
    pub fn same_grant(&self, other: &Self) -> bool {
        fn grants(
            snapshot: &AuthorizationSnapshot,
        ) -> BTreeMap<
            DeviceId,
            (
                RelationshipId,
                ReplicationEndpointId,
                bool,
                Vec<ProfileId>,
                Vec<WorkspaceId>,
                Option<koru_device::BuildPayload>,
            ),
        > {
            let mut snapshot = snapshot.clone();
            normalize_snapshot(&mut snapshot);
            snapshot
                .peers
                .into_iter()
                .map(|peer| {
                    (
                        peer.device_id,
                        (
                            peer.relationship_id,
                            peer.endpoint_id,
                            peer.all_data,
                            peer.authorized_profiles,
                            peer.workspaces,
                            peer.build,
                        ),
                    )
                })
                .collect()
        }
        self.local_device == other.local_device
            && self.local_build == other.local_build
            && grants(self) == grants(other)
    }
}

fn normalize_snapshot(snapshot: &mut AuthorizationSnapshot) {
    for peer in &mut snapshot.peers {
        peer.authorized_profiles.sort();
        peer.authorized_profiles.dedup();
        peer.workspaces.sort();
        peer.workspaces.dedup();
    }
    let mut by_device = BTreeMap::new();
    for peer in snapshot.peers.drain(..) {
        by_device.insert(peer.device_id, peer);
    }
    snapshot.peers = by_device.into_values().collect();
}

fn rejected(error: HostError) -> ControlResponse {
    ControlResponse::Rejected {
        code: error.code().to_owned(),
    }
}

/// Some refusals come back from a DTO's own `validate`, which already names a
/// stable lowercase code. Reusing it keeps one vocabulary at the boundary.
fn rejected_code(code: &str) -> ControlResponse {
    ControlResponse::Rejected {
        code: code.to_owned(),
    }
}

/// Every refusal code is a stable lowercase string. They are read by the
/// Agent, logged, and frozen in the host-control vectors, so a rename is a
/// wire change.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    Protocol,
    StaleSession,
    OutOfOrder,
    UnauthorizedPeer,
    WrongPairingDevice,
    MalformedTrustGossip,
    /// The same sequence number with a different body.
    CloudSequenceConflict,
    /// A sequence number below the latest one this session acknowledged.
    StaleCloudSequence,
    /// An account generation below the one currently held for that account,
    /// or a permit whose generation does not match its page.
    StaleCloudGeneration,
    /// A permit naming another account, in a page or in an explicit fence.
    ForeignCloudPermit,
    /// A page or fence list over [`cloud::MAX_CLOUD_PERMIT_PAGE`], or one that
    /// names the same permit twice.
    CloudPermitPage,
    /// The page would take the host over [`cloud::MAX_CLOUD_PERMITS`] live
    /// permits. Nothing is applied: schedule the catalog in cohorts instead.
    CloudPermitCapacity,
    /// A session install naming a permit that is not acknowledged and current.
    UnknownCloudPermit,
    /// The principal disagrees with its permit on account, dataset or
    /// generation.
    CloudPermitMismatch,
    /// A session generation at or below the one already installed.
    CloudSessionReplay,
    /// A decision naming a network generation this host never advertised or
    /// has already superseded.
    StaleCloudDecision,
    StaleCloudNetworkGeneration,
    /// A shape this operation never accepts — a Profile dataset in
    /// `read_cloud_bootstrap`, a noncanonical id, a nonpositive generation.
    InvalidCloudRequest,
}

impl HostError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Protocol => "protocol",
            Self::StaleSession => "stale_session",
            Self::OutOfOrder => "out_of_order",
            Self::UnauthorizedPeer => "unauthorized_peer",
            Self::WrongPairingDevice => "wrong_pairing_device",
            Self::MalformedTrustGossip => "malformed_trust_gossip",
            Self::CloudSequenceConflict => "cloud_sequence_conflict",
            Self::StaleCloudSequence => "stale_cloud_sequence",
            Self::StaleCloudGeneration => "stale_cloud_generation",
            Self::ForeignCloudPermit => "foreign_cloud_permit",
            Self::CloudPermitPage => "cloud_permit_page",
            Self::CloudPermitCapacity => "cloud_permit_capacity",
            Self::UnknownCloudPermit => "unknown_cloud_permit",
            Self::CloudPermitMismatch => "cloud_permit_mismatch",
            Self::CloudSessionReplay => "cloud_session_replay",
            Self::StaleCloudDecision => "stale_cloud_decision",
            Self::StaleCloudNetworkGeneration => "stale_cloud_network_generation",
            Self::InvalidCloudRequest => "invalid_cloud_request",
        }
    }
}

/// True for the control-7 Cloud variants, which carry an extra 64 KiB
/// frame cap on top of the existing one ([`cloud::MAX_CLOUD_CONTROL_FRAME_BYTES`]).
/// A reader checks the encoded body it already has rather than re-encoding the
/// decoded request. The root-unit join (`JoinCloudRootUnit`) is deliberately
/// not under it: its body is the frames the account API answered, bounded
/// by that API's response cap and by the socket's own frame cap, and a
/// personal unit's history is not a permit page.
pub fn is_cloud_request(request: &ControlRequest) -> bool {
    matches!(
        request,
        ControlRequest::ReplaceCloudContext { .. }
            | ControlRequest::InstallCloudSession { .. }
            | ControlRequest::FenceCloud { .. }
            | ControlRequest::CloudStatus { .. }
            | ControlRequest::DecideCloudTransfer { .. }
            | ControlRequest::ReplaceCloudNetworkPolicy { .. }
            | ControlRequest::ReplaceCloudBlobAccess { .. }
            | ControlRequest::ReadCloudBootstrap { .. }
            | ControlRequest::ReadCloudBootstrapV2 { .. }
            | ControlRequest::BindCloudAccountScope { .. }
            | ControlRequest::ReadCloudAccountBinding { .. }
            | ControlRequest::ReadCloudRootInventory { .. }
            | ControlRequest::ReadCloudRootFrontier { .. }
            | ControlRequest::ExportCloudRootUnit { .. }
    )
}

#[cfg(test)]
mod tests;
