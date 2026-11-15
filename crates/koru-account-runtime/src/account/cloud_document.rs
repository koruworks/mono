// SPDX-License-Identifier: MPL-2.0

//! The existing authenticated native Cloud carrier, supplied by each host.
use super::Cancel;
use koru_store_wire::cloud_document::{
    CloudDocumentAction, CloudDocumentFailure, CloudDocumentOutcome, CloudDocumentSelection,
};

/// The account service admits only a bounded exact selector from its current
/// trusted inventory page. Hosts independently bind it to their current
/// account, Workspace/incarnation and enabled ordinary content session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudDocumentContext {
    pub owner_scope: u64,
    pub account_generation: u64,
}

pub trait CloudDocumentPort: Send + Sync {
    /// `can_write` is a current account entitlement gate, combined with the
    /// host's live content permit; it is never read from a caller's DTO.
    /// Cancellation closes the inspection carrier. A possibly submitted
    /// removal returns OutcomeUnknown and must be followed by a Cloud reread.
    fn run(
        &self,
        context: CloudDocumentContext,
        selection: CloudDocumentSelection,
        action: CloudDocumentAction,
        can_write: bool,
        cancel: &Cancel,
    ) -> Result<CloudDocumentOutcome, CloudDocumentFailure>;
}

use super::*;
use koru_account_wire::cloud_plan::documents::{
    CloudUsageDocumentsQueryV1, CloudUsageDocumentsResponseV1, UnitRole,
};
use koru_agent_wire::{
    CloudDocumentInspectionPayload, CloudDocumentSelectionPayload, CloudInspectionFailure,
};

pub(super) struct InventoryPage {
    context: CloudDocumentContext,
    account: String,
    workspace: String,
    incarnation: String,
    cursor: Option<String>,
    rows: Vec<CloudDocumentSelection>,
}
impl InventoryPage {
    fn contains(&self, context: CloudDocumentContext, selection: &CloudDocumentSelection) -> bool {
        self.context == context
            && self.account == selection.account_id
            && self.workspace == selection.workspace_id
            && self.incarnation == selection.incarnation_id
            && self.rows.contains(selection)
    }
}
impl ProductionAccountService {
    pub(super) fn document_context(&self) -> Option<(CloudDocumentContext, String)> {
        if !self.visible.as_ref()?.load(Ordering::Acquire) {
            return None;
        }
        let owner = self.durable_owner?;
        let snapshot = self.cloud.as_ref()?.snapshot();
        if snapshot.account_id != Some(owner.account) {
            return None;
        }
        Some((
            CloudDocumentContext {
                owner_scope: snapshot.owner_scope?,
                account_generation: snapshot.account_generation,
            },
            owner.account.to_string(),
        ))
    }
    pub(super) fn remember_document_page(
        &mut self,
        context: Option<(CloudDocumentContext, String)>,
        cursor: Option<String>,
        page: Option<&CloudUsageDocumentsResponseV1>,
        cancel: &Cancel,
    ) {
        self.cloud_document_page = None;
        let Some((context, account)) = context else {
            return;
        };
        if cancel.cancelled() || self.document_context() != Some((context, account.clone())) {
            return;
        }
        let Some(page) = page else {
            return;
        };
        let rows = page
            .items
            .iter()
            .filter_map(|row| {
                if row.unit_role != UnitRole::Content {
                    return None;
                }
                let selection = CloudDocumentSelection {
                    account_id: account.clone(),
                    workspace_id: page.workspace_id.0.to_string(),
                    incarnation_id: page.incarnation_id.0.to_string(),
                    cloud_unit_id: row.cloud_unit_id.0.to_string(),
                    document_id: row.document_id?.0.to_string(),
                    source_head_digest: row.source_head_digest.clone()?,
                };
                selection.validate().then_some(selection)
            })
            .collect();
        self.cloud_document_page = Some(InventoryPage {
            context,
            account,
            workspace: page.workspace_id.0.to_string(),
            incarnation: page.incarnation_id.0.to_string(),
            cursor,
            rows,
        });
    }
    fn exact_document_action(
        &mut self,
        selection: &CloudDocumentSelection,
        action: CloudDocumentAction,
        cancel: &Cancel,
    ) -> Result<CloudDocumentOutcome, CloudDocumentFailure> {
        use CloudDocumentFailure::*;
        let (context, account) = self.document_context().ok_or(Unavailable)?;
        if cancel.cancelled() || account != selection.account_id {
            return Err(Unavailable);
        }
        let cached = self.cloud_document_page.as_ref().ok_or(SourceChanged)?;
        if !cached.contains(context, selection) {
            return Err(SourceChanged);
        }
        let cursor = cached.cursor.clone();
        let authorization = self.authorization().map_err(|_| Unavailable)?;
        let page = self
            .client
            .cloud_usage_documents(
                &authorization,
                cloud_inventory::read_uuid(&selection.workspace_id).map_err(|_| Unavailable)?,
                &CloudUsageDocumentsQueryV1 {
                    incarnation_id: cloud_inventory::read_uuid(&selection.incarnation_id)
                        .map_err(|_| Unavailable)?,
                    limit: Some(100),
                    cursor: cursor.clone(),
                },
            )
            .map_err(|error| match error {
                ClientError::Api { code, .. } if code.as_str() == "usage_revision_changed" => {
                    SourceChanged
                }
                _ => Unavailable,
            })?;
        self.remember_document_page(
            Some((context, account.clone())),
            cursor,
            Some(&page),
            cancel,
        );
        if cancel.cancelled() || self.document_context() != Some((context, account.clone())) {
            return Err(Unavailable);
        }
        if !self
            .cloud_document_page
            .as_ref()
            .is_some_and(|p| p.contains(context, selection))
        {
            return Err(SourceChanged);
        }
        // Re-read the server's current entitlement; neither the DTO nor an old
        // paid badge supplies a write grant. The host separately requires its
        // exact currently enabled ordinary Workspace session.
        let can_write = self
            .client
            .cloud_subscription_status(&self.authorization().map_err(|_| Unavailable)?, &|| {
                cancel.cancelled()
            })
            .is_ok_and(|status| status.cloud_access);
        if cancel.cancelled() || self.document_context() != Some((context, account.clone())) {
            return Err(Unavailable);
        }
        if matches!(action, CloudDocumentAction::RemoveFile { .. }) && !can_write {
            return Err(Unavailable);
        }
        let port = self.cloud_document.clone().ok_or(Unavailable)?;
        let removal = matches!(action, CloudDocumentAction::RemoveFile { .. });
        let result = port.run(context, selection.clone(), action, can_write, cancel);
        if removal {
            self.cloud_document_page = None;
        }
        if cancel.cancelled() || self.document_context() != Some((context, account)) {
            return Err(if removal { OutcomeUnknown } else { Unavailable });
        }
        match &result {
            Ok(CloudDocumentOutcome::View(view)) if !removal => {
                if view.selection != *selection || view.to_json().is_err() {
                    return Err(Unavailable);
                }
            }
            Ok(CloudDocumentOutcome::Removed) if removal => {}
            Ok(_) => return Err(Unavailable),
            Err(_) => {}
        }
        result
    }
    pub(super) fn cloud_document_command(
        &mut self,
        generation: u64,
        request_id: u64,
        encoded: CloudDocumentSelectionPayload,
        node_id: Option<String>,
        cancel: &Cancel,
    ) -> Result<ServiceOutcome, ServiceError> {
        let selection = CloudDocumentSelection::from_json(encoded.as_str())
            .map_err(|_| ServiceError::terminal("invalid_request"))?;
        let action = node_id
            .clone()
            .map(|node_id| CloudDocumentAction::RemoveFile { node_id })
            .unwrap_or(CloudDocumentAction::Inspect);
        if !action.validate() {
            return Err(ServiceError::terminal("invalid_request"));
        }
        let answer = self.exact_document_action(&selection, action, cancel);
        let failure = |error| match error {
            CloudDocumentFailure::Unavailable => CloudInspectionFailure::Unavailable,
            CloudDocumentFailure::SourceChanged => CloudInspectionFailure::SourceChanged,
            CloudDocumentFailure::OutcomeUnknown => CloudInspectionFailure::OutcomeUnknown,
        };
        let result = if let Some(node_id) = node_id {
            let (removed, failure) = match answer {
                Ok(CloudDocumentOutcome::Removed) => (true, None),
                Err(error) => (false, Some(failure(error))),
                _ => (false, Some(CloudInspectionFailure::Unavailable)),
            };
            AccountOperationResult::CloudFileRemoval {
                request_id,
                selection: encoded,
                node_id,
                removed,
                failure,
            }
        } else {
            let (payload, failure) = match answer {
                Ok(CloudDocumentOutcome::View(view)) => match view
                    .to_json()
                    .ok()
                    .and_then(|json| CloudDocumentInspectionPayload::new(json).ok())
                {
                    Some(payload) => (Some(payload), None),
                    None => (None, Some(CloudInspectionFailure::Unavailable)),
                },
                Err(error) => (None, Some(failure(error))),
                _ => (None, Some(CloudInspectionFailure::Unavailable)),
            };
            AccountOperationResult::CloudDocumentInspection {
                request_id,
                selection: encoded,
                payload,
                failure,
            }
        };
        Ok(ServiceOutcome {
            state: self.state(generation, AccountStage::SignedIn),
            credential: CredentialChange::Keep,
            result,
            follow_up: None,
        })
    }
}
