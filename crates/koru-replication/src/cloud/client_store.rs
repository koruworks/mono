// SPDX-License-Identifier: MPL-2.0

//! Completed Cloud downloads enter the same durable Store used by both hosts.
//! This is synchronous Store work: the host must submit the entire Completed
//! value to its retained Store owner, never run this on a UI/transport executor.
//! No peer identity, listener, local mode representation or authority is minted.
mod export;
pub mod removal;
pub use export::{Export, ExportRequest, version};

use super::{
    Refusal,
    transfer::{self, Completed, Error, Record, Scope, Unit},
};
use koru_store_core::{
    DocumentId, ProfileId, Timestamp, WorkspaceId,
    errors::Code,
    ports::Catalog,
    profile_units::ProfileUnitRef,
    service::{Backend, Store},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Transfer(Error),
    Store(Code),
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Transfer(error)
    }
}
impl From<koru_store_core::Error> for Failure {
    fn from(error: koru_store_core::Error) -> Self {
        Self::Store(error.code())
    }
}

/// Constructible only after the existing Store persistence boundary succeeds.
/// It proves local persistence, not that an Ack was sent or the worker retained
/// the upload. The serving owner still applies its original session fences.
pub struct Receipt {
    unit: Unit,
    durable_sequence: Option<u64>,
    acknowledgement: Record,
}
impl Receipt {
    pub fn unit(&self) -> Unit {
        self.unit
    }
    pub fn durable_sequence(&self) -> Option<u64> {
        self.durable_sequence
    }
    pub fn acknowledgement(&self) -> &Record {
        &self.acknowledgement
    }
}

/// Execute after carrier handoff, inside the host's serialized Store owner.
/// The mandatory authorization port rechecks current local mode/account/scope
/// at execution time, not merely when a job was queued. A verified handshake
/// alone is not authorization to import. A policy change after completed
/// handoff is non-retroactive; revocation/lifecycle still belong to that port.
pub fn receive<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    scope: Scope,
    mut complete: Completed,
    at: Timestamp,
    authorize: impl FnOnce(&mut Store<C, B>, Scope, Unit) -> Result<(), Refusal>,
) -> Result<Receipt, Failure> {
    if complete.origin != scope.principal.update_origin()
        || complete.bytes.is_empty()
        || complete.bytes.len() > transfer::MAX_PAYLOAD
    {
        return Err(Error::Invalid.into());
    }
    scope.object_unit(complete.unit)?;
    // Build, but never expose, a success Ack before touching the Store. Thus
    // an encoding failure cannot happen after a successful persistent write.
    let acknowledgement = scope.committed_ack(complete.unit, complete.batch_id)?;
    authorize(store, scope, complete.unit).map_err(Error::Authority)?;
    let unit = complete.unit;
    let durable_sequence = match unit {
        Unit::Document {
            workspace_id,
            document_id,
        } => {
            let workspace = WorkspaceId::parse(&workspace_id.to_string()).ok_or(Error::Invalid)?;
            let document = DocumentId::parse(&document_id.to_string()).ok_or(Error::Invalid)?;
            if store.manifest_id(workspace)? != scope.manifest {
                return Err(Error::Invalid.into());
            }
            // Move, do not clone, the payload. The emptied Completed still owns
            // its original reservation; the commit window keeps that reservation
            // with these exact bytes even if flush fails and the caller goes away.
            let bytes = std::mem::take(&mut complete.bytes);
            let origin = complete.origin.clone();
            store.apply_update_retained(workspace, document, bytes, at, origin, complete)?;
            store.flush(workspace, document, at)?
        }
        Unit::Identity { profile_id }
        | Unit::Private { profile_id }
        | Unit::Personal { profile_id, .. } => {
            let profile = ProfileId::parse(&profile_id.to_string()).ok_or(Error::Invalid)?;
            let profile_unit = match unit {
                Unit::Identity { .. } => ProfileUnitRef::Identity { profile },
                Unit::Private { .. } => ProfileUnitRef::Private { profile },
                Unit::Personal { workspace_id, .. } => ProfileUnitRef::Personal {
                    owner: profile,
                    workspace: WorkspaceId::parse(&workspace_id.to_string())
                        .ok_or(Error::Invalid)?,
                },
                Unit::Document { .. } => unreachable!(),
            };
            // This existing path stages and causally validates before its
            // atomic catalog commit, then publishes the live Profile state.
            // Unlike workspace updates, it needs no separate flush.
            store
                .apply_profile_unit_updates(profile_unit, std::slice::from_ref(&complete.bytes))?;
            None
        }
    };
    Ok(Receipt {
        unit,
        durable_sequence,
        acknowledgement,
    })
}
