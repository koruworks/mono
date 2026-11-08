// SPDX-License-Identifier: MPL-2.0

//! A snapshot that changed after selection must not replace the outgoing view.

use koru_store_core::{Code, Error, Manifest, Result};

pub(super) fn require_live_manifest(
    manifest: &Manifest,
    expected: koru_core::WorkspaceId,
) -> Result<()> {
    if manifest.workspace_id().map(|id| id.as_uuid()) != Some(expected.as_uuid())
        || manifest.version().is_empty()
    {
        return Err(Error::new(
            Code::CorruptState,
            "workspace manifest is not applied to the requested identity",
        ));
    }
    if manifest.archived() == Some(true) || manifest.workspace_deleted_at().is_some() {
        return Err(Error::new(
            Code::WorkspaceNotFound,
            "workspace is no longer a live navigation destination",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
