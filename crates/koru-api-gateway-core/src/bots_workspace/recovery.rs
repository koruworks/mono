// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_mcp_server::native::{NativeReconciliation, NativeWriteDescription, NativeWriteKind};
use koru_store_core::{NativeEffectKind, Timestamp};

impl Boundary {
    pub(super) fn recover(
        &self,
        request: workspace::RecoveryRequest,
    ) -> Result<workspace::RecoveryReply, Error> {
        match request {
            workspace::RecoveryRequest::Fence { record, force } => {
                let lease = record.lease.as_ref().ok_or(Error::LeaseMismatch)?;
                // A live boundary may still be executing. Only the engine's
                // interrupted/requested state or a changed incarnation starts
                // its permanent drain. A timer alone is never absence proof.
                if !force && lease.boundary == self.incarnation {
                    return Ok(workspace::RecoveryReply::Pending);
                }
                let binding = record.store;
                let drain = self
                    .store
                    .with_store(move |store| store.drain_native_effect(&binding, Timestamp::now()))
                    .map_err(|_| Error::StorageUnavailable)?
                    .map_err(|_| Error::StorageUnavailable)?;
                Ok(workspace::RecoveryReply::Drained(drain))
            }
            workspace::RecoveryRequest::Lookup { record } => {
                if !record
                    .recovery
                    .drain
                    .as_ref()
                    .is_some_and(|d| d.binding == record.store)
                {
                    return Err(Error::RecoveryPending);
                }
                let backend = self.backend(ReadAuthority {
                    owner: record.store.user_scope,
                    workspace: record.store.workspace,
                })?;
                let description = NativeWriteDescription {
                    kind: match record.prepared.kind {
                        NativeEffectKind::CreateDocument => NativeWriteKind::Created,
                        NativeEffectKind::EditDocument => NativeWriteKind::Edited,
                    },
                    workspace: record.prepared.workspace,
                    document: record.prepared.document,
                    fingerprint: record
                        .prepared
                        .fingerprint
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    expected_revision: record.prepared.expected_revision.clone(),
                    title: record.prepared.title.clone(),
                };
                match backend
                    .reconcile_native_description(&description, writes::identity(&record))
                    .map_err(native_error)?
                {
                    NativeReconciliation::Verified(evidence) => {
                        Ok(workspace::RecoveryReply::Evidence(Some(writes::outcome(
                            &record, evidence,
                        ))))
                    }
                    NativeReconciliation::Unconfirmed => {
                        Ok(workspace::RecoveryReply::Evidence(None))
                    }
                }
            }
        }
    }
}
