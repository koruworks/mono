// SPDX-License-Identifier: MPL-2.0

//! Pin policy and bounded repair belong to the window controller, so every
//! entry point counts the same adopted and still-pending choices.

use super::*;
use std::time::{Duration, Instant};

const REPAIR_AFTER: [u64; 3] = [1, 5, 30];

pub(super) struct PinRepair {
    pub pinned: bool,
    pub notice: Notice,
    started: Instant,
    attempts: usize,
    pub awaiting_catalog: bool,
}

impl PinRepair {
    pub fn has_automatic_attempt(&self) -> bool {
        !self.awaiting_catalog && self.attempts < REPAIR_AFTER.len()
    }
}

impl WorkspacesController {
    pub fn effective_pinned(&self, row: &Row) -> bool {
        self.pending_pin(row.id).unwrap_or(row.pinned)
    }

    pub fn pin_listing_pending(&self, workspace: WorkspaceId) -> bool {
        self.pin_repairs.contains_key(&workspace)
    }

    pub(super) fn pin_reason(&self, row: &Row) -> Option<String> {
        let key = if self.pending_pin(row.id).is_some() {
            if self.pin_listing_pending(row.id) {
                Key::WorkspacePinListingPending
            } else {
                Key::WorkspacePinSaving
            }
        } else if self.removals.contains_key(&row.owner) {
            Key::ProfileWaitRemoving
        } else if row.deleted {
            Key::WorkspacesTrash
        } else if row.archived {
            Key::WorkspaceArchivedUnavailable
        } else if let Some(reason) = row.authority_reason(self.lang) {
            return Some(reason);
        } else if !row.pinned
            && self
                .rows
                .iter()
                .filter(|candidate| {
                    candidate.owner == row.owner
                        && candidate.verified_live()
                        && self.effective_pinned(candidate)
                })
                .count()
                >= 3
        {
            Key::WorkspacePinLimit
        } else {
            return None;
        };
        Some(t(self.lang, key).into())
    }

    pub(super) fn pin_settled(
        &mut self,
        workspace: WorkspaceId,
        result: koru_store_core::errors::Result<koru_store_core::service::PinnedWorkspace>,
        now: Instant,
    ) {
        match result {
            Ok(result) if result.workspace.id == workspace => {
                let pending = matches!(
                    result.listing,
                    koru_store_core::service::ListingState::Pending { .. }
                );
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == workspace) {
                    row.pinned = result.workspace.pinned;
                    row.listing_differs = pending;
                }
                self.pin_repairs.insert(
                    workspace,
                    PinRepair {
                        pinned: result.workspace.pinned,
                        notice: Notice {
                            text: t(self.lang, Key::WorkspacePinListingPending).into(),
                            retry: Some(Retry::PinListing(workspace)),
                        },
                        started: now,
                        attempts: 0,
                        awaiting_catalog: !pending,
                    },
                );
                if pending {
                    self.show_pin_notice(workspace);
                }
            }
            _ => {
                self.accept_notice(Notice {
                    text: crate::REFUSAL_UNAVAILABLE.into(),
                    retry: None,
                });
                self.notice_owner = self.row(workspace).map(|row| row.owner);
            }
        }
        self.links.fetch_catalog();
    }

    pub(super) fn poll_pin_repairs_at(&mut self, now: Instant) -> bool {
        let due: Vec<_> = self.pin_repairs.iter().filter_map(|(workspace, repair)| {
            (repair.has_automatic_attempt()
                && now.saturating_duration_since(repair.started) >= Duration::from_secs(REPAIR_AFTER[repair.attempts])
                && !self.pending.values().any(|pending| matches!(pending, Pending::ReconcileListing(id) if id == workspace)))
                .then_some(*workspace)
        }).collect();
        for workspace in &due {
            self.pin_repairs.get_mut(workspace).unwrap().attempts += 1;
            self.send_pin_repair(*workspace);
        }
        !due.is_empty()
    }

    fn send_pin_repair(&mut self, workspace: WorkspaceId) {
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::ReconcileListing(id) if *id == workspace))
        {
            return;
        }
        let command = self.start_command(Pending::ReconcileListing(workspace));
        self.links.reconcile_listing(command, workspace);
    }

    pub(super) fn retry_pin_listing(&mut self, workspace: WorkspaceId) {
        if self.pin_repairs.contains_key(&workspace) {
            self.send_pin_repair(workspace);
            self.show_pin_notice(workspace);
        }
    }

    pub(super) fn pin_repair_settled(&mut self, workspace: WorkspaceId, answer: Answer) {
        if let Some(repair) = self.pin_repairs.get_mut(&workspace) {
            if let Answer::ListingReconciled(Ok(row)) = answer {
                if row.id == workspace {
                    repair.pinned = row.pinned;
                    repair.awaiting_catalog = true;
                }
            }
            self.show_pin_notice(workspace);
        }
        // Even a refused repair may have met a remote lifecycle change.
        self.links.fetch_catalog();
    }

    fn show_pin_notice(&mut self, workspace: WorkspaceId) {
        if self.notice.as_ref().is_some_and(|notice| {
            matches!(
                notice.retry,
                Some(Retry::Lifecycle(..) | Retry::Migration(..))
            )
        }) {
            return;
        }
        self.notice = Some(Notice {
            text: t(self.lang, Key::WorkspacePinListingPending).into(),
            retry: Some(Retry::PinListing(workspace)),
        });
        self.notice_owner = self.row(workspace).map(|row| row.owner);
    }

    pub(super) fn adopt_pin_repairs(&mut self) {
        self.pin_repairs.retain(|id, _repair| {
            self.rows
                .iter()
                .find(|row| row.id == *id)
                .is_some_and(|row| {
                    row.authority != crate::Authority::Verified || row.listing_differs
                })
        });
        if self.notice.as_ref().is_some_and(|notice| matches!(notice.retry, Some(Retry::PinListing(id)) if !self.pin_repairs.contains_key(&id))) {
            self.notice = None;
            self.notice_owner = None;
            if let Some(id) = self.pin_repairs.keys().next().copied() { self.show_pin_notice(id); }
        }
    }
}

#[cfg(test)]
mod tests;
