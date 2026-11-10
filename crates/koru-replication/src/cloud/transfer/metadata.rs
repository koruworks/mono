// SPDX-License-Identifier: MPL-2.0

//! Standard scoped requests and acknowledgements, not catalog discovery.
//! The account path's root and personal frames are [`root`], admitted only
//! under a user-catalog or user-personal permit.
use super::{Error, Record, Scope, Unit};
use crate::{cloud::Refusal, protocol::PeerFrame};

#[path = "metadata_root.rs"]
pub mod root;
use koru_device::cloud::CloudId;
use koru_store_core::profile_units::ProfileUnitRef;
use loro::VersionVector;
use loro_protocol::{CrdtType, Permission, ProtocolMessage as Message, UpdateStatusCode, encoding};
pub use root::{CarrierAdmission, RootMetadata, decode_root, is_root_frame};

/// Negotiated inside the existing authenticated dataset stream, never an ALPN
/// or an authorization grant. Older workers ignore join auth and omit extra.
pub const LIVE_ROOM_V1: &[u8] = b"koru/cloud-room/1";
pub const MAX_LIVE_ROOMS: usize = 32;

/// A subscriber's knowledge grows only from its own join VV or durable Ack.
pub fn confirmed_version(known: &[u8], delivered: &[u8]) -> Result<Vec<u8>, Error> {
    let mut known = VersionVector::decode(known).map_err(|_| Error::Invalid)?;
    known.merge(&VersionVector::decode(delivered).map_err(|_| Error::Invalid)?);
    Ok(known.encode())
}

pub struct Request {
    pub unit: Unit,
    pub known_version: Option<Vec<u8>>,
    join: bool,
    pub live: bool,
}
impl Request {
    fn new(unit: Unit, version: Vec<u8>, join: bool) -> Result<Self, Error> {
        let empty = version.is_empty()
            || VersionVector::decode(&version)
                .map_err(|_| Error::Invalid)?
                .is_empty();
        Ok(Self {
            unit,
            known_version: (!empty).then_some(version),
            join,
            live: false,
        })
    }
    pub fn needs_payload(&self, result: &[u8]) -> Result<bool, Error> {
        let ours = VersionVector::decode(result).map_err(|_| Error::Invalid)?;
        match &self.known_version {
            None => Ok(!ours.is_empty()),
            Some(known) => Ok(!VersionVector::decode(known)
                .map_err(|_| Error::Invalid)?
                .includes_vv(&ours)),
        }
    }
    pub fn response(&self, scope: Scope, version: Vec<u8>) -> Result<Option<Record>, Error> {
        if !self.join {
            return Ok(None);
        }
        let message = Message::JoinResponseOk {
            crdt: CrdtType::Loro,
            room_id: scope.room(self.unit)?,
            permission: Permission::Write,
            version,
            extra: self.live.then(|| LIVE_ROOM_V1.to_vec()),
        };
        Ok(Some(Record::Peer(PeerFrame::Loro {
            workspace_id: scope.eligibility_workspace,
            message: encoding::encode(&message).map_err(|_| Error::Invalid)?,
        })))
    }

    /// Worker-initiated catch-up uses the last version confirmed by this client,
    /// not the latest worker version or another subscriber's acknowledgement.
    pub fn push(unit: Unit, version: Vec<u8>) -> Result<Self, Error> {
        Self::new(unit, version, false)
    }
}

pub enum Metadata {
    Request(Request),
    Ack {
        unit: Unit,
        batch: [u8; 8],
        ok: bool,
    },
    SyncRequest,
    Compare(Vec<crate::protocol::CloudDocumentDigest>),
    Compared(Vec<koru_store_core::DocumentId>),
    Joined {
        unit: Unit,
        version: Vec<u8>,
        live: bool,
    },
    Leave(Unit),
    Ignore,
}

/// The ordinary Loro join request for a known, admitted unit.
pub fn join_request(scope: Scope, unit: Unit, version: Vec<u8>) -> Result<Record, Error> {
    join_request_mode(scope, unit, version, false)
}

pub fn join_request_mode(
    scope: Scope,
    unit: Unit,
    version: Vec<u8>,
    live: bool,
) -> Result<Record, Error> {
    Request::new(unit, version.clone(), true)?;
    let message = Message::JoinRequest {
        crdt: CrdtType::Loro,
        room_id: scope.room(unit)?,
        version,
        auth: if live {
            LIVE_ROOM_V1.to_vec()
        } else {
            Vec::new()
        },
    };
    Ok(Record::Peer(PeerFrame::Loro {
        workspace_id: scope.eligibility_workspace,
        message: encoding::encode(&message).map_err(|_| Error::Invalid)?,
    }))
}

/// Capacity comes from the existing owner, not an unbounded request queue.
/// Catalog/inventory dispatch is deliberately not fabricated here.
///
/// This is a v1 dataset session's decoder: a Workspace or Profile dataset.
/// The root-catalog frames of the account path are refused on it with
/// `principal_not_allowed` — a content session never reads or writes the
/// user's organization — and admitted by [`decode_root`] on a session whose
/// permit is the account catalog's.
pub fn decode(scope: Scope, frame: PeerFrame, capacity: usize) -> Result<Vec<Metadata>, Error> {
    if is_root_frame(&frame) {
        // `of(principal)` is never a catalog admission, so this refuses.
        decode_root(CarrierAdmission::of(scope.principal), &frame)?;
        return Err(Error::Invalid);
    }
    let mut output = Vec::new();
    let mut push = |value| {
        if output.len() >= capacity {
            tracing::warn!(
                capacity,
                "cloud transfer: unavailable, a frame carried more than the queue holds"
            );
            return Err(Error::Unavailable);
        }
        output.push(value);
        Ok(())
    };
    let messages = match frame {
        PeerFrame::CloudCompare {
            workspace_id,
            documents,
        } => {
            compare_scope(scope, workspace_id)?;
            if documents.is_empty()
                || documents.len() > crate::protocol::CLOUD_COMPARE_LIMIT
                || documents
                    .iter()
                    .map(|d| d.document_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != documents.len()
            {
                return Err(Error::Invalid);
            }
            push(Metadata::Compare(documents))?;
            return Ok(output);
        }
        PeerFrame::CloudCompared {
            workspace_id,
            matching,
        } => {
            compare_scope(scope, workspace_id)?;
            if matching.len() > crate::protocol::CLOUD_COMPARE_LIMIT
                || matching
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != matching.len()
            {
                return Err(Error::Invalid);
            }
            push(Metadata::Compared(matching))?;
            return Ok(output);
        }
        PeerFrame::ProfileRequest { unit, version } => {
            let profile = CloudId::from_bytes(unit.profile().as_uuid().into_bytes());
            let unit = match unit {
                ProfileUnitRef::Identity { .. } => Unit::Identity {
                    profile_id: profile,
                },
                ProfileUnitRef::Private { .. } => Unit::Private {
                    profile_id: profile,
                },
                ProfileUnitRef::Personal { workspace, .. } => Unit::Personal {
                    profile_id: profile,
                    workspace_id: CloudId::from_bytes(workspace.as_uuid().into_bytes()),
                },
                ProfileUnitRef::LegacyPrivate { .. } => return Err(Error::Invalid),
            };
            scope.room(unit)?;
            push(Metadata::Request(Request::new(unit, version, false)?))?;
            return Ok(output);
        }
        PeerFrame::Loro {
            workspace_id,
            message,
        } if workspace_id == scope.eligibility_workspace => vec![message],
        PeerFrame::LoroBatch {
            workspace_id,
            messages,
        } if workspace_id == scope.eligibility_workspace => messages,
        PeerFrame::SyncRequest => {
            push(Metadata::SyncRequest)?;
            return Ok(output);
        }
        PeerFrame::SyncAck => return Ok(output),
        PeerFrame::Error { code } => {
            return Err(match code.as_str() {
                "sync_transfer_invalid" => Error::Invalid,
                "sync_peer_closed" => Error::Closed,
                "sync_transfer_timeout" => Error::Timeout,
                "sync_policy_changed" => Error::PolicyChanged,
                "sync_update_too_large" => Error::TooLarge,
                "sync_unavailable" => Error::Unavailable,
                "sync_unit_exists" => Error::UnitExists,
                "sync_document_claimed" => Error::DocumentClaimed,
                _ => Error::Authority(Refusal::from_code(&code)),
            });
        }
        PeerFrame::CatalogRequest
        | PeerFrame::Catalog { .. }
        | PeerFrame::InventoryRequest { .. }
        | PeerFrame::Inventory(_) => {
            tracing::warn!(
                "cloud transfer: unavailable, a peer-mesh frame arrived on a Cloud stream"
            );
            return Err(Error::Unavailable);
        }
        _ => return Err(Error::Invalid),
    };
    if messages.is_empty() {
        return Err(Error::Invalid);
    }
    for encoded in messages {
        let message = encoding::try_decode(&encoded).ok_or(Error::Invalid)?;
        if encoding::encode(&message).map_err(|_| Error::Invalid)? != encoded {
            return Err(Error::Invalid);
        }
        let value = match message {
            Message::JoinRequest {
                crdt: CrdtType::Loro,
                room_id,
                version,
                auth,
                ..
            } => {
                let mut request = Request::new(scope.unit_for_room(&room_id)?, version, true)?;
                request.live =
                    auth == LIVE_ROOM_V1 && matches!(request.unit, Unit::Document { .. });
                Metadata::Request(request)
            }
            Message::Ack {
                crdt: CrdtType::Loro,
                room_id,
                ref_id,
                status,
            } => Metadata::Ack {
                unit: scope.unit_for_room(&room_id)?,
                batch: ref_id.0,
                ok: status == UpdateStatusCode::Ok,
            },
            Message::JoinResponseOk {
                crdt: CrdtType::Loro,
                room_id,
                version,
                extra,
                ..
            } => {
                VersionVector::decode(&version).map_err(|_| Error::Invalid)?;
                Metadata::Joined {
                    unit: scope.unit_for_room(&room_id)?,
                    version,
                    live: extra.as_deref() == Some(LIVE_ROOM_V1),
                }
            }
            Message::JoinError {
                crdt: CrdtType::Loro,
                room_id,
                ..
            }
            | Message::RoomError {
                crdt: CrdtType::Loro,
                room_id,
                ..
            } => {
                scope.unit_for_room(&room_id)?;
                Metadata::Ignore
            }
            Message::Leave {
                crdt: CrdtType::Loro,
                room_id,
                ..
            } => Metadata::Leave(scope.unit_for_room(&room_id)?),
            _ => return Err(Error::Invalid),
        };
        push(value)?;
    }
    Ok(output)
}

/// Cloud inventory is deliberately scoped. Do not enable peer CatalogRequest:
/// the signed dataset grants no access to another Workspace or Profile.
pub fn compare_scope(scope: Scope, workspace: koru_store_core::WorkspaceId) -> Result<(), Error> {
    if workspace != scope.eligibility_workspace
        || !matches!(scope.principal, koru_device::cloud::CloudReplicaPrincipalV1::Workspace { workspace_id, .. } if workspace_id.as_uuid() == workspace.as_uuid())
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
