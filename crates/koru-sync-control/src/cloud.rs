// SPDX-License-Identifier: MPL-2.0

//! Process-memory replacement policy; never a credential or durable receipt.
//! One owner per connection/dataset, shared by desktop and mobile composition.
//!
//! This module also holds the strict DTOs of control protocol 7's seven Cloud
//! operations (HOST-CONTROL §3). Every one of them is in-memory state: a
//! permit, a status snapshot, a network policy. None is a durable receipt, a
//! coverage claim or a credential, and none survives a control session.

use koru_device::ReplicationEndpointId;
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_store_core::profile_units::ProfileUnitRef;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ControlSessionId;

/// Lightweight standing subscriptions, independent of active work and wire
/// page size. Capacity exhaustion must not rotate healthy subscriptions.
pub const MAX_CLOUD_PERMITS: usize = 4096;

/// One `permits` page, and one explicit `fence_cloud` list, are bounded by the
/// same number: a page is an upsert, never a replacement of an arbitrarily
/// large account set by one unbounded message.
pub const MAX_CLOUD_PERMIT_PAGE: usize = 32;

/// A status snapshot names at most the live permit set, and each record at
/// most two pending consent tickets.
pub const MAX_CLOUD_PENDING_CONSENTS: usize = 2;

/// At most Identity, Private and Personal for the exact authorizing Workspace.
pub const MAX_ALLOWED_PROFILE_UNITS: usize = 3;

/// The seven Cloud control variants are additionally capped at 64 KiB on top
/// of the existing control frame cap (HOST-CONTROL §3). The structural bounds
/// above are what actually keep a body under it; this is the backstop a reader
/// applies to the encoded frame before the state machine sees it.
pub const MAX_CLOUD_CONTROL_FRAME_BYTES: usize = 64 * 1024;

pub const CLOUD_PERMIT_VERSION: u32 = 1;
pub const CLOUD_WORKSPACE_DESCRIPTOR_VERSION: u32 = 1;
pub const CLOUD_STATUS_VERSION: u32 = 1;
/// The v2 permit and descriptor of root-catalog Plan 3.1 (TECHNICAL.final
/// §7.3): separate admissions per scope, placement beside — not inside —
/// content identity. Beside v1, which old clients keep issuing.
pub const CLOUD_PERMIT_VERSION_V2: u32 = 2;
pub const CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2: u32 = 2;

/// Identity only, and deliberately **not** [`ReplicaDescriptor`], which also
/// carries names and appearance. It comes from the hosted descriptor read
/// (§3.1) or, for the originating device, from `read_cloud_bootstrap` — never
/// from a fresh device's invented manifest or a free-form UI input.
///
/// [`ReplicaDescriptor`]: koru_store_core
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceDescriptorV1 {
    pub version: u32,
    pub profile_id: CloudId,
    pub workspace_id: CloudId,
    pub workspace_incarnation_id: CloudId,
    pub logical_replica_id: CloudId,
    pub lineage_id: CloudId,
    pub manifest_document_id: CloudId,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
}

impl CloudWorkspaceDescriptorV1 {
    /// Canonical UUIDv4 for every id; positive signed epochs, matching the
    /// existing API domain. A partial or fabricated descriptor is refused
    /// here rather than turned into a manifest downstream.
    pub fn validate(&self) -> Result<(), HostCloudError> {
        if self.version != CLOUD_WORKSPACE_DESCRIPTOR_VERSION {
            return Err(HostCloudError::DescriptorVersion);
        }
        if !self.profile_id.is_v4()
            || !self.workspace_id.is_v4()
            || !self.workspace_incarnation_id.is_v4()
            || !self.logical_replica_id.is_v4()
            || !self.lineage_id.is_v4()
            || !self.manifest_document_id.is_v4()
        {
            return Err(HostCloudError::DescriptorIdentity);
        }
        // §3.1: lineage equals the Workspace incarnation.
        if self.lineage_id != self.workspace_incarnation_id {
            return Err(HostCloudError::DescriptorLineage);
        }
        if self.authorization_version <= 0
            || self.membership_epoch <= 0
            || self.cloud_access_epoch <= 0
        {
            return Err(HostCloudError::DescriptorEpoch);
        }
        Ok(())
    }
}

/// One bounded authority context the Agent asks the gateway to hold. It is an
/// admission limit, never proof that local content exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPermitV1 {
    pub version: u32,
    pub permit_id: Uuid,
    pub dataset: CloudReplicaPrincipalV1,
    pub account_id: CloudId,
    pub profile_id: CloudId,
    pub authority_workspace_id: CloudId,
    pub authority_workspace_incarnation_id: CloudId,
    pub account_generation: u64,
    pub expires_at_seconds: i64,
    /// Empty for Workspace datasets; at most Identity, Private and Personal
    /// for the exact authorizing Workspace on a Profile dataset, limited
    /// further by the actual grant. Never a union of Personal scopes from
    /// unrelated Workspaces.
    pub allowed_profile_units: Vec<ProfileUnitRef>,
    /// Required for Workspace datasets, null for Profile datasets.
    pub workspace_descriptor: Option<CloudWorkspaceDescriptorV1>,
}

impl CloudPermitV1 {
    pub fn validate(&self) -> Result<(), HostCloudError> {
        if self.version != CLOUD_PERMIT_VERSION {
            return Err(HostCloudError::PermitVersion);
        }
        if self.permit_id.get_version_num() != 4 {
            return Err(HostCloudError::PermitIdentity);
        }
        if !self.account_id.is_v4()
            || !self.profile_id.is_v4()
            || !self.authority_workspace_id.is_v4()
            || !self.authority_workspace_incarnation_id.is_v4()
        {
            return Err(HostCloudError::PermitIdentity);
        }
        if self.account_generation == 0 {
            return Err(HostCloudError::PermitGeneration);
        }
        if self.expires_at_seconds <= 0 {
            return Err(HostCloudError::PermitExpiry);
        }
        match (self.dataset.is_workspace(), &self.workspace_descriptor) {
            (true, Some(descriptor)) => {
                if !self.allowed_profile_units.is_empty() {
                    return Err(HostCloudError::PermitProfileUnits);
                }
                descriptor.validate()?;
            }
            (true, None) => return Err(HostCloudError::PermitDescriptorRequired),
            (false, Some(_)) => return Err(HostCloudError::PermitDescriptorForbidden),
            (false, None) => {
                if self.allowed_profile_units.len() > MAX_ALLOWED_PROFILE_UNITS {
                    return Err(HostCloudError::PermitProfileUnits);
                }
                let mut seen = self.allowed_profile_units.clone();
                seen.sort();
                seen.dedup();
                if seen.len() != self.allowed_profile_units.len() {
                    return Err(HostCloudError::PermitProfileUnits);
                }
                if self
                    .allowed_profile_units
                    .iter()
                    .any(|unit| matches!(unit, ProfileUnitRef::LegacyPrivate { .. }))
                {
                    return Err(HostCloudError::PermitProfileUnits);
                }
            }
        }
        Ok(())
    }
}

/// The v2 bootstrap descriptor: the same content identity as
/// [`CloudWorkspaceDescriptorV1`] with the membership Profile gone and the
/// Workspace's **placement as a projection of the committed account
/// catalog** beside it (root-catalog TECHNICAL.final §7.3 "Hosted storage
/// must separate owner account/workspace membership from placement").
/// `placement` null is explicitly unassigned — a Workspace the v1 shape
/// could not describe at all. It still comes from the hosted descriptor read
/// or `read_cloud_bootstrap`, never from an invented manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceDescriptorV2 {
    pub version: u32,
    pub workspace_id: CloudId,
    pub workspace_incarnation_id: CloudId,
    pub logical_replica_id: CloudId,
    pub lineage_id: CloudId,
    pub manifest_document_id: CloudId,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    pub placement: Option<CloudId>,
    /// The catalog unit revision at which the placement was committed to the
    /// hosted projection; a Profile-grant cache invalidates on it.
    pub placement_revision: i64,
}

impl CloudWorkspaceDescriptorV2 {
    /// Canonical UUIDv4 for every id, lineage equal to the incarnation,
    /// positive epochs and a nonnegative placement revision. A placement
    /// that is present must be a canonical v4 too; null is a value here,
    /// not an omission.
    pub fn validate(&self) -> Result<(), HostCloudError> {
        if self.version != CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2 {
            return Err(HostCloudError::DescriptorVersion);
        }
        if !self.workspace_id.is_v4()
            || !self.workspace_incarnation_id.is_v4()
            || !self.logical_replica_id.is_v4()
            || !self.lineage_id.is_v4()
            || !self.manifest_document_id.is_v4()
            || self.placement.is_some_and(|profile| !profile.is_v4())
        {
            return Err(HostCloudError::DescriptorIdentity);
        }
        if self.lineage_id != self.workspace_incarnation_id {
            return Err(HostCloudError::DescriptorLineage);
        }
        if self.authorization_version <= 0
            || self.membership_epoch <= 0
            || self.cloud_access_epoch <= 0
            || self.placement_revision < 0
        {
            return Err(HostCloudError::DescriptorEpoch);
        }
        Ok(())
    }

    /// The v1 dataset principal of this Workspace, for the existing
    /// handshake.
    pub fn dataset(&self) -> CloudReplicaPrincipalV1 {
        CloudReplicaPrincipalV1::Workspace {
            workspace_id: self.workspace_id,
            incarnation_id: self.workspace_incarnation_id,
            logical_replica_id: self.logical_replica_id,
        }
    }
}

/// What one v2 permit admits (TECHNICAL.final §7.3 "separately versioned
/// user-catalog, workspace, user-personal and optional Profile
/// Identity/Private admissions, each with its own eligibility and authority
/// generation"). A permit is for one scope; the `(workspace_permit,
/// profile_permit)` pairing of v1 is gone — a Workspace is admitted on its
/// own descriptor, membership and incarnation, and no Profile permit is
/// required beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudPermitScopeV2 {
    /// The account's root units: Profile and Workspace catalog units, over
    /// the account catalog session. No v1 dataset: the session is the
    /// scope's, not a replica's.
    UserCatalog,
    /// One Workspace's content. The descriptor's placement is a projection
    /// for the UI and the grant cache, never an admission input.
    Workspace {
        descriptor: CloudWorkspaceDescriptorV2,
    },
    /// The account's personal v2 unit for one Workspace, over the account
    /// catalog session. Follows the Workspace's policy; names no Profile.
    UserPersonal { workspace_id: CloudId },
    /// One Profile's Identity and Private units over its v1 Profile
    /// dataset, eligible only while a Cloud-enabled Workspace is currently
    /// placed under it (§4.4); never Personal, which is `UserPersonal`.
    ProfileUnits {
        dataset: CloudReplicaPrincipalV1,
        /// Identity and/or Private of exactly this Profile.
        allowed_profile_units: Vec<ProfileUnitRef>,
    },
}

/// One bounded authority context for one scope. Like [`CloudPermitV1`], an
/// admission limit, never proof that local content exists; unlike it,
/// fenced by the scope's own `authority_generation` as well as the account
/// generation, so a Profile-units permit issued before the last Cloud
/// Workspace moved out is stale on its own, without re-issuing the
/// Workspace's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPermitV2 {
    pub version: u32,
    pub permit_id: Uuid,
    pub account_id: CloudId,
    /// The account's catalog scope as the service minted it. `None` only
    /// in the v1 compatibility window (root-catalog Plan 3.2): a service
    /// that still answers v1 has minted no scope, and the permits a device
    /// builds from a v1 token pair say so rather than invent one.
    pub catalog_scope_id: Option<CloudId>,
    pub scope: CloudPermitScopeV2,
    pub account_generation: u64,
    pub authority_generation: u64,
    pub expires_at_seconds: i64,
}

impl CloudPermitV2 {
    pub fn validate(&self) -> Result<(), HostCloudError> {
        if self.version != CLOUD_PERMIT_VERSION_V2 {
            return Err(HostCloudError::PermitVersion);
        }
        if self.permit_id.get_version_num() != 4
            || !self.account_id.is_v4()
            || self.catalog_scope_id.is_some_and(|scope| !scope.is_v4())
        {
            return Err(HostCloudError::PermitIdentity);
        }
        if self.account_generation == 0 || self.authority_generation == 0 {
            return Err(HostCloudError::PermitGeneration);
        }
        if self.expires_at_seconds <= 0 {
            return Err(HostCloudError::PermitExpiry);
        }
        match &self.scope {
            CloudPermitScopeV2::UserCatalog => Ok(()),
            CloudPermitScopeV2::Workspace { descriptor } => descriptor.validate(),
            CloudPermitScopeV2::UserPersonal { workspace_id } => {
                if !workspace_id.is_v4() {
                    return Err(HostCloudError::PermitIdentity);
                }
                Ok(())
            }
            CloudPermitScopeV2::ProfileUnits {
                dataset,
                allowed_profile_units,
            } => {
                let CloudReplicaPrincipalV1::Profile { profile_id, .. } = dataset else {
                    return Err(HostCloudError::PermitScope);
                };
                if !profile_id.is_v4() {
                    return Err(HostCloudError::PermitIdentity);
                }
                if allowed_profile_units.is_empty() || allowed_profile_units.len() > 2 {
                    return Err(HostCloudError::PermitProfileUnits);
                }
                let mut seen = allowed_profile_units.clone();
                seen.sort();
                seen.dedup();
                if seen.len() != allowed_profile_units.len() {
                    return Err(HostCloudError::PermitProfileUnits);
                }
                for unit in allowed_profile_units {
                    let profile = match unit {
                        ProfileUnitRef::Identity { profile }
                        | ProfileUnitRef::Private { profile } => profile,
                        // Personal is `UserPersonal`, keyed by the Workspace;
                        // the legacy private unit is never admitted to Cloud.
                        ProfileUnitRef::Personal { .. } | ProfileUnitRef::LegacyPrivate { .. } => {
                            return Err(HostCloudError::PermitProfileUnits);
                        }
                    };
                    if profile.as_uuid().as_bytes() != profile_id.as_bytes() {
                        return Err(HostCloudError::PermitProfileUnits);
                    }
                }
                Ok(())
            }
        }
    }

    /// The v1 dataset principal this permit's session is admitted under:
    /// the Workspace's or the Profile's replica for the content scopes,
    /// `None` for the two account-catalog scopes, whose session is the
    /// catalog's own.
    pub fn dataset(&self) -> Option<CloudReplicaPrincipalV1> {
        match &self.scope {
            CloudPermitScopeV2::Workspace { descriptor } => Some(descriptor.dataset()),
            CloudPermitScopeV2::ProfileUnits { dataset, .. } => Some(*dataset),
            CloudPermitScopeV2::UserCatalog | CloudPermitScopeV2::UserPersonal { .. } => None,
        }
    }

    /// The Workspace a content permit is about: the Workspace scope's own
    /// id, or the personal scope's Workspace. `None` for the catalog and the
    /// Profile-units scopes.
    pub fn workspace_id(&self) -> Option<CloudId> {
        match &self.scope {
            CloudPermitScopeV2::Workspace { descriptor } => Some(descriptor.workspace_id),
            CloudPermitScopeV2::UserPersonal { workspace_id } => Some(*workspace_id),
            CloudPermitScopeV2::UserCatalog | CloudPermitScopeV2::ProfileUnits { .. } => None,
        }
    }

    /// The Profile a permit names **as a placement hint**: the Workspace
    /// scope's projected placement, or the Profile whose units a
    /// Profile-units permit admits. Never an admission input (root-catalog
    /// TECHNICAL.final §7.3): the mode initializer reads it to know which
    /// subjects the account covers, nothing else.
    pub fn placement_profile(&self) -> Option<CloudId> {
        match &self.scope {
            CloudPermitScopeV2::Workspace { descriptor } => descriptor.placement,
            CloudPermitScopeV2::ProfileUnits { dataset, .. } => match dataset {
                CloudReplicaPrincipalV1::Profile { profile_id, .. } => Some(*profile_id),
                CloudReplicaPrincipalV1::Workspace { .. } => None,
            },
            CloudPermitScopeV2::UserCatalog | CloudPermitScopeV2::UserPersonal { .. } => None,
        }
    }

    /// The v2 reading of a v1 permit, for the compatibility window of
    /// root-catalog Plan 3.2 (the contract's mapping: a v1 Workspace permit
    /// is a v2 `workspace` permit whose placement is the v1 membership
    /// Profile; a v1 Profile permit is a v2 `profile_units` permit over
    /// Identity and Private only — its Personal unit is the v2
    /// `user_personal` scope, which no v1 token carries, and the Store has
    /// answered `None` for the v1 Personal address since catalog 13). The
    /// authority generation is 1: a v1 service has none, and the account
    /// generation alone fences it.
    pub fn from_v1(permit: &CloudPermitV1) -> Result<Self, HostCloudError> {
        permit.validate()?;
        let scope = match (permit.dataset, &permit.workspace_descriptor) {
            (CloudReplicaPrincipalV1::Workspace { .. }, Some(descriptor)) => {
                CloudPermitScopeV2::Workspace {
                    descriptor: CloudWorkspaceDescriptorV2::from_v1(descriptor),
                }
            }
            (CloudReplicaPrincipalV1::Workspace { .. }, None) => {
                return Err(HostCloudError::PermitDescriptorRequired);
            }
            (dataset @ CloudReplicaPrincipalV1::Profile { profile_id, .. }, _) => {
                if profile_id != permit.profile_id {
                    return Err(HostCloudError::PermitScope);
                }
                let allowed_profile_units: Vec<ProfileUnitRef> = permit
                    .allowed_profile_units
                    .iter()
                    .filter(|unit| {
                        matches!(
                            unit,
                            ProfileUnitRef::Identity { .. } | ProfileUnitRef::Private { .. }
                        )
                    })
                    .cloned()
                    .collect();
                if allowed_profile_units.is_empty() {
                    return Err(HostCloudError::PermitProfileUnits);
                }
                CloudPermitScopeV2::ProfileUnits {
                    dataset,
                    allowed_profile_units,
                }
            }
        };
        let converted = Self {
            version: CLOUD_PERMIT_VERSION_V2,
            permit_id: permit.permit_id,
            account_id: permit.account_id,
            catalog_scope_id: None,
            scope,
            account_generation: permit.account_generation,
            authority_generation: 1,
            expires_at_seconds: permit.expires_at_seconds,
        };
        converted.validate()?;
        Ok(converted)
    }
}

impl CloudWorkspaceDescriptorV2 {
    /// The v1 descriptor read as v2: the membership Profile becomes the
    /// placement projection at revision 0 — the v1 service answers it from
    /// the same projection during the window (the contract's §4).
    pub fn from_v1(descriptor: &CloudWorkspaceDescriptorV1) -> Self {
        Self {
            version: CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2,
            workspace_id: descriptor.workspace_id,
            workspace_incarnation_id: descriptor.workspace_incarnation_id,
            logical_replica_id: descriptor.logical_replica_id,
            lineage_id: descriptor.lineage_id,
            manifest_document_id: descriptor.manifest_document_id,
            authorization_version: descriptor.authorization_version,
            membership_epoch: descriptor.membership_epoch,
            cloud_access_epoch: descriptor.cloud_access_epoch,
            placement: Some(descriptor.profile_id),
            placement_revision: 0,
        }
    }
}

// ----------------------------------------------------- the root-unit join

/// At most this many update frames in one root-unit control message, the
/// bound `koru-account-wire::root_catalog::MAX_ROOT_UNIT_FRAMES` puts on the
/// HTTP body the frames came from.
pub const MAX_CLOUD_ROOT_FRAMES: usize = 64;

/// At most this many units in one root inventory answer: Profiles plus
/// Workspaces plus personal units of one account on one device.
pub const MAX_CLOUD_ROOT_INVENTORY: usize = 4096;

/// What the device holds as its account-scope binding, as
/// `Store::account_binding` answers it, for the Agent to declare to the
/// service (root-catalog Plan 3.2; TECHNICAL.final §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudAccountBindingRef {
    pub account_id: CloudId,
    pub catalog_scope_id: CloudId,
}

/// One unit of the account path's root inventory: the unit and the frontier
/// this device holds for it (an encoded Loro version vector).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudRootUnitState {
    pub unit: koru_store_core::service::JoinUnit,
    pub frontier: Vec<u8>,
}

/// What the Store's export boundary answered for one unit over the Cloud
/// path: derived bytes, a hold (logged by id, never raw bytes), or no such
/// unit here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudRootExport {
    Published {
        frames: Vec<Vec<u8>>,
        redacted_ops: u32,
    },
    Held,
    NotFound,
}

/// What the Store's staged join did with the service's frames for one unit
/// (the peer protocol 6 receipt phases on the account relationship).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudRootJoin {
    Applied { frontier: Vec<u8> },
    Pending { frontier: Vec<u8> },
}

/// Where one live permit's work currently stands. `Settled` is **not** durable
/// Cloud coverage, and no stage on this list may be used as input to the
/// coverage projection: only the existing durable receipt and version evidence
/// can establish that.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudStage {
    AwaitingAuthority,
    Connecting,
    Reconciling,
    WaitingForConsent,
    Offline,
    Unavailable,
    CheckingCoverage,
    Settled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusRecord {
    pub permit_id: Uuid,
    pub account_id: CloudId,
    pub account_generation: u64,
    pub dataset: CloudReplicaPrincipalV1,
    pub stage: CloudStage,
    /// A stable lowercase code, never a message, a title or a path.
    pub error_code: Option<String>,
    pub pending_consent_ids: Vec<Uuid>,
    pub bytes_held: u64,
    pub queued_jobs: u32,
}

/// The current in-memory state of the bounded live permit set, complete and
/// sorted by `permit_id`. It is not a listing of every document waiting in a
/// catalog, not a durable work queue, and it holds no credential string, name,
/// title, content or permanent synced flag. A different control session
/// invalidates the whole snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatus {
    pub version: u32,
    pub revision: u64,
    pub control_session_id: ControlSessionId,
    pub records: Vec<CloudStatusRecord>,
}

/// Exactly TECHNICAL §4.4's `CloudTransferDecisionV1` values. New labels were
/// deliberately not invented for the control protocol: a decision crossing
/// this channel names the same two outcomes the wire record does.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudTransferDecision {
    Accept,
    Defer,
}

/// TECHNICAL §4.5's three network-cost classifications. Unknown is restricted:
/// an observation that cannot prove unmetered must not exempt a download.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudNetworkCost {
    Unmetered,
    Metered,
    Unknown,
}

/// What the Agent knows and the gateway needs in order to reach the blob
/// **control** plane: where it is, which region it is, and the account bearer
/// token that opens it.
///
/// **Why this crosses the channel at all.** The account lives in the Agent and
/// the Store lives in the gateway, and a blob transfer needs both: the five
/// control routes take an account bearer token, and every other step — the
/// referenced-blob walk, the staging file, the object install, the
/// announcement — is the Store's. Splitting the engine across the two
/// processes would put half a state machine on each side of an IPC boundary;
/// carrying the fact the Agent holds is the smaller seam. Both processes are
/// the same person's, on the same machine, over the per-user named pipe with
/// its security descriptor, and the gateway already holds every document's
/// plaintext. Recorded as an open question for the maintainers.
///
/// **The byte-URL allowlist is deliberately not here.** Where a person's file
/// may be sent is a fact the *device* holds, compiled in per region; a
/// configurable allowlist is an allowlist an attacker configures, and one that
/// arrived over a channel is one that arrived.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudBlobAccessV1 {
    pub version: u32,
    /// The control plane's origin, already validated by the account owner.
    pub api_origin: String,
    /// `us` or `eu`, as the regional credential policy names it. The gateway
    /// turns this into the compiled-in byte-origin set and nothing else.
    pub region: String,
    /// The account bearer token. Short-lived; the Agent replaces it.
    pub access_token: String,
    /// When the token stops being one, in epoch milliseconds UTC. The gateway
    /// stops using it then rather than discovering it from a `401`.
    pub expires_at_millis: i64,
}

pub const CLOUD_BLOB_ACCESS_VERSION: u32 = 1;

/// Redacted, deliberately and by construction. A bearer token that reached a
/// log would open every Workspace of an account, and the one place a value
/// like this escapes is a `{:?}` somebody added while debugging something
/// else.
impl std::fmt::Debug for CloudBlobAccessV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudBlobAccessV1")
            .field("version", &self.version)
            .field("api_origin", &self.api_origin)
            .field("region", &self.region)
            .field("access_token", &"<redacted>")
            .field("expires_at_millis", &self.expires_at_millis)
            .finish()
    }
}

impl CloudBlobAccessV1 {
    /// Shape only. Whether the token *works* is the control plane's answer and
    /// is never guessed here.
    pub fn validate(&self) -> Result<(), HostCloudError> {
        if self.version != CLOUD_BLOB_ACCESS_VERSION {
            return Err(HostCloudError::BlobAccessVersion);
        }
        if !matches!(self.region.as_str(), "us" | "eu") {
            return Err(HostCloudError::BlobAccessShape);
        }
        // Bounded, and an absolute `https` (or loopback `http`) origin with no
        // path, query or fragment — the same policy the account client's own
        // origin validation applies, because it is the same origin.
        if self.api_origin.len() > 512 || self.access_token.is_empty() {
            return Err(HostCloudError::BlobAccessShape);
        }
        if self.access_token.len() > 4096
            || self
                .access_token
                .bytes()
                .any(|byte| !(0x21..=0x7e).contains(&byte))
        {
            return Err(HostCloudError::BlobAccessShape);
        }
        if self.expires_at_millis <= 0 {
            return Err(HostCloudError::BlobAccessExpiry);
        }
        let origin =
            url::Url::parse(&self.api_origin).map_err(|_| HostCloudError::BlobAccessShape)?;
        let loopback = matches!(
            origin.host(),
            Some(url::Host::Domain("localhost"))
                | Some(url::Host::Ipv4(_))
                | Some(url::Host::Ipv6(_))
        );
        let scheme_ok = origin.scheme() == "https" || (origin.scheme() == "http" && loopback);
        if !scheme_ok
            || origin.host_str().is_none_or(str::is_empty)
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
        {
            return Err(HostCloudError::BlobAccessShape);
        }
        Ok(())
    }

    pub fn live_at(&self, now_millis: i64) -> bool {
        now_millis < self.expires_at_millis
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostCloudError {
    BlobAccessVersion,
    BlobAccessShape,
    BlobAccessExpiry,
    DescriptorVersion,
    DescriptorIdentity,
    DescriptorLineage,
    DescriptorEpoch,
    PermitVersion,
    PermitIdentity,
    PermitGeneration,
    PermitExpiry,
    PermitProfileUnits,
    PermitDescriptorRequired,
    PermitDescriptorForbidden,
    /// A v2 permit whose scope and dataset disagree: a Profile-units scope
    /// over a Workspace dataset, or the reverse.
    PermitScope,
}

impl HostCloudError {
    pub fn code(self) -> &'static str {
        match self {
            Self::BlobAccessVersion => "cloud_blob_access_version",
            Self::BlobAccessShape => "cloud_blob_access_shape",
            Self::BlobAccessExpiry => "cloud_blob_access_expiry",
            Self::DescriptorVersion => "cloud_descriptor_version",
            Self::DescriptorIdentity => "cloud_descriptor_identity",
            Self::DescriptorLineage => "cloud_descriptor_lineage",
            Self::DescriptorEpoch => "cloud_descriptor_epoch",
            Self::PermitVersion => "cloud_permit_version",
            Self::PermitIdentity => "cloud_permit_identity",
            Self::PermitGeneration => "cloud_permit_generation",
            Self::PermitExpiry => "cloud_permit_expiry",
            Self::PermitProfileUnits => "cloud_permit_profile_units",
            Self::PermitDescriptorRequired => "cloud_permit_descriptor_required",
            Self::PermitDescriptorForbidden => "cloud_permit_descriptor_forbidden",
            Self::PermitScope => "cloud_permit_scope",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionGeneration {
    pub generation: u64,
    pub expires_at_seconds: i64,
}

pub struct CloudSessionHandover {
    endpoint: ReplicationEndpointId,
    principal: CloudReplicaPrincipalV1,
    active: Option<SessionGeneration>,
    draining: Option<SessionGeneration>,
    highest_generation: u64,
}

impl CloudSessionHandover {
    pub fn new(endpoint: ReplicationEndpointId, principal: CloudReplicaPrincipalV1) -> Self {
        Self {
            endpoint,
            principal,
            active: None,
            draining: None,
            highest_generation: 0,
        }
    }
    pub fn refresh_due(&self, now_seconds: i64) -> bool {
        self.active
            .is_none_or(|current| current.expires_at_seconds.saturating_sub(now_seconds) <= 120)
    }
    pub fn active(&self, now_seconds: i64) -> Result<SessionGeneration, &'static str> {
        self.active
            .filter(|s| s.expires_at_seconds > now_seconds)
            .ok_or("credential_expired")
    }
    /// Call only after SessionAccepted is received, never when a replacement
    /// request is sent. Failure leaves the old generation's exact expiry intact.
    pub fn acknowledge(
        &mut self,
        endpoint: ReplicationEndpointId,
        principal: CloudReplicaPrincipalV1,
        replacement: SessionGeneration,
        now_seconds: i64,
    ) -> Result<(), &'static str> {
        if endpoint != self.endpoint {
            return Err("endpoint_mismatch");
        }
        if principal != self.principal {
            return Err("replica_mismatch");
        }
        if replacement.generation <= self.highest_generation {
            return Err("handshake_replay");
        }
        if replacement.expires_at_seconds <= now_seconds {
            return Err("credential_expired");
        }
        if self.draining(now_seconds).is_some() {
            return Err("sync_unavailable");
        }
        self.draining = self.active.replace(replacement);
        self.highest_generation = replacement.generation;
        Ok(())
    }
    pub fn draining(&self, now_seconds: i64) -> Option<SessionGeneration> {
        self.draining
            .filter(|old| old.expires_at_seconds > now_seconds)
    }
    pub fn drained(&mut self, generation: u64) {
        if self
            .draining
            .is_some_and(|old| old.generation == generation)
        {
            self.draining = None
        }
    }
    pub fn fence(&mut self) {
        // Keep the high-water mark for this connection even after revocation.
        self.active = None;
        self.draining = None;
    }
}

#[cfg(test)]
mod tests;
