// SPDX-License-Identifier: MPL-2.0

//! Populate a new document before any content is exposed to another replica.

use koru_core::schema::{Cutting, Parent, WorkspaceDoc};
use koru_core::{Kind, NodeId, StoreError};

#[derive(Clone, Copy)]
pub(super) enum InitialContents<'a> {
    SeedOnOpen,
    Cutting {
        cutting: &'a Cutting,
        title: &'a str,
    },
}

/// Populate an empty, unpublished document from an authored document cutting.
///
/// This is the in-memory part of [`crate::Workspace::create_document_from_cutting_after`].
/// Existing content, including trash, is refused before any write. `plant`
/// preserves the subtree's content and remints node identities; the destination's
/// workspace/document stamps remain its own. A nonempty title renames the root.
///
/// The caller must discard the unpublished destination on error. Core edit
/// groups collect undo operations, not rollback, so a failed plant may have
/// written part of the subtree. The source cutting is never changed.
pub fn initialize_document_from_cutting(
    doc: &mut WorkspaceDoc,
    cutting: &Cutting,
    title: &str,
) -> Result<NodeId, StoreError> {
    if !doc.compatibility().writable() {
        return Err(StoreError::Malformed(
            "document initialization requires a writable document",
        ));
    }
    if !doc.walk_with_trash()?.is_empty() {
        return Err(StoreError::Malformed(
            "document initialization requires an empty document",
        ));
    }
    doc.stamp_schema_version()?;
    let root = doc.plant(cutting, Parent::Root, title)?;
    if doc.kind_of(root) != Some(Kind::Document) {
        return Err(StoreError::Malformed(
            "document initialization requires a document cutting",
        ));
    }
    Ok(root)
}

#[cfg(test)]
mod tests;
