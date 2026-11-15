// SPDX-License-Identifier: MPL-2.0

//! One authoritative page, bounded cursor history, and no local Store lookup.
use super::*;
use koru_account_wire::cloud_plan::documents::*;
use koru_account_wire::cloud_plan::*;
use koru_agent_wire::{CloudInventoryFailure, CloudInventoryPayload};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Selection {
    #[default]
    Workspaces,
    Documents {
        workspace: ReadUuid,
        incarnation: ReadUuid,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PageData {
    Workspaces(CloudUsageWorkspacesResponseV1),
    Documents(CloudUsageDocumentsResponseV1),
}
impl PageData {
    pub fn cursor(&self) -> Option<&str> {
        match self {
            Self::Workspaces(p) => p.next_cursor.as_deref(),
            Self::Documents(p) => p.next_cursor.as_deref(),
        }
    }
    fn same_epoch(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Workspaces(a), Self::Workspaces(b)) => {
                a.revision == b.revision
                    && a.generation == b.generation
                    && a.catalog_revision_digest == b.catalog_revision_digest
            }
            (Self::Documents(a), Self::Documents(b)) => {
                a.revision == b.revision
                    && a.generation == b.generation
                    && a.workspace_id == b.workspace_id
                    && a.incarnation_id == b.incarnation_id
                    && a.source_revision == b.source_revision
                    && a.inventory_digest == b.inventory_digest
                    && a.manifest == b.manifest
            }
            _ => false,
        }
    }
}
struct Request {
    pending: Pending,
    deadline: Instant,
}
pub(super) struct Retired {
    pub(super) pending: Pending,
    pub(super) cancelled: bool,
}
#[derive(Default)]
pub(crate) struct Cancellations(Vec<(u64, Retired)>);
impl Cancellations {
    pub(super) fn capture_rows(&mut self, owner: u64, rows: impl Iterator<Item = Retired>) {
        self.0.extend(rows.map(|row| (owner, row)));
        debug_assert!(self.0.len() <= 64);
    }
    pub(crate) fn capture(&mut self, owner: u64, inventory: &mut Inventory) {
        inventory.hide();
        self.0
            .extend(inventory.retired.drain(..).map(|p| (owner, p)));
        debug_assert!(
            self.0.len() <= 64,
            "inventory admission bounds shared retirements"
        );
    }
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn take(&mut self) -> Option<(u64, AccountCommand)> {
        let (owner, retired) = self
            .0
            .iter_mut()
            .find(|(_, r)| !r.cancelled && r.pending.generation.is_some())?;
        retired.cancelled = true;
        Some((
            *owner,
            AccountCommand::Cancel {
                generation: retired.pending.generation?,
            },
        ))
    }
    pub(crate) fn admit(&mut self, admission: CommandAdmission) -> bool {
        let Some((_, retired)) = self.0.iter_mut().find(|(owner, r)| {
            *owner == admission.owner_scope && r.pending.submission == admission.submission
        }) else {
            return false;
        };
        retired.pending.generation = Some(admission.generation);
        true
    }
    pub(crate) fn fail(&mut self, failure: CommandFailure) -> bool {
        let before = self.0.len();
        self.0
            .retain(|(_, r)| r.pending.submission != failure.submission);
        before != self.0.len()
    }
    pub(crate) fn result(
        &mut self,
        owner: u64,
        generation: u64,
        result: &AccountOperationResult,
    ) -> bool {
        let id = match result {
            AccountOperationResult::CloudUsageWorkspaces { request_id, .. }
            | AccountOperationResult::CloudUsageDocuments { request_id, .. }
            | AccountOperationResult::CloudDocumentInspection { request_id, .. }
            | AccountOperationResult::CloudFileRemoval { request_id, .. } => Some(*request_id),
            _ => None,
        };
        let before = self.0.len();
        self.0.retain(|(scope, r)| {
            *scope != owner
                || !(r.pending.generation == Some(generation) || id == Some(r.pending.id))
        });
        before != self.0.len()
    }
}
#[derive(Default)]
pub(crate) struct Inventory {
    pub revision: u64,
    pub visible: bool,
    pub selection: Selection,
    pub page: Option<PageData>,
    pub failed: bool,
    pub restarted: bool,
    pub details: std::collections::BTreeSet<uuid::Uuid>,
    pub profile_details: std::collections::BTreeSet<uuid::Uuid>,
    due: bool,
    pending: Option<Request>,
    // An admission can arrive after hiding. Retain its submission so it can
    // still be cancelled. Bound even a broken command transport's backlog.
    retired: Vec<Retired>,
    cursor: Option<String>,
    history: Vec<Option<String>>,
}
impl Inventory {
    pub(super) fn backlog(&self) -> usize {
        self.retired.len() + usize::from(self.pending.is_some())
    }
    pub fn checking(&self) -> bool {
        self.due || self.pending.is_some()
    }
    pub fn first_page(&self) -> bool {
        self.cursor.is_none()
    }
    pub fn can_previous(&self) -> bool {
        !self.history.is_empty()
    }
    fn retire(&mut self) {
        if let Some(request) = self.pending.take() {
            self.retired.push(Retired {
                pending: request.pending,
                cancelled: false,
            });
        }
    }
    fn clear(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.retire();
        self.page = None;
        self.cursor = None;
        self.history.clear();
        self.details.clear();
        self.profile_details.clear();
        self.failed = false;
    }
    pub fn open(&mut self) {
        self.clear();
        self.visible = true;
        self.selection = Selection::Workspaces;
        self.restarted = false;
        self.due = true;
    }
    pub fn hide(&mut self) {
        self.clear();
        self.visible = false;
        self.due = false;
    }
    pub fn suspend(&mut self) {
        self.clear();
        self.due = self.visible;
        self.restarted = false;
    }
    pub fn first(&mut self) {
        self.clear();
        self.restarted = false;
        self.due = true;
    }
    pub fn workspaces(&mut self) {
        self.selection = Selection::Workspaces;
        self.first();
    }
    pub fn documents(&mut self, workspace: ReadUuid, incarnation: ReadUuid) -> bool {
        if !matches!(&self.page, Some(PageData::Workspaces(p)) if p.items.iter().any(|r| r.workspace_id == workspace && r.incarnation_id == incarnation))
        {
            return false;
        }
        self.clear();
        self.selection = Selection::Documents {
            workspace,
            incarnation,
        };
        self.restarted = false;
        self.due = true;
        true
    }
    pub fn next(&mut self) {
        if self.checking() {
            return;
        }
        let Some(next) = self
            .page
            .as_ref()
            .and_then(PageData::cursor)
            .map(str::to_owned)
        else {
            return;
        };
        self.history.push(self.cursor.take());
        self.revision = self.revision.wrapping_add(1);
        if self.history.len() > 32 {
            self.history.remove(0);
        }
        self.cursor = Some(next);
        self.details.clear();
        self.profile_details.clear();
        self.due = true;
    }
    pub fn previous(&mut self) {
        if self.checking() {
            return;
        }
        if let Some(cursor) = self.history.pop() {
            self.revision = self.revision.wrapping_add(1);
            self.cursor = cursor;
            self.details.clear();
            self.profile_details.clear();
            self.due = true;
        }
    }
    pub fn toggle_details(&mut self, id: uuid::Uuid) {
        self.revision = self.revision.wrapping_add(1);
        let present = match &self.page {
            Some(PageData::Workspaces(p)) => p.items.iter().any(|r| r.workspace_id.0 == id),
            Some(PageData::Documents(p)) => p.items.iter().any(|r| r.cloud_unit_id.0 == id),
            None => false,
        };
        if present && !self.details.remove(&id) {
            self.details.insert(id);
        }
    }
    pub fn toggle_profile_details(&mut self, id: uuid::Uuid) {
        self.revision = self.revision.wrapping_add(1);
        if matches!(&self.page, Some(PageData::Workspaces(p)) if p.items.iter().any(|r| r.placement.profile_id.is_some_and(|p| p.0 == id)))
            && !self.profile_details.remove(&id)
        {
            self.profile_details.insert(id);
        }
    }
    fn restart(&mut self) {
        let used = self.restarted;
        self.clear();
        self.selection = Selection::Workspaces;
        self.restarted = true;
        self.failed = used;
        self.due = !used;
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|p| p.deadline)
    }
    pub fn maintain(&mut self, now: Instant) -> bool {
        if !self.deadline().is_some_and(|deadline| now >= deadline) {
            return false;
        }
        self.retire();
        self.page = None;
        self.failed = true;
        self.due = false;
        self.revision = self.revision.wrapping_add(1);
        true
    }
    pub fn take_cancel(&mut self) -> Option<AccountCommand> {
        let retired = self
            .retired
            .iter_mut()
            .find(|p| !p.cancelled && p.pending.generation.is_some())?;
        retired.cancelled = true;
        Some(AccountCommand::Cancel {
            generation: retired.pending.generation?,
        })
    }
    pub fn take(&mut self, now: Instant) -> Option<AccountCommand> {
        self.maintain(now);
        if !self.visible || !self.due || self.pending.is_some() {
            return None;
        }
        self.due = false;
        if self.retired.len() >= 64 {
            self.revision = self.revision.wrapping_add(1);
            self.failed = true;
            self.page = None;
            return None;
        }
        let id = next_request_id();
        self.pending = Some(Request {
            pending: Pending {
                id,
                submission: CommandSubmissionId::next(),
                generation: None,
            },
            deadline: now + Duration::from_secs(30),
        });
        Some(match self.selection {
            Selection::Workspaces => AccountCommand::InspectCloudUsageWorkspaces {
                request_id: id,
                cursor: self.cursor.clone(),
            },
            Selection::Documents {
                workspace,
                incarnation,
            } => AccountCommand::InspectCloudUsageDocuments {
                request_id: id,
                workspace_id: workspace.0.to_string(),
                incarnation_id: incarnation.0.to_string(),
                cursor: self.cursor.clone(),
            },
        })
    }
    pub(crate) fn capacity(&mut self, reserved: usize) -> bool {
        if reserved + self.retired.len() < 64 {
            return true;
        }
        if self.pending.is_none() && self.due {
            self.due = false;
            self.failed = true;
            self.page = None;
            self.revision = self.revision.wrapping_add(1);
        }
        false
    }
    pub fn submission(&self, id: u64) -> Option<CommandSubmissionId> {
        self.pending
            .as_ref()
            .filter(|p| p.pending.id == id)
            .map(|p| p.pending.submission)
    }
    pub fn admit(&mut self, admission: CommandAdmission) -> bool {
        if let Some(p) = self
            .pending
            .as_mut()
            .map(|p| &mut p.pending)
            .filter(|p| p.submission == admission.submission)
        {
            p.generation = Some(admission.generation);
            return true;
        }
        if let Some(p) = self
            .retired
            .iter_mut()
            .find(|p| p.pending.submission == admission.submission)
        {
            p.pending.generation = Some(admission.generation);
            return true;
        }
        false
    }
    pub fn fail(&mut self, failure: CommandFailure) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.pending.submission == failure.submission)
        {
            self.pending = None;
            self.page = None;
            self.failed = true;
            self.revision = self.revision.wrapping_add(1);
            return true;
        }
        let before = self.retired.len();
        self.retired
            .retain(|p| p.pending.submission != failure.submission);
        before != self.retired.len()
    }
    pub fn result(&mut self, generation: u64, result: &AccountOperationResult) -> bool {
        let result_id = match result {
            AccountOperationResult::CloudUsageWorkspaces { request_id, .. }
            | AccountOperationResult::CloudUsageDocuments { request_id, .. } => Some(*request_id),
            _ => None,
        };
        if let Some(index) = self.retired.iter().position(|p| {
            p.pending.generation == Some(generation) || result_id == Some(p.pending.id)
        }) {
            self.retired.remove(index);
            return true;
        }
        let (id, payload, failure, selection): (_, Option<&CloudInventoryPayload>, _, _) =
            match result {
                AccountOperationResult::CloudUsageWorkspaces {
                    request_id,
                    payload,
                    failure,
                } => (
                    *request_id,
                    payload.as_ref(),
                    *failure,
                    Selection::Workspaces,
                ),
                AccountOperationResult::CloudUsageDocuments {
                    request_id,
                    workspace_id,
                    incarnation_id,
                    payload,
                    failure,
                } => {
                    let (Ok(workspace), Ok(incarnation)) = (
                        uuid::Uuid::parse_str(workspace_id),
                        uuid::Uuid::parse_str(incarnation_id),
                    ) else {
                        return false;
                    };
                    (
                        *request_id,
                        payload.as_ref(),
                        *failure,
                        Selection::Documents {
                            workspace: ReadUuid(workspace),
                            incarnation: ReadUuid(incarnation),
                        },
                    )
                }
                _ => return false,
            };
        let Some(p) = &self.pending else {
            return false;
        };
        if p.pending.id != id || !p.pending.generation.is_none_or(|g| g == generation) {
            return false;
        }
        self.pending = None;
        self.revision = self.revision.wrapping_add(1);
        if self.selection != selection || (payload.is_some() && failure.is_some()) {
            self.page = None;
            self.failed = true;
            return true;
        }
        if failure == Some(CloudInventoryFailure::RevisionChanged) {
            self.restart();
            return true;
        }
        let page = payload.and_then(|payload| match selection {
            Selection::Workspaces => serde_json::from_str(payload.as_str())
                .ok()
                .map(PageData::Workspaces),
            Selection::Documents {
                workspace,
                incarnation,
            } => serde_json::from_str::<CloudUsageDocumentsResponseV1>(payload.as_str())
                .ok()
                .filter(|p| p.workspace_id == workspace && p.incarnation_id == incarnation)
                .map(PageData::Documents),
        });
        let Some(page) = page.filter(|_| failure.is_none()) else {
            self.page = None;
            self.failed = true;
            return true;
        };
        if self.page.as_ref().is_some_and(|old| !old.same_epoch(&page)) {
            self.restart();
            return true;
        }
        self.page = Some(page);
        self.failed = false;
        true
    }
    pub fn profile_total(&self, profile: ReadUuid) -> Option<i64> {
        let Some(PageData::Workspaces(page)) = &self.page else {
            return None;
        };
        if !self.first_page()
            || page.next_cursor.is_some()
            || page
                .items
                .iter()
                .any(|r| r.placement.state == PlacementState::Unknown)
        {
            return None;
        }
        let mut total = 0i64;
        let mut found = false;
        for row in page
            .items
            .iter()
            .filter(|r| r.placement.profile_id == Some(profile))
        {
            if row.usage.state != UsageState::Known {
                return None;
            }
            total = total.checked_add(row.usage.files_bytes?.get())?;
            found = true;
        }
        found.then_some(total)
    }
}

impl ProfilesController {
    pub(crate) fn retire_cloud_inventory(&mut self) {
        self.retire_cloud_copy();
        self.inventory_cancellations
            .capture(self.account.active_scope, &mut self.cloud.inventory);
    }
    pub(crate) fn cloud_inventory(&self) -> &Inventory {
        &self.cloud.inventory
    }
    pub(crate) fn cloud_inventory_mut(&mut self) -> &mut Inventory {
        self.cloud.inventory.revision = self.cloud.inventory.revision.wrapping_add(1);
        &mut self.cloud.inventory
    }
    pub(crate) fn cloud_inventory_profile_name(&self, id: ReadUuid) -> Option<&str> {
        self.account_profiles
            .iter()
            .find(|p| p.profile_id == id.0.to_string())
            .and_then(|p| p.name.as_deref())
            .filter(|name| !name.trim().is_empty())
    }
}

#[cfg(test)]
mod tests;
