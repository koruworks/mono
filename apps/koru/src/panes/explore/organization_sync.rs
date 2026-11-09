// SPDX-License-Identifier: MPL-2.0

//! **What the sync icon and a Workspace row say about the organization's
//! sync** (root-catalog TECHNICAL.final §4.4, §7.2, §9; Plan 2.2), derived
//! from the Agent's [`RootSyncStatus`] push and nothing else.
//!
//! The Agent sends ids and states, per subject and per linked device. This
//! module turns them into one reading per Profile and per Workspace: across
//! devices the worse reading wins, and a Profile inherits the worst of its
//! own unit and of the Workspaces placed under it, because "up to date" is
//! never claimed while an eligible unit of the Profile's organization is
//! still unapplied somewhere. The words and the sentences behind them are
//! the pane's [`super::SyncStatus`] keys.
//!
//! Nothing here names a hidden subject: a `Held` reading is about the
//! Workspace this device asked a peer for, never about what the peer hides.

use std::collections::HashMap;

use koru_agent_wire::{RootSyncState, RootSyncStatus};
use koru_core::{ProfileId, WorkspaceId};

#[cfg(test)]
mod tests;

/// One reading, in the order a person should see them when several are true.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum OrganizationSync {
    UpToDate,
    Pending,
    Held,
    PausedBySettings,
    BlockedByMismatch,
}

impl OrganizationSync {
    fn from_wire(state: RootSyncState) -> Self {
        match state {
            RootSyncState::UpToDate => Self::UpToDate,
            RootSyncState::Pending => Self::Pending,
            RootSyncState::Held => Self::Held,
            RootSyncState::PausedBySettings => Self::PausedBySettings,
            RootSyncState::BlockedByMismatch => Self::BlockedByMismatch,
        }
    }

    /// A stop beats a wait, a wait beats done.
    pub(crate) fn worse(self, other: Self) -> Self {
        fn rank(reading: OrganizationSync) -> u8 {
            match reading {
                OrganizationSync::UpToDate => 0,
                OrganizationSync::Pending => 1,
                OrganizationSync::Held => 2,
                OrganizationSync::PausedBySettings => 3,
                OrganizationSync::BlockedByMismatch => 4,
            }
        }
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// The push, folded to one reading per subject across every linked device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OrganizationReadings {
    pub(crate) profiles: HashMap<ProfileId, OrganizationSync>,
    pub(crate) workspaces: HashMap<WorkspaceId, OrganizationSync>,
}

impl OrganizationReadings {
    pub(crate) fn from_status(status: &RootSyncStatus) -> Self {
        let mut out = Self::default();
        for subject in &status.subjects {
            let reading = OrganizationSync::from_wire(subject.state);
            if let Some(profile) = subject.profile_id.as_deref().and_then(ProfileId::parse) {
                out.profiles
                    .entry(profile)
                    .and_modify(|held| *held = held.worse(reading))
                    .or_insert(reading);
            }
            if let Some(workspace) = subject.workspace_id.as_deref().and_then(WorkspaceId::parse) {
                out.workspaces
                    .entry(workspace)
                    .and_modify(|held| *held = held.worse(reading))
                    .or_insert(reading);
            }
        }
        out
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.profiles.is_empty() && self.workspaces.is_empty()
    }

    /// One Workspace's reading, if any linked device said anything about it.
    pub(crate) fn workspace(&self, workspace: WorkspaceId) -> Option<OrganizationSync> {
        self.workspaces.get(&workspace).copied()
    }

    /// One Profile's reading: its own unit's, made worse by any of the
    /// Workspaces placed under it. `None` when no linked device said
    /// anything about the Profile or its Workspaces — the surfaces then say
    /// what they said before.
    pub(crate) fn profile(
        &self,
        profile: ProfileId,
        members: impl IntoIterator<Item = WorkspaceId>,
    ) -> Option<OrganizationSync> {
        let mut reading = self.profiles.get(&profile).copied();
        for member in members {
            if let Some(member) = self.workspaces.get(&member).copied() {
                reading = Some(reading.map_or(member, |held| held.worse(member)));
            }
        }
        reading
    }
}
