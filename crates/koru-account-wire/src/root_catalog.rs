// SPDX-License-Identifier: MPL-2.0

//! **The v2 Cloud catalog contract** — root-catalog
//! `docs/features/root-catalog/TECHNICAL.final.md` §4.1, §7.3 and the
//! roadmap's Plan 3.1, written for the private service to implement against
//! (`docs/cloud-platform/contracts/root-catalog-cloud-v2.md`). Additive and
//! versioned: every shape here carries [`Version2`], every v1 shape keeps
//! its meaning, and no v1 permit pair is required to admit a Workspace any
//! more.
//!
//! Five things cross here and nothing else:
//!
//! 1. **The account's one catalog binding** — get-or-create, idempotent;
//!    a device already bound to another account is refused with
//!    `catalog_binding_conflict`, never merged (§4.1).
//! 2. **Unit addressing** by kind and subject ([`CloudUnitRefV2`]), separate
//!    from content identity (`workspace_id`, `incarnation`,
//!    `logical_replica_id`, `lineage_id` stay what they are). The catalog
//!    scope is never in a body: it is the session's.
//! 3. **The per-unit join with watermarks**, mirroring peer protocol 6: an
//!    inventory with the service's projection watermark per unit, a request
//!    from the device's accepted frontier, updates from the export boundary,
//!    and a receipt that says applied / pending / unchanged / held. The same
//!    bodies are the HTTP shapes; over the Cloud carrier the Plan 2.1
//!    `PeerFrame` root frames carry them under a user-catalog or
//!    user-personal permit (`koru-replication::cloud::transfer::metadata`).
//! 4. **Separate admissions**: a sync token for one scope at a time —
//!    user catalog, one Workspace, one Workspace's personal unit, one
//!    Profile's Identity/Private units — each with its own authority
//!    generation ([`CloudSyncScopeV2`], [`SyncTokenRequestV2`]).
//! 5. **The discovery receipt** with scope, authorization generation,
//!    projection coverage and results grouped by **current placement** from
//!    the committed projection, so a move regroups a Workspace without
//!    counting it twice or declaring its former Profile empty before covered
//!    discovery (§7.3).
//!
//! No name, color, icon, transport mode, credential or content appears in
//! any of these, and no hidden subject's UUID: a unit the account's catalog
//! does not hold as Cloud-eligible is redacted or held on the way in and is
//! in no list, count or digest on the way out.

use koru_account_core::root_catalog::{SubjectLifecycle, WorkspacePlacement};
use koru_account_core::{CatalogScopeId, KoruIdentityId, ProfileId, WorkspaceId};
use koru_device::cloud::CloudReplicaPrincipalV1;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::cloud_sync::{SyncCredential, SyncUnavailableCode};
use crate::types::canonical_v4_workspace_id;
use crate::{CloudSyncRouteV1, Version2};

/// Discovery v2 page size: one receipt lists at most this many Workspaces
/// and this many Profiles; `next` carries the rest. The same bound as v1's
/// page (`cloud_selection::CLOUD_WORKSPACE_PAGE_SIZE`).
pub const DISCOVERY_PAGE_SIZE: usize = 64;

/// At most this many units in one `CloudRootCatalogV2`. A person's catalog
/// is Profiles plus Workspaces plus personal units; a catalog over this is
/// paged by the service with `next`.
pub const MAX_ROOT_CATALOG_UNITS: usize = 1024;

/// At most this many update frames in one `CloudRootUnitUpdatesV2`.
pub const MAX_ROOT_UNIT_FRAMES: usize = 64;

pub(crate) fn canonical_v4_profile_id<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<ProfileId, D::Error> {
    let value = String::deserialize(d)?;
    let id = uuid::Uuid::parse_str(&value).map_err(de::Error::custom)?;
    if id.to_string() != value
        || id.get_version() != Some(uuid::Version::Random)
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(de::Error::custom("invalid canonical v4 Profile UUID"));
    }
    Ok(ProfileId::from_uuid(id))
}

// ------------------------------------------------------------- binding

/// What the device already records as its account-scope binding
/// (`Store::bind_account_scope`), if anything. Declared so the service
/// refuses a device of another account before reading or writing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudCatalogBindingRefV2 {
    pub account_id: KoruIdentityId,
    pub catalog_scope_id: CatalogScopeId,
}

/// `PUT /v1/account/cloud/catalog/binding`. The account and the identity
/// come from the native session; the body is only what the device holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudCatalogBindingRequestV2 {
    pub version: Version2,
    pub held: Option<CloudCatalogBindingRefV2>,
}

/// The account's one catalog binding (§4.1). A second device, a second
/// verified identity of the account and a retry all get exactly this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudCatalogBindingV2 {
    pub version: Version2,
    pub account_id: KoruIdentityId,
    pub catalog_scope_id: CatalogScopeId,
    pub bound_at_ms: i64,
    /// 1 at creation; moves only if the service ever re-mints the scope,
    /// which nothing in this contract does.
    pub binding_generation: u64,
}

// ---------------------------------------------------------- addressing

/// One unit of the account's catalog: kind and subject, under the session's
/// scope. Request-side ids are canonical v4 or the body is refused.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudUnitRefV2 {
    ProfileCatalog {
        #[serde(deserialize_with = "canonical_v4_profile_id")]
        subject_id: ProfileId,
    },
    WorkspaceCatalog {
        #[serde(deserialize_with = "canonical_v4_workspace_id")]
        subject_id: WorkspaceId,
    },
    /// The user's personal unit for a Workspace, keyed by the Workspace
    /// alone under the account's scope (§5.1).
    PersonalV2 {
        #[serde(deserialize_with = "canonical_v4_workspace_id")]
        subject_id: WorkspaceId,
    },
}

impl From<koru_account_core::root_catalog::CloudUnitRef> for CloudUnitRefV2 {
    fn from(unit: koru_account_core::root_catalog::CloudUnitRef) -> Self {
        use koru_account_core::root_catalog::CloudUnitRef as Core;
        match unit {
            Core::ProfileCatalog { subject_id } => Self::ProfileCatalog { subject_id },
            Core::WorkspaceCatalog { subject_id } => Self::WorkspaceCatalog { subject_id },
            Core::PersonalV2 { subject_id } => Self::PersonalV2 { subject_id },
        }
    }
}

impl From<CloudUnitRefV2> for koru_account_core::root_catalog::CloudUnitRef {
    fn from(unit: CloudUnitRefV2) -> Self {
        match unit {
            CloudUnitRefV2::ProfileCatalog { subject_id } => Self::ProfileCatalog { subject_id },
            CloudUnitRefV2::WorkspaceCatalog { subject_id } => {
                Self::WorkspaceCatalog { subject_id }
            }
            CloudUnitRefV2::PersonalV2 { subject_id } => Self::PersonalV2 { subject_id },
        }
    }
}

// ----------------------------------------------------------- the join

/// One admitted unit as the service advertises it: its address, the
/// frontier Cloud holds (an encoded version vector) and how far the
/// discovery projection has committed it. The watermark is a per-unit
/// `i64` — the unit revision the projection has read — never a global
/// vector (§7.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudRootUnitStateV2 {
    pub unit: CloudUnitRefV2,
    pub frontier: Vec<u8>,
    pub projection_watermark: i64,
}

/// `GET /v1/account/cloud/catalog`: the account's admitted units, with a
/// digest over exactly them so equal digests end the root exchange. No
/// name, no scope, no hidden subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudRootCatalogV2 {
    pub version: Version2,
    pub digest: Vec<u8>,
    pub units: Vec<CloudRootUnitStateV2>,
    /// The last unit listed iff more follow; explicitly null otherwise.
    pub next: Option<CloudUnitRefV2>,
}

/// `POST /v1/account/cloud/catalog/units/pull`: ask for one unit from the
/// device's accepted frontier (its join receipt); the empty vector asks for
/// a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudRootUnitRequestV2 {
    pub version: Version2,
    pub unit: CloudUnitRefV2,
    pub frontier: Vec<u8>,
}

/// Bytes of one unit through the export boundary, in either direction: a
/// derived snapshot or updates since the requested frontier, never raw unit
/// bytes. On a push the device sends them (`POST
/// /v1/account/cloud/catalog/units/push`); on a pull the service answers
/// them with the watermark its projection holds for the unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudRootUnitUpdatesV2 {
    pub version: Version2,
    pub unit: CloudUnitRefV2,
    pub updates: Vec<Vec<u8>>,
    /// The service's watermark for the unit on a pull; on a push the device
    /// echoes the last watermark it saw, which the service may ignore.
    pub projection_watermark: i64,
}

/// Why a valid unit was held rather than stored or forwarded: the export
/// boundary's own reasons (§4.3) plus the two hosted preconditions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudHoldReasonV2 {
    /// The Workspace's current placement names a Profile whose unit the
    /// account's catalog does not hold as Cloud-eligible. Send that Profile's
    /// unit first, or the Workspace stays held.
    CurrentPlacementHidden,
    OwnerHidden,
    LifecycleDiffers,
    /// A personal unit whose Workspace catalog unit is not admitted yet.
    WorkspaceUnitRequired,
}

/// The service's answer to a push: the join receipt of §4.1 on the account
/// path. `frontier` is always the frontier Cloud holds after the call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudRootJoinReceiptV2 {
    /// Stored durably at `revision`; the projection advances to it
    /// separately and the watermark says when it has.
    Applied {
        version: Version2,
        unit: CloudUnitRefV2,
        frontier: Vec<u8>,
        revision: i64,
        projection_watermark: i64,
        /// How many historical references the hosted derivation redacted
        /// before storing. A number for the device's log, never a list.
        redacted_ops: u32,
    },
    /// Ancestors missing: nothing stored; send from `frontier`.
    Pending {
        version: Version2,
        unit: CloudUnitRefV2,
        frontier: Vec<u8>,
    },
    /// Nothing new; an idempotent re-send.
    Unchanged {
        version: Version2,
        unit: CloudUnitRefV2,
        frontier: Vec<u8>,
    },
    /// Valid but not storable now; retried when the hold reason clears.
    Held {
        version: Version2,
        unit: CloudUnitRefV2,
        reason: CloudHoldReasonV2,
    },
}

// ------------------------------------------------------------ permits

/// One admission scope (§7.3 "separately versioned user-catalog, workspace,
/// user-personal and optional Profile Identity/Private admissions, each
/// with its own eligibility and authority generation"). A token is for one
/// scope; a device holding several holds several tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudSyncScopeV2 {
    /// The account's root units: Profile and Workspace catalog units.
    UserCatalog,
    /// One Workspace's content, admitted on Workspace id, membership and
    /// incarnation. No Profile is named and none is required.
    Workspace {
        #[serde(deserialize_with = "canonical_v4_workspace_id")]
        workspace_id: WorkspaceId,
    },
    /// The account's personal unit for one Workspace. Follows the
    /// Workspace's policy plus the account's own authorization; no Profile.
    UserPersonal {
        #[serde(deserialize_with = "canonical_v4_workspace_id")]
        workspace_id: WorkspaceId,
    },
    /// One Profile's Identity and Private units, eligible only while a
    /// Cloud-enabled Workspace is currently placed under it (§4.4).
    ProfileUnits {
        #[serde(deserialize_with = "canonical_v4_profile_id")]
        profile_id: ProfileId,
    },
}

/// `POST /v1/account/cloud/sync-token`. Everything but the scope comes
/// from the native session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncTokenRequestV2 {
    pub version: Version2,
    pub scope: CloudSyncScopeV2,
}

/// Where the scope's session dials, or why not yet. Mirrors the v1
/// `DatasetRouteResultV1` without a dataset: the dataset, when there is
/// one, is on the response beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudRouteResultV2 {
    Available {
        route: CloudSyncRouteV1,
    },
    Unavailable {
        #[schemars(schema_with = "sync_unavailable_code_schema")]
        code: SyncUnavailableCode,
        #[schemars(range(min = 1, max = 300))]
        retry_after_seconds: u16,
    },
}

fn sync_unavailable_code_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"string","const":"sync_unavailable"})
}

/// One short-lived credential for one scope. `dataset` is the v1 dataset
/// principal for a `workspace` or `profile_units` scope — the same lineage
/// the v1 handshake admits — and null for `user_catalog` and
/// `user_personal`, whose session is the account catalog's. Shape is not
/// authority: the device correlates everything here with its own trusted
/// context, as it does for v1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncTokenResponseV2 {
    pub version: Version2,
    pub credential: SyncCredential,
    pub expires_at_seconds: i64,
    pub catalog_scope_id: CatalogScopeId,
    pub scope: CloudSyncScopeV2,
    /// The scope's eligibility and authority generation, which the permit
    /// built from this token carries and a stale permit is fenced by.
    pub authority_generation: u64,
    pub dataset: Option<CloudReplicaPrincipalV1>,
    pub route: CloudRouteResultV2,
}

impl SyncTokenResponseV2 {
    /// Shape only, the way `SyncTokenResponseV1::new` checks it: a dataset
    /// exactly for the content scopes, of the matching kind, never for the
    /// catalog scopes; positive expiry and generation.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.expires_at_seconds < 0 {
            return Err("invalid sync token expiry");
        }
        if self.authority_generation == 0 {
            return Err("invalid authority generation");
        }
        match (self.scope, self.dataset) {
            (
                CloudSyncScopeV2::Workspace { workspace_id },
                Some(CloudReplicaPrincipalV1::Workspace {
                    workspace_id: dataset,
                    ..
                }),
            ) if dataset.as_bytes() == workspace_id.as_uuid().as_bytes() => Ok(()),
            (
                CloudSyncScopeV2::ProfileUnits { profile_id },
                Some(CloudReplicaPrincipalV1::Profile {
                    profile_id: dataset,
                    ..
                }),
            ) if dataset.as_bytes() == profile_id.as_uuid().as_bytes() => Ok(()),
            (CloudSyncScopeV2::UserCatalog | CloudSyncScopeV2::UserPersonal { .. }, None) => Ok(()),
            _ => Err("sync token dataset does not match its scope"),
        }
    }
}

fn credential_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"string","minLength":1,"maxLength":crate::cloud_sync::MAX_SYNC_CREDENTIAL_BYTES})
}

fn dataset_schema(_: &mut SchemaGenerator) -> Schema {
    let uuid = serde_json::json!({"type":"string","format":"uuid",
        "pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"});
    schemars::json_schema!({"oneOf":[
        {"type":"null"},
        {"type":"object","additionalProperties":false,
            "required":["kind","workspace_id","incarnation_id","logical_replica_id"],
            "properties":{"kind":{"type":"string","const":"workspace"},
                "workspace_id":uuid,"incarnation_id":uuid,"logical_replica_id":uuid}},
        {"type":"object","additionalProperties":false,
            "required":["kind","profile_id","lineage_id","logical_replica_id"],
            "properties":{"kind":{"type":"string","const":"profile"},
                "profile_id":uuid,"lineage_id":uuid,"logical_replica_id":uuid}}
    ]})
}

impl JsonSchema for SyncTokenResponseV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SyncTokenResponseV2".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let scope = generator.subschema_for::<CloudSyncScopeV2>();
        let route = generator.subschema_for::<CloudRouteResultV2>();
        let catalog_scope = generator.subschema_for::<CatalogScopeId>();
        schemars::json_schema!({"type":"object","additionalProperties":false,
        "required":["version","credential","expires_at_seconds","catalog_scope_id","scope","authority_generation","dataset","route"],
        "properties":{
            "version":{"type":"integer","const":2},
            "credential":credential_schema(generator),
            "expires_at_seconds":{"type":"integer","minimum":0,"maximum":i64::MAX},
            "catalog_scope_id":catalog_scope,
            "scope":scope,
            "authority_generation":{"type":"integer","minimum":1},
            "dataset":dataset_schema(generator),
            "route":route
        }})
    }
}

// --------------------------------------------------------- descriptor

/// `GET /v1/account/cloud/workspaces/{workspace_id}/descriptor`:
/// identity-only bootstrap facts for one authorized Workspace, with its
/// **placement as a projection of the committed catalog** beside — not
/// inside — the content identity. Nothing here names a membership Profile,
/// and an explicitly unassigned Workspace is describable (`placement`
/// null), which the v1 descriptor could not say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceDescriptorV2 {
    pub version: Version2,
    pub workspace_id: WorkspaceId,
    #[schemars(with = "String")]
    pub workspace_incarnation_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub logical_replica_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub lineage_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub manifest_document_id: uuid::Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    pub placement: WorkspacePlacement,
}

// ---------------------------------------------------------- discovery

/// What a discovery pass asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudDiscoveryScopeV2 {
    /// The Profiles and their lifecycle, with the Workspaces currently
    /// placed under each.
    UserCatalog,
    /// The Workspaces the account may sync, with their placement.
    Workspaces,
}

/// How much of the admitted catalog the projection this receipt was read
/// from has committed. A count computed under `Partial` is a count of what
/// the projection has read, not of the catalog; a former Profile is not
/// declared empty while a unit that may still place something under it is
/// pending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudCoverageV2 {
    Complete,
    Partial { pending: Vec<CloudUnitRefV2> },
    Held { reason: CloudHoldReasonV2 },
}

/// The account's membership facts for one Workspace — separate from where
/// the Workspace is placed. These are what content admission reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudMembershipV2 {
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    /// The live membership also grants document writes.
    pub documents_write: bool,
}

/// One Workspace in a discovery receipt: content identity, the account's
/// membership, and its placement as the committed projection holds it.
/// `placement.placement` null is explicitly unassigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceCandidateV2 {
    pub workspace_id: WorkspaceId,
    #[schemars(with = "String")]
    pub workspace_incarnation_id: uuid::Uuid,
    pub lifecycle: SubjectLifecycle,
    pub membership: CloudMembershipV2,
    pub placement: WorkspacePlacement,
    pub projection_watermark: i64,
}

/// One Profile in a discovery receipt: lifecycle from its catalog unit,
/// whether its Identity/Private units are currently eligible (§4.4), and
/// the watermark the answer was read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudProfileResultV2 {
    pub profile_id: ProfileId,
    pub lifecycle: SubjectLifecycle,
    pub profile_units_eligible: bool,
    pub projection_watermark: i64,
}

/// The cursor of a discovery page: the last Workspace listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudDiscoveryCursorV2 {
    pub workspace_id: WorkspaceId,
}

/// `GET /v1/account/cloud/discovery?scope=…[&after_workspace_id=…]`.
/// Counts a client derives group by `results[*].placement`; a Workspace
/// appears once, under its current placement, and the receipt says how
/// much of the catalog that grouping covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudDiscoveryReceiptV2 {
    pub version: Version2,
    pub account_id: KoruIdentityId,
    pub catalog_scope_id: CatalogScopeId,
    /// The account's authorization generation the receipt was read under;
    /// a permit from an older generation does not correlate to it.
    pub authorization_generation: u64,
    /// The service's projection pass that produced the receipt. Monotonic
    /// per scope; a client correlates its own pass to it and never treats
    /// an older pass as newer evidence.
    pub pass: u64,
    pub scope: CloudDiscoveryScopeV2,
    pub coverage: CloudCoverageV2,
    pub profiles: Vec<CloudProfileResultV2>,
    pub results: Vec<CloudWorkspaceCandidateV2>,
    pub next: Option<CloudDiscoveryCursorV2>,
}

#[cfg(test)]
mod tests;
