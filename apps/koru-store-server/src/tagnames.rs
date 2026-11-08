// SPDX-License-Identifier: MPL-2.0

//! Where the search index gets a tag's name.
//!
//! The index resolves a `koru.tag` attribute's `TagId` to the word somebody
//! typed, because an id is searchable as a field and useless as a word
//! (ADR-182). The table lives in the workspace's **system document**, which is
//! a `LoroDoc` in root containers the projection deliberately does not walk —
//! so it takes the store's lock and the node model, and `koru-search` has
//! neither and must keep neither. This is the half that knows both, the same
//! shape `search.rs`'s `Port` already is.
//!
//! **The same door as `attributes.rs`**, and now literally the same code:
//! finding a workspace's system documents through the projection's listing and
//! opening them under the Store lock is [`crate::systemdoc`]. What is left here
//! is the one thing that was ever this file's — which question to ask the
//! document once it is open.

use std::sync::{Arc, Mutex, Weak};

use koru_projection::Projections;
use koru_search::TagNames;
use koru_store_api::state::AppState;
use koru_store_core::ids::WorkspaceId;

use crate::systemdoc;

pub struct TagSource {
    /// Its own read-only connection, per the `read_ports` note: WAL lets it
    /// answer without the projector's mutex.
    projections: Mutex<Projections>,
    /// `Weak` for the same reason the attribute port's is: the daemon holds the
    /// strong reference for the life of the process, so a failed upgrade means
    /// shutdown and this degrades rather than fails.
    state: Weak<AppState>,
}

/// Build the source, or `None` — in which case the index carries on writing the
/// ids it always wrote, which is a stale word rather than a lost document.
pub fn tag_source(
    derived: &crate::derived::Derived,
    state: &Arc<AppState>,
) -> Option<Box<dyn TagNames>> {
    match derived.projections() {
        Ok(projections) => Some(Box::new(TagSource {
            projections: Mutex::new(projections),
            state: Arc::downgrade(state),
        })),
        Err(error) => {
            tracing::warn!("search will index tag ids rather than names: {error}");
            None
        }
    }
}

impl TagNames for TagSource {
    fn tag_names(&self, workspace: WorkspaceId) -> Option<Vec<(String, String)>> {
        let ids = {
            let projections = self.projections.lock().ok()?;
            let ids = systemdoc::system_documents(&projections, workspace);
            if ids.is_empty() {
                // Not "this workspace has no tags": the listing has not been
                // mirrored yet, and the indexer reads `None` as *not now*.
                return None;
            }
            ids
        };
        let system = systemdoc::open_system_document(&self.state.upgrade()?, workspace, &ids)?;
        // Alias tombstones included and already followed — a node still
        // carrying a merged-away id comes out spelled like the tag it now
        // belongs to. `SystemDoc::tag_names` is where that walk lives, beside
        // `resolve`.
        Some(
            system
                .tag_names()
                .into_iter()
                .map(|(id, name)| (id.to_storage(), name))
                .collect(),
        )
    }
}
