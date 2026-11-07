// SPDX-License-Identifier: MPL-2.0

//! Catalog invalidation refreshes durable lifecycle as well as presentation.

use super::*;
use crate::Authority;
use koru_store_core::service::{WorkspaceAuthority, WorkspaceAuthorityState};

pub(super) fn catalog_answer(client: &Client) -> Answer {
    let Ok(catalog) = client.catalog().workspaces() else {
        return Answer::Unavailable;
    };
    let Ok(profiles) = client.store().profiles() else {
        return Answer::Unavailable;
    };
    let owners: std::collections::BTreeSet<_> = catalog
        .iter()
        .map(|row| row.owner)
        .chain(profiles.iter().map(|row| row.id))
        .collect();
    let mut removals = std::collections::BTreeMap::new();
    let mut rows = Vec::new();
    for owner in owners {
        match client.store().profile_removal(owner) {
            Ok(Some(mark)) => {
                removals.insert(owner, mark);
            }
            Ok(None) => {}
            Err(_) => return Answer::Unavailable,
        }
        match client.store().workspace_authority(owner) {
            Ok(authority) => rows.extend(
                authority
                    .into_iter()
                    .filter(|row| row.workspace.owner == owner)
                    .map(overlay),
            ),
            Err(error) => rows.extend(
                catalog
                    .iter()
                    .filter(|row| row.owner == owner)
                    .cloned()
                    .map(|workspace| {
                        let mut row = row_of(workspace);
                        row.authority = Authority::Uncertain {
                            cause: error.code().as_str().into(),
                            recovery: None,
                        };
                        row
                    }),
            ),
        }
    }
    rows.sort_by_key(|row| (row.owner, row.position, row.id));
    Answer::CatalogState { rows, removals }
}

fn overlay(value: WorkspaceAuthority) -> Row {
    let mut row = row_of(value.workspace);
    row.listing_differs = value.listing_differs;
    match value.authority {
        WorkspaceAuthorityState::Read {
            archived,
            deleted_at,
            pinned,
            applied: true,
        } => {
            row.archived = archived;
            row.deleted = deleted_at.is_some();
            row.pinned = pinned;
            row.authority = Authority::Verified;
        }
        WorkspaceAuthorityState::Read { applied: false, .. } => row.authority = Authority::Pending,
        WorkspaceAuthorityState::Unreadable { cause, recovery } => {
            row.authority = Authority::Uncertain { cause, recovery }
        }
    }
    row
}

#[cfg(test)]
mod tests;
