// SPDX-License-Identifier: MPL-2.0

//! **The shared Profile deletion batch and its recovery**, as the HTTP
//! boundary carries them (root-catalog TECHNICAL.final §3.6, §3.7, §6.3).
//!
//! Distinct from [`super::profile_removal`], which is the **device-local**
//! removal attempt (`DELETE /v1/profiles/{id}/local` and the attempts
//! machinery): that hides a Profile on one installation and consents to
//! nothing global. The routes these bodies belong to write a **root fact**
//! that converges between a person's devices and is restored as a batch:
//!
//! | | |
//! | --- | --- |
//! | `GET /v1/profiles/{id}/deletion-preview` | [`ProfileDeletionPreview`] |
//! | `POST /v1/profiles/{id}/delete` | [`DeleteProfile`] → [`ProfileDeletionBatch`] |
//! | `GET /v1/profiles/deleted` | `Vec<`[`DeletedProfileListing`]`>` |
//! | `POST /v1/profiles/{id}/restore` | [`RestoreProfile`] → [`ProfileRestoreOutcome`] |
//! | `GET /v1/workspaces/unassigned` | `Vec<`[`UnassignedWorkspace`]`>` |
//!
//! The answer types are the Store's own (`koru_store_core::service`), named
//! here so the client and the server share one declaration.

pub use koru_store_core::ids::{BatchId, DeletionId};
pub use koru_store_core::service::{
    DeletedProfile, ProfileDeletionBatch, ProfileDeletionPreview, ProfileRestoreOutcome,
    UnassignedReason, UnassignedWorkspace,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `POST /v1/profiles/{id}/delete`: the preview the confirmation was shown
/// over, echoed back whole. A preview whose targets or revision no longer
/// match is refused with `idempotency_conflict` (HTTP 409): a materially
/// destructive confirmation taken over a changed list requires a refresh.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteProfile {
    pub preview: ProfileDeletionPreview,
}

/// `POST /v1/profiles/{id}/restore`: which deletion batch to restore — the
/// `deletion` from a [`DeletedProfileListing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RestoreProfile {
    pub deletion: DeletionId,
}

/// One restorable Profile deletion as `GET /v1/profiles/deleted` lists it:
/// the Store's record, plus the name of the retained Profile row so a Trash
/// surface can say which Profile it is without a second read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeletedProfileListing {
    /// The retained Profile row's name. `None` only when the row is gone,
    /// which the Store does not do while a deletion is restorable.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub deletion: DeletedProfile,
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_store_core::ids::{ProfileId, WorkspaceId};

    #[test]
    fn a_listing_flattens_the_record_beside_its_name() {
        let listing = DeletedProfileListing {
            name: Some("Work".into()),
            deletion: DeletedProfile {
                profile: ProfileId::new(),
                deletion: DeletionId::new(),
                batch_id: BatchId::new(),
                at_ms: 7,
                retention_ms: 9,
                targets: vec![WorkspaceId::new()],
                expired_or_purged: Vec::new(),
            },
        };
        let json = serde_json::to_value(&listing).unwrap();
        assert_eq!(json["name"], "Work");
        assert_eq!(json["at_ms"], 7);
        assert!(json.get("deletion").is_some(), "the record's own field");
        let back: DeletedProfileListing = serde_json::from_value(json).unwrap();
        assert_eq!(back, listing);
    }
}
