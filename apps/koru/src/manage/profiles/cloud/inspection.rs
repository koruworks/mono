// SPDX-License-Identifier: MPL-2.0
//! One native exact-source inspection/removal lane. No Store or local editor.
use super::*;
use crate::manage::cloud::copy::{Intent as CopyIntent, Phase, Presentation, ViewKey};
use koru_agent_wire::{CloudDocumentSelectionPayload, CloudInspectionFailure};
use koru_store_wire::cloud_document::{CloudDocumentFailure, CloudDocumentSelection};
use std::time::{Duration, Instant};

struct Request {
    pending: Pending,
    deadline: Instant,
    key: ViewKey,
    node: Option<String>,
}
#[derive(Default)]
pub(crate) struct Inspection {
    pub view: Presentation,
    pub revision: u64,
    due: Option<Option<String>>,
    pending: Option<Request>,
    retired: Vec<inventory::Retired>,
}
impl Inspection {
    pub fn visible(&self) -> bool {
        self.view.key().is_some()
    }
    pub fn backlog(&self) -> usize {
        self.retired.len() + usize::from(self.pending.is_some())
    }
    fn retire(&mut self) {
        self.due = None;
        if let Some(request) = self.pending.take() {
            self.retired.push(inventory::Retired {
                pending: request.pending,
                cancelled: false,
            });
        }
    }
    pub fn close(&mut self) {
        self.retire();
        self.view.clear();
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn open(&mut self, owner: u64, selection: CloudDocumentSelection) {
        self.close();
        if self.view.open(ViewKey {
            owner_scope: owner,
            view_epoch: next_request_id(),
            selection,
        }) {
            self.due = Some(None);
        }
    }
    pub fn queue(&mut self, intent: CopyIntent) -> bool {
        self.revision = self.revision.wrapping_add(1);
        match intent {
            CopyIntent::Back => {
                self.close();
                false
            }
            CopyIntent::Reload(_) => {
                self.close();
                true
            }
            CopyIntent::Remove { key, node } => {
                if self.view.key() == Some(&key)
                    && self.view.phase() == Phase::Removing
                    && self.pending.is_none()
                {
                    self.due = Some(Some(node));
                }
                false
            }
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|r| r.deadline)
    }
    pub fn maintain(&mut self, now: Instant) -> bool {
        if !self.deadline().is_some_and(|due| now >= due) {
            return false;
        }
        let request = self.pending.as_ref().unwrap();
        let key = request.key.clone();
        let reason = if request.node.is_some() {
            CloudDocumentFailure::OutcomeUnknown
        } else {
            CloudDocumentFailure::Unavailable
        };
        self.retire();
        self.view.failed(&key, reason);
        self.revision = self.revision.wrapping_add(1);
        true
    }
    pub fn take(&mut self, now: Instant, reserved: usize) -> Option<AccountCommand> {
        self.maintain(now);
        if self.pending.is_some() {
            return None;
        }
        let node = self.due.take()?;
        let key = self.view.key()?.clone();
        if self.retired.len().saturating_add(reserved) >= 64 {
            self.view.failed(&key, CloudDocumentFailure::Unavailable);
            self.revision = self.revision.wrapping_add(1);
            return None;
        }
        let selection =
            CloudDocumentSelectionPayload::new(serde_json::to_string(&key.selection).ok()?).ok()?;
        let id = next_request_id();
        self.pending = Some(Request {
            pending: Pending {
                id,
                submission: CommandSubmissionId::next(),
                generation: None,
            },
            deadline: now + Duration::from_secs(30),
            key,
            node: node.clone(),
        });
        Some(match node {
            None => AccountCommand::InspectCloudDocument {
                request_id: id,
                selection,
            },
            Some(node_id) => AccountCommand::RemoveCloudFile {
                request_id: id,
                selection,
                node_id,
            },
        })
    }
    pub fn submission(&self, id: u64) -> Option<CommandSubmissionId> {
        self.pending
            .as_ref()
            .filter(|r| r.pending.id == id)
            .map(|r| r.pending.submission)
    }
    pub fn admit(&mut self, admission: CommandAdmission) -> bool {
        let pending = self
            .pending
            .as_mut()
            .map(|r| &mut r.pending)
            .filter(|p| p.submission == admission.submission)
            .or_else(|| {
                self.retired
                    .iter_mut()
                    .find(|r| r.pending.submission == admission.submission)
                    .map(|r| &mut r.pending)
            });
        if let Some(pending) = pending {
            pending.generation = Some(admission.generation);
            true
        } else {
            false
        }
    }
    pub fn fail(&mut self, failure: CommandFailure) -> bool {
        if let Some(request) = self
            .pending
            .take_if(|r| r.pending.submission == failure.submission)
        {
            self.view.failed(
                &request.key,
                if request.node.is_some() {
                    CloudDocumentFailure::OutcomeUnknown
                } else {
                    CloudDocumentFailure::Unavailable
                },
            );
            self.revision = self.revision.wrapping_add(1);
            return true;
        }
        let before = self.retired.len();
        self.retired
            .retain(|r| r.pending.submission != failure.submission);
        before != self.retired.len()
    }
    pub fn take_cancel(&mut self) -> Option<AccountCommand> {
        let row = self
            .retired
            .iter_mut()
            .find(|r| !r.cancelled && r.pending.generation.is_some())?;
        row.cancelled = true;
        Some(AccountCommand::Cancel {
            generation: row.pending.generation?,
        })
    }
    pub fn result(&mut self, generation: u64, result: &AccountOperationResult) -> Option<bool> {
        let id = match result {
            AccountOperationResult::CloudDocumentInspection { request_id, .. }
            | AccountOperationResult::CloudFileRemoval { request_id, .. } => *request_id,
            _ => return None,
        };
        if let Some(at) = self
            .retired
            .iter()
            .position(|r| r.pending.id == id || r.pending.generation == Some(generation))
        {
            self.retired.remove(at);
            // The view remains closed. A terminal removal may have changed
            // durable content even when its outcome is unknown locally.
            return Some(matches!(
                result,
                AccountOperationResult::CloudFileRemoval { .. }
            ));
        }
        let request = self.pending.take_if(|r| {
            r.pending.id == id && r.pending.generation.is_none_or(|g| g == generation)
        })?;
        self.revision = self.revision.wrapping_add(1);
        let mut acknowledged = false;
        match result {
            AccountOperationResult::CloudDocumentInspection {
                selection,
                payload,
                failure,
                ..
            } if request.node.is_none()
                && CloudDocumentSelection::from_json(selection.as_str())
                    .ok()
                    .as_ref()
                    == Some(&request.key.selection) =>
            {
                if let (Some(payload), None) = (payload, failure) {
                    self.view.install(&request.key, payload.as_str());
                } else {
                    self.view.failed(&request.key, reason(*failure));
                }
            }
            AccountOperationResult::CloudFileRemoval {
                selection,
                node_id,
                removed,
                failure,
                ..
            } if request.node.as_ref() == Some(node_id)
                && CloudDocumentSelection::from_json(selection.as_str())
                    .ok()
                    .as_ref()
                    == Some(&request.key.selection) =>
            {
                if *removed && failure.is_none() {
                    acknowledged = self.view.acknowledged(&request.key);
                } else {
                    self.view.failed(
                        &request.key,
                        if *removed || failure.is_none() {
                            CloudDocumentFailure::OutcomeUnknown
                        } else {
                            reason(*failure)
                        },
                    );
                }
            }
            _ => {
                self.view.failed(
                    &request.key,
                    if request.node.is_some() {
                        CloudDocumentFailure::OutcomeUnknown
                    } else {
                        CloudDocumentFailure::Unavailable
                    },
                );
            }
        }
        Some(acknowledged)
    }
}
fn reason(failure: Option<CloudInspectionFailure>) -> CloudDocumentFailure {
    match failure {
        Some(CloudInspectionFailure::SourceChanged) => CloudDocumentFailure::SourceChanged,
        Some(CloudInspectionFailure::OutcomeUnknown) => CloudDocumentFailure::OutcomeUnknown,
        _ => CloudDocumentFailure::Unavailable,
    }
}
impl ProfilesController {
    pub(crate) fn cloud_copy(&self) -> &Inspection {
        &self.cloud.inspection
    }
    pub(crate) fn cloud_copy_mut(&mut self) -> &mut Inspection {
        &mut self.cloud.inspection
    }
    pub(crate) fn open_cloud_copy(&mut self, unit: uuid::Uuid) {
        if !self.cloud_readable() {
            return;
        }
        let Some(inventory::PageData::Documents(page)) = &self.cloud.inventory.page else {
            return;
        };
        let Some(row) = page.items.iter().find(|r| r.cloud_unit_id.0 == unit) else {
            return;
        };
        if row.membership.state
            == koru_account_wire::cloud_plan::documents::MembershipState::Structural
        {
            return;
        }
        let (Some(account), Some(document), Some(head)) = (
            &self.account.active_account_id,
            row.document_id,
            &row.source_head_digest,
        ) else {
            return;
        };
        let selection = CloudDocumentSelection {
            account_id: account.clone(),
            workspace_id: page.workspace_id.0.to_string(),
            incarnation_id: page.incarnation_id.0.to_string(),
            cloud_unit_id: unit.to_string(),
            document_id: document.0.to_string(),
            source_head_digest: head.to_string(),
        };
        self.cloud
            .inspection
            .open(self.account.active_scope, selection);
    }
    pub(crate) fn cloud_copy_intent(&mut self, intent: CopyIntent) {
        if self.cloud.inspection.queue(intent) {
            self.cloud.inventory.workspaces();
            self.cloud.storage.usage_changed();
        }
    }
    pub(super) fn retire_cloud_copy(&mut self) {
        self.cloud.inspection.close();
        self.inventory_cancellations.capture_rows(
            self.account.active_scope,
            self.cloud.inspection.retired.drain(..),
        );
    }
}

#[cfg(test)]
mod tests;
