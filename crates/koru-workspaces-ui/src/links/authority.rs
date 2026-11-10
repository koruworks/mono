// SPDX-License-Identifier: MPL-2.0

//! Catalog invalidation refreshes durable lifecycle as well as presentation.

use super::*;
use crate::Authority;
use crate::state::{DeletedProfileRow, Recovery, UnassignedRow};
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
    let recovery = recovery_answer(client);
    // An unassigned workspace whose owner no listing names still needs a
    // row: *Move to…* reads its destinations off the row. The catalog's
    // including-deleted read lists it under its former owner, so this is
    // the fallback for a catalog that did not.
    for unassigned in &recovery.unassigned {
        if !rows.iter().any(|row| row.id == unassigned.id)
            && let Some(workspace) = catalog.iter().find(|row| row.id == unassigned.id)
        {
            rows.push(row_of(workspace.clone()));
        }
    }
    Answer::CatalogState {
        rows,
        removals,
        recovery,
    }
}

/// The recovery lists, read in the same pass as the rows. **A failed read
/// is an empty list, not an unavailable catalog**: the rows on screen are
/// still true, and a Store that answers the catalog but not these two
/// routes shows no recovery group rather than a dimmed manager.
fn recovery_answer(client: &Client) -> Recovery {
    let unassigned = client
        .store()
        .unassigned_workspaces()
        .map(|rows| {
            rows.into_iter()
                .map(|row| UnassignedRow {
                    id: row.workspace.id,
                    name: row.workspace.name,
                    icon: row.workspace.icon,
                    color: row.workspace.color,
                    former_profile: row.former_profile,
                    reason: row.reason,
                })
                .collect()
        })
        .unwrap_or_default();
    let deleted_profiles = client
        .store()
        .profile_deletions()
        .map(|rows| {
            rows.into_iter()
                .map(|row| DeletedProfileRow {
                    profile: row.deletion.profile,
                    name: row.name,
                    deletion: row.deletion.deletion,
                    at_ms: row.deletion.at_ms,
                    retention_ms: row.deletion.retention_ms,
                    targets: row.deletion.targets.len(),
                    expired_or_purged: row.deletion.expired_or_purged.len(),
                })
                .collect()
        })
        .unwrap_or_default();
    Recovery {
        unassigned,
        deleted_profiles,
    }
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
