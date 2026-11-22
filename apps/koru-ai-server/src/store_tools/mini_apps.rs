// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::mini_apps::{MiniAppRegistry, Registry, unavailable};
impl MiniAppRegistry for StoreToolAdapter {
    fn read(
        &self,
        context: &AiToolExecutionContext,
        lists: bool,
        documents: bool,
    ) -> Result<Registry, AiError> {
        let workspace = context
            .mini_app
            .as_ref()
            .ok_or_else(unavailable)?
            .host
            .workspace_id;
        if !self.readable_workspaces(context)?.contains(&workspace) {
            return Err(unavailable());
        }
        let (entries, snapshots) = self
            .store
            .read(move |store| {
                if store.workspace(workspace)?.deleted_at.is_some() {
                    return Ok(None);
                }
                let entries = if lists || documents {
                    store.documents(workspace)?
                } else {
                    Vec::new()
                };
                let mut ids: Vec<_> = entries
                    .iter()
                    .filter(|entry| {
                        lists
                            && entry.kind == koru_core::explore::KIND_SYSTEM
                            && entry.trashed_at.is_none()
                    })
                    .map(|entry| entry.id)
                    .collect();
                ids.sort();
                let mut snapshots = Vec::new();
                let mut total = 0usize;
                for id in ids {
                    let snapshot = store.snapshot(workspace, id)?;
                    total = total.saturating_add(snapshot.len());
                    if total > 16 * 1024 * 1024 {
                        return Ok(None);
                    }
                    snapshots.push(snapshot);
                }
                Ok(Some((entries, snapshots)))
            })
            .map_err(tool_failure)?
            .ok_or_else(unavailable)?;
        let mut system = None;
        for snapshot in &snapshots {
            if let Some(system) = &mut system {
                koru_core::system::SystemDoc::absorb(system, snapshot)
                    .map_err(|_| unavailable())?;
            } else {
                system = Some(
                    koru_core::system::SystemDoc::from_snapshot(snapshot)
                        .map_err(|_| unavailable())?,
                );
            }
        }
        Ok(Registry {
            lists: system
                .map(|s| {
                    s.lists()
                        .into_iter()
                        .map(|l| (l.id.as_uuid(), l.name))
                        .collect()
                })
                .unwrap_or_default(),
            documents: entries
                .into_iter()
                .filter(|entry| {
                    documents
                        && entry.kind == koru_core::explore::KIND_DOCUMENT
                        && entry.trashed_at.is_none()
                })
                .map(|entry| entry.id)
                .collect(),
        })
    }
}
