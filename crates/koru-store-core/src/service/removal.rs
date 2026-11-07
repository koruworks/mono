// SPDX-License-Identifier: MPL-2.0

//! Persisted Profile removal attempts. Workspace steps run under the same
//! Store lock as these transitions; stale workers cannot resume after Keep.

use super::*;
use crate::ids::RemovalAttemptId;
use crate::profile_removal::{RemovalKind, RemovalMark, RemovalUpdate};

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Preflight before disconnecting rooms. The mutation checks again under
    /// this same Store lock, before it flushes or changes any workspace bytes.
    pub fn validate_workspace_removal(
        &self,
        workspace: WorkspaceId,
        attempt: Option<RemovalAttemptId>,
    ) -> Result<()> {
        let owner = self.workspace_including_deleted(workspace)?.owner;
        match attempt {
            Some(attempt) => {
                let mark = self.profile_removal(owner)?;
                crate::profile_removal::require_attempt(mark.as_ref(), attempt)?;
                Ok(())
            }
            None => self.refuse_profile_removal(owner),
        }
    }

    pub fn begin_profile_removal(
        &mut self,
        id: ProfileId,
        kind: RemovalKind,
        at: Timestamp,
    ) -> Result<RemovalMark> {
        self.refuse_holder(id, "removed")?;
        self.profile(id)?;
        self.catalog
            .update_profile_removal(id, RemovalUpdate::Begin { kind, at })?
            .ok_or_else(|| Error::bare(Code::Internal))
    }

    pub fn stop_profile_removal(&mut self, id: ProfileId, attempt: RemovalAttemptId) -> Result<()> {
        self.catalog
            .update_profile_removal(id, RemovalUpdate::Stop { attempt })?;
        Ok(())
    }

    pub fn keep_profile(&mut self, id: ProfileId, attempt: RemovalAttemptId) -> Result<()> {
        self.catalog
            .update_profile_removal(id, RemovalUpdate::Keep { attempt })?;
        Ok(())
    }

    pub(super) fn removal_step(
        &mut self,
        id: ProfileId,
        attempt: RemovalAttemptId,
        at: Timestamp,
    ) -> Result<()> {
        self.catalog
            .update_profile_removal(id, RemovalUpdate::Step { attempt, at })?;
        Ok(())
    }

    pub(super) fn refuse_profile_removal(&self, id: ProfileId) -> Result<()> {
        if self.profile_removal(id)?.is_some() {
            return Err(Error::bare(Code::ProfileBeingRemoved));
        }
        Ok(())
    }
}
