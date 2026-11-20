// SPDX-License-Identifier: MPL-2.0

//! **Deleting a run**: into the trash, back out of it, and the empty.
//!
//! Three verbs, and the split between them and the window is the whole
//! shape of the feature.
//!
//! A run *is* a Run document in the workspace's manifest, so putting one in
//! the trash is that document's move into the workspace's bin — reversible,
//! synced, and exactly what happens to every other document in Koru
//! (ADR-69). That move is a manifest edit and the manifest belongs to the
//! window. What only this worker can do is the rest: refuse a run that is
//! still working, remember what the person said about its results, and — on
//! the empty — remove everything device-local that the run left behind.
//!
//! So each verb here answers with the document, and the window moves it.
//!
//! **The empty is ordered so nothing is half-deleted.** The working folder
//! goes first, because it is the only step that can be refused by something
//! outside Koru; a refusal changes nothing else and the run stays in the
//! trash, where it can be deleted again. Then the results, if the person
//! asked for them to go. Then the authority record, then the ledger's rows,
//! then — back in the window — the document.
//!
//! **And the folder is removed only when Koru made it.**
//! [`koru_delegate_scope::remove_working_folder`] is the one place that is
//! decided, from the scope the ledger recorded at creation. A run scoped to
//! a project root ran *inside the person's own repository*; the run is
//! forgotten and that directory is left exactly alone, and
//! [`koru_delegate_core::FolderOutcome::Kept`] is what the trash tells them
//! it did rather than saying nothing.

use koru_delegate_core::{
    ArtifactDisposition, DocumentId, FolderOutcome, RunHeader, RunId, WorkspaceId,
};
use koru_delegate_scope::{Removal, remove_working_folder};

use super::{Orchestrator, RunError};

/// What the empty did, per run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Purged {
    pub run: RunId,
    pub document: DocumentId,
    pub folder: FolderOutcome,
    pub artifacts_retired: u32,
}

/// A run and where it is registered: the document the window moves, and
/// the workspace the families it created live in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunPlace {
    pub header: RunHeader,
    pub document: DocumentId,
    pub workspace: WorkspaceId,
}

impl Orchestrator {
    /// **Into the trash.**
    ///
    /// Refuses a run that is still working with `wrong_status`: trashing is
    /// meant to be reversible, and a run still writing into a folder is the
    /// one case where it is not — the agent keeps going, keeps writing, and
    /// keeps registering results into a run the person believes they put
    /// away. Cancel it first, which is a verb they already have.
    ///
    /// Records what to do with the results and answers with the document.
    /// Nothing on disk and nothing in the artifact store changes here.
    pub fn trash(
        &mut self,
        run: RunId,
        artifacts: ArtifactDisposition,
    ) -> Result<RunPlace, RunError> {
        let status = self.status(run)?;
        if status.may_be_writing() {
            return Err(RunError::WrongStatus {
                status: status.name(),
            });
        }
        self.ports.store.record_deletion(run, artifacts)?;
        self.place(run)
    }

    /// **Back out of the trash, whole.** Nothing was deleted, so there is
    /// nothing to put back: the decision about the results is forgotten and
    /// the document is the caller's to move out of the bin.
    pub fn restore(&mut self, run: RunId) -> Result<RunPlace, RunError> {
        let place = self.place(run)?;
        self.ports.store.clear_deletion(run)?;
        Ok(place)
    }

    /// **The empty.** Everything this device holds about the run, in the
    /// order the module docs give.
    pub fn purge(&mut self, run: RunId) -> Result<Purged, RunError> {
        let place = self.place(run)?;
        // A run that is still working never reached the trash, and a
        // restart could not have started it there either — but the empty is
        // the irreversible half, so it asks again rather than trusting that
        // the trash asked.
        let status = self.status(run)?;
        if status.may_be_writing() {
            return Err(RunError::WrongStatus {
                status: status.name(),
            });
        }

        // **The folder first**, because it is the only step something
        // outside Koru can refuse. The scope is the creation intent's, as
        // recorded; the path is the authority record's, as resolved when
        // the run started. Neither is re-derived: a base a person has since
        // changed or a project that has moved would name a different
        // directory, and that is how a delete removes the wrong thing.
        let scope = self
            .ports
            .store
            .intent_facts(run)?
            .map(|facts| facts.scope)
            .unwrap_or(koru_delegate_core::WorkspaceScope::None);
        let folder = self.ports.mcp.run_workspace(run)?;
        let removal = remove_working_folder(&scope, folder.as_deref());
        let folder = match removal {
            Removal::Removed => FolderOutcome::Removed,
            Removal::Kept => FolderOutcome::Kept,
            Removal::Absent => FolderOutcome::Absent,
            Removal::Refused(kind) => return Err(RunError::FolderNotRemoved { kind }),
        };

        let artifacts_retired = match self.disposition(run)? {
            ArtifactDisposition::Delete => self
                .ports
                .registrations
                .retire_results(run, place.workspace)?,
            ArtifactDisposition::Keep => 0,
        };

        self.ports.mcp.forget(run)?;
        self.ports.store.forget_run(run)?;
        // The run is no longer this orchestrator's to hold open.
        self.runs.remove(&run);
        Ok(Purged {
            run,
            document: place.document,
            folder,
            artifacts_retired,
        })
    }

    /// What the person decided about this run's results, or the safe
    /// default for a run nobody was asked about.
    fn disposition(&self, run: RunId) -> Result<ArtifactDisposition, RunError> {
        Ok(self.deletion_recorded(run).unwrap_or_default())
    }

    /// **What was recorded about this run's results, if anything was.**
    /// `None` is a run nobody has been asked about — one that produced
    /// nothing, or one that has never been in the trash — and is the reason
    /// [`Self::disposition`] falls back to `Keep` rather than to nothing.
    pub fn deletion_recorded(&self, run: RunId) -> Option<ArtifactDisposition> {
        self.ports
            .store
            .deletion(run)
            .ok()
            .flatten()
            .map(|record| record.artifacts)
    }

    /// The run's header and where it is registered.
    fn place(&mut self, run: RunId) -> Result<RunPlace, RunError> {
        let state = self.open(run)?;
        let (document, workspace) = (state.document, state.workspace);
        Ok(RunPlace {
            header: self.header(run)?,
            document,
            workspace,
        })
    }
}
