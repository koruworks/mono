// SPDX-License-Identifier: MPL-2.0

//! **Which profile a chat belongs to** (AI schema 13, koru-ai D120).
//!
//! A chat started after schema 13 is given its window's profile when it is
//! created, and a branch its source's. A chat written before has no profile
//! in the store, and `ai.db` cannot name one by itself: it holds workspace
//! and document ids, and only the Store knows which profile owns them. So
//! the column is nullable and the AI service places each such chat itself,
//! through the Store port it already reads documents with, as soon as the
//! Store answers — [`place_chat`] is the rule it applies.
//!
//! **The rule.** The profile that owns the workspace the chat last read —
//! the newest of the workspaces its tools completed a call in and the
//! documents it read as context; failing that, the workspace it was most
//! recently granted; failing that, the first profile in the order Koru lists
//! them. A workspace whose profile is gone counts for nothing. The only time
//! there is no answer is when the Store has no profile at all, and then the
//! chat waits, unlisted, for the first one.

use std::collections::HashMap;

use crate::{ChatId, DocumentId, ProfileId, Timestamp, WorkspaceId};

/// Koru's profiles, and the profile that owns each workspace, as the Store
/// lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileCatalog {
    /// The live profiles in the order Koru lists them — the Store's, oldest
    /// first. The first is where a chat with no evidence goes.
    pub profiles: Vec<ProfileId>,
    /// The profile that owns each workspace the Store knows.
    pub workspace_owners: HashMap<WorkspaceId, ProfileId>,
}

impl ProfileCatalog {
    /// The live profile that owns `workspace`; `None` for a workspace the
    /// Store does not know or whose profile was removed.
    pub fn owner(&self, workspace: WorkspaceId) -> Option<ProfileId> {
        self.workspace_owners
            .get(&workspace)
            .copied()
            .filter(|owner| self.profiles.contains(owner))
    }

    /// The first profile in Koru's order.
    pub fn first(&self) -> Option<ProfileId> {
        self.profiles.first().copied()
    }
}

/// Something a chat read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChatRead {
    /// A workspace one of its tools completed a call in.
    Workspace(WorkspaceId),
    /// A document it read as context: pinned, or an `@` chip.
    Document(DocumentId),
}

/// What the AI store holds about where a chat with no profile was working.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatPlacementEvidence {
    pub chat_id: ChatId,
    /// What it read, newest first.
    pub reads: Vec<(Timestamp, ChatRead)>,
    /// The workspaces it was granted, most recently granted first.
    pub grants: Vec<WorkspaceId>,
}

/// **The profile a chat with no profile goes to** — the rule in the module
/// documentation. `document_workspace` answers which workspace holds a
/// document, `None` for one the Store no longer has. `None` only when the
/// catalog has no profile.
pub fn place_chat(
    evidence: &ChatPlacementEvidence,
    catalog: &ProfileCatalog,
    mut document_workspace: impl FnMut(DocumentId) -> Option<WorkspaceId>,
) -> Option<ProfileId> {
    let mut reads = evidence.reads.clone();
    // Newest first, whatever order they came in; a tie keeps theirs.
    reads.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    reads
        .iter()
        .find_map(|(_, read)| {
            let workspace = match *read {
                ChatRead::Workspace(workspace) => Some(workspace),
                ChatRead::Document(document) => document_workspace(document),
            }?;
            catalog.owner(workspace)
        })
        .or_else(|| {
            evidence
                .grants
                .iter()
                .find_map(|workspace| catalog.owner(*workspace))
        })
        .or_else(|| catalog.first())
}

#[cfg(test)]
mod tests;
