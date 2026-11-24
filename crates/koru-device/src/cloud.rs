// SPDX-License-Identifier: MPL-2.0

//! Durable Cloud identity is separate from device identity and trust records.
//! These strict wire values confer no authority until application admission.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::ReplicationEndpointId;

/// A canonical lowercase hyphenated UUID at the Cloud wire boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CloudId(Uuid);

impl std::fmt::Display for CloudId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl CloudId {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Uuid::from_bytes(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// The amendment requires canonical UUIDv4 for every Cloud identity it
    /// names. Decoding already refuses a noncanonical *encoding*; this is the
    /// separate version check a `validate()` runs before admission.
    pub fn is_v4(&self) -> bool {
        self.0.get_version_num() == 4
    }
}

impl Serialize for CloudId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.hyphenated().to_string())
    }
}

impl<'de> Deserialize<'de> for CloudId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let id = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
        if id.hyphenated().to_string() != value {
            return Err(serde::de::Error::custom("noncanonical Cloud UUID"));
        }
        Ok(Self(id))
    }
}

/// Exactly one retained dataset lineage. Runtime placement never changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudReplicaPrincipalV1 {
    Workspace {
        workspace_id: CloudId,
        incarnation_id: CloudId,
        logical_replica_id: CloudId,
    },
    Profile {
        profile_id: CloudId,
        lineage_id: CloudId,
        logical_replica_id: CloudId,
    },
}

impl CloudReplicaPrincipalV1 {
    pub fn logical_replica_id(self) -> CloudId {
        match self {
            Self::Workspace {
                logical_replica_id, ..
            }
            | Self::Profile {
                logical_replica_id, ..
            } => logical_replica_id,
        }
    }

    pub fn update_origin(self) -> koru_store_core::ports::UpdateOrigin {
        koru_store_core::ports::UpdateOrigin::Cloud {
            logical_replica_id: *self.logical_replica_id().as_bytes(),
        }
    }

    /// Cloud must never be converted to a bilateral device trust principal.
    pub fn admit_on_device_path(self) -> Result<(), &'static str> {
        Err("principal_not_allowed")
    }

    /// True for the Workspace arm, which is the only dataset kind that carries
    /// a Workspace descriptor and the only one `read_cloud_bootstrap` accepts.
    pub fn is_workspace(self) -> bool {
        matches!(self, Self::Workspace { .. })
    }
}

/// One live Cloud authority context at the network edge, per HOST-CONTROL §2.
///
/// **Nothing here is secret, and `Debug` therefore redacts nothing.** The
/// credential is named by its signed `jti` (`credential_id`) and never by its
/// bearer bytes; the endpoints are public Ed25519 keys; the epochs and
/// generations are bookkeeping. A value of this type confers no authority on
/// its own — it is what an admission check is run *against*, the same rule
/// [`CloudReplicaPrincipalV1`] states above.
///
/// No lease or worker id is carried (D231): the device never learns or
/// verifies one, and the worker fences itself on its own lease rows.
///
/// `account_id` is the authorizing `KoruIdentityId`'s UUID; account-core sits
/// above this crate, so it is carried here as a [`CloudId`].
/// `control_session_id` is `koru_sync_control::ControlSessionId`'s transparent
/// UUID, converted at the sync-control layer for the same reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPrincipalV1 {
    pub version: u32,
    pub permit_id: Uuid,
    pub account_id: CloudId,
    pub profile_id: CloudId,
    pub dataset: CloudReplicaPrincipalV1,
    pub client_endpoint_id: ReplicationEndpointId,
    /// The endpoint named by the issued route and authenticated by the live
    /// connection — not a caller-supplied address.
    pub worker_endpoint_id: ReplicationEndpointId,
    /// The signed `jti` of the admitted credential, never its bearer bytes.
    pub credential_id: CloudId,
    pub authorization_version: u64,
    pub membership_epoch: u64,
    pub cloud_access_epoch: u64,
    pub authority_workspace_id: CloudId,
    pub authority_workspace_incarnation_id: CloudId,
    pub control_session_id: Uuid,
    pub account_generation: u64,
    pub session_generation: u64,
    /// The admitted credential's expiry (D231: not a route-proof expiry).
    pub expires_at_seconds: i64,
    /// The admitted credential's signed issue time (`iat`): the API's clock,
    /// verified with the credential, and accepted by the worker for this
    /// session. The Store trusts it as "the Cloud's time from its last
    /// successful session" when the 30-day trash sweep decides what is due
    /// (`koru_core::trash_expiry`). `None` from an Agent built before it was
    /// carried; such a session vouches for no time and the sweep waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_issued_at_seconds: Option<i64>,
}

pub const CLOUD_PRINCIPAL_VERSION: u32 = 1;

impl CloudPrincipalV1 {
    /// Shape validation only. Admission still checks this value against a live
    /// permit, the current authority and the authenticated remote endpoint.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != CLOUD_PRINCIPAL_VERSION {
            return Err("cloud_principal_version");
        }
        if self.permit_id.get_version_num() != 4 {
            return Err("cloud_principal_permit_id");
        }
        if !self.account_id.is_v4()
            || !self.profile_id.is_v4()
            || !self.credential_id.is_v4()
            || !self.authority_workspace_id.is_v4()
            || !self.authority_workspace_incarnation_id.is_v4()
        {
            return Err("cloud_principal_identity");
        }
        if self.control_session_id.is_nil() {
            return Err("cloud_principal_control_session");
        }
        // The three Workspace authority epochs describe a real credential
        // context; zero is the absence of one, not a generation.
        if self.authorization_version == 0
            || self.membership_epoch == 0
            || self.cloud_access_epoch == 0
        {
            return Err("cloud_principal_authority_epoch");
        }
        if self.account_generation == 0 {
            return Err("cloud_principal_account_generation");
        }
        if self.session_generation == 0 {
            return Err("cloud_principal_session_generation");
        }
        if self.expires_at_seconds <= 0 {
            return Err("cloud_principal_expiry");
        }
        self.client_endpoint_id
            .validate_cloud_key()
            .map_err(|_| "cloud_principal_client_endpoint")?;
        self.worker_endpoint_id
            .validate_cloud_key()
            .map_err(|_| "cloud_principal_worker_endpoint")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
