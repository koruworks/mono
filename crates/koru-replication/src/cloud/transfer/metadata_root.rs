// SPDX-License-Identifier: MPL-2.0

//! The root-catalog frames on the Cloud carrier (root-catalog Plan 3.1;
//! TECHNICAL.final §7.3 "Account attach creates/gets one catalog binding and
//! uses the same eligible-unit join protocol").
//!
//! Peer protocol 6's root frames — `RootCatalogRequest`/`RootCatalog`,
//! `RootUnitRequest`/`RootUnitUpdates`, `PersonalRequest`/`PersonalUpdates`
//! — are the same bodies the account path exchanges over HTTP
//! (`koru-account-wire::root_catalog`). On the carrier they are admitted
//! only under a permit whose scope is the account catalog's: root frames
//! under a **user-catalog** permit, personal frames under a **user-personal**
//! permit. A Workspace or Profile-units session — the two v1 dataset kinds —
//! refuses them with `principal_not_allowed`, so a content session can never
//! read or write the user's organization, exactly as a Workspace-granted
//! peer cannot (§7.2). Nothing here is authorization: the permit scope is
//! what the session was admitted under, and the Store's eligibility and
//! export boundary still decide what the frames may carry.

use koru_device::cloud::CloudReplicaPrincipalV1;
use koru_store_core::WorkspaceId;
use koru_store_core::root_catalog::CatalogUnitRef;

use super::Error;
use crate::cloud::Refusal;
use crate::protocol::{PeerFrame, PersonalUnitState, RootUnitState};

/// What the session's permit admits on this stream. The two content
/// admissions are the v1 dataset kinds and are derived from the handshake's
/// principal; the two catalog admissions come from a `CloudPermitV2`
/// `UserCatalog` / `UserPersonal` scope, which has no v1 dataset principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarrierAdmission {
    /// One Workspace's documents (a `CloudReplicaPrincipalV1::Workspace`).
    Workspace,
    /// One Profile's Identity/Private units (a `CloudReplicaPrincipalV1::Profile`).
    ProfileUnits,
    /// The account's root units.
    UserCatalog,
    /// The account's personal v2 units.
    UserPersonal,
}

impl CarrierAdmission {
    /// The admission a v1 dataset session has. A catalog session is never
    /// derived from a principal: its permit says what it is.
    pub fn of(principal: CloudReplicaPrincipalV1) -> Self {
        match principal {
            CloudReplicaPrincipalV1::Workspace { .. } => Self::Workspace,
            CloudReplicaPrincipalV1::Profile { .. } => Self::ProfileUnits,
        }
    }

    pub fn admits_root_frames(self) -> bool {
        matches!(self, Self::UserCatalog)
    }

    pub fn admits_personal_frames(self) -> bool {
        matches!(self, Self::UserPersonal)
    }
}

/// A root or personal frame as the carrier hands it on, already checked
/// against the session's admission. The bytes are the export boundary's;
/// the version vectors are passed through for the Store to decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootMetadata {
    RootCatalogRequest,
    RootCatalog {
        digest: Vec<u8>,
        units: Vec<RootUnitState>,
        personal: Vec<PersonalUnitState>,
    },
    RootUnitRequest {
        unit: CatalogUnitRef,
        version: Vec<u8>,
    },
    RootUnitUpdates {
        unit: CatalogUnitRef,
        updates: Vec<Vec<u8>>,
    },
    PersonalRequest {
        workspace: WorkspaceId,
        version: Vec<u8>,
    },
    PersonalUpdates {
        workspace: WorkspaceId,
        updates: Vec<Vec<u8>>,
    },
}

/// Whether `frame` is one of the root-catalog frames at all.
pub fn is_root_frame(frame: &PeerFrame) -> bool {
    matches!(
        frame,
        PeerFrame::RootCatalogRequest
            | PeerFrame::RootCatalog { .. }
            | PeerFrame::RootUnitRequest { .. }
            | PeerFrame::RootUnitUpdates { .. }
            | PeerFrame::PersonalRequest { .. }
            | PeerFrame::PersonalUpdates { .. }
    )
}

/// Classify one root or personal frame under `admission`. `Ok(None)` for a
/// frame that is not a root frame (the caller's ordinary `decode` owns it);
/// `principal_not_allowed` for a root frame on a session whose admission
/// does not cover it; `Invalid` for a root frame that does not validate.
pub fn decode_root(
    admission: CarrierAdmission,
    frame: &PeerFrame,
) -> Result<Option<RootMetadata>, Error> {
    let refuse = || Err(Error::Authority(Refusal::PrincipalNotAllowed));
    let value = match frame {
        PeerFrame::RootCatalogRequest => {
            if !admission.admits_root_frames() {
                return refuse();
            }
            RootMetadata::RootCatalogRequest
        }
        PeerFrame::RootCatalog {
            digest,
            units,
            personal,
        } => {
            if !admission.admits_root_frames() {
                return refuse();
            }
            for unit in units {
                unit.validate().map_err(|_| Error::Invalid)?;
            }
            for unit in personal {
                unit.validate().map_err(|_| Error::Invalid)?;
            }
            RootMetadata::RootCatalog {
                digest: digest.clone(),
                units: units.clone(),
                personal: personal.clone(),
            }
        }
        PeerFrame::RootUnitRequest { unit, version } => {
            if !admission.admits_root_frames() {
                return refuse();
            }
            version_ok(version)?;
            RootMetadata::RootUnitRequest {
                unit: *unit,
                version: version.clone(),
            }
        }
        PeerFrame::RootUnitUpdates { unit, updates } => {
            if !admission.admits_root_frames() {
                return refuse();
            }
            frames_ok(updates)?;
            RootMetadata::RootUnitUpdates {
                unit: *unit,
                updates: updates.clone(),
            }
        }
        PeerFrame::PersonalRequest { workspace, version } => {
            if !admission.admits_personal_frames() {
                return refuse();
            }
            version_ok(version)?;
            RootMetadata::PersonalRequest {
                workspace: *workspace,
                version: version.clone(),
            }
        }
        PeerFrame::PersonalUpdates { workspace, updates } => {
            if !admission.admits_personal_frames() {
                return refuse();
            }
            frames_ok(updates)?;
            RootMetadata::PersonalUpdates {
                workspace: *workspace,
                updates: updates.clone(),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

/// A request's version is the recipient's accepted frontier: an encoded
/// version vector, or empty for "send a snapshot".
fn version_ok(version: &[u8]) -> Result<(), Error> {
    if version.is_empty() {
        return Ok(());
    }
    loro::VersionVector::decode(version)
        .map(|_| ())
        .map_err(|_| Error::Invalid)
}

/// Updates carry at least one non-empty frame. Whether the bytes decode is
/// the Store's staging to decide, where a frame with missing ancestors is
/// `Pending` rather than invalid.
fn frames_ok(updates: &[Vec<u8>]) -> Result<(), Error> {
    if updates.is_empty() || updates.iter().any(Vec::is_empty) {
        return Err(Error::Invalid);
    }
    Ok(())
}
