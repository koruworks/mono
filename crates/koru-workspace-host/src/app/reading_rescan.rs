// SPDX-License-Identifier: MPL-2.0

//! Workspace re-reading uses the same Store offers and requests as Read again.
//! Listing, snapshot resolution and requests all stay off the event loop.

use super::{App, Host, ImportMessage, Wake, client_at_segment, workspace_file_hash};
use koru_store_core::model::{BlobHash, DocumentEntry};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Summary {
    pub queued: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// One request per distinct blob. An unavailable item never stops later files.
fn enqueue(
    entries: &[DocumentEntry],
    mut hash: impl FnMut(&DocumentEntry) -> Result<BlobHash, ()>,
    mut request: impl FnMut(&BlobHash) -> Result<bool, ()>,
) -> Summary {
    let by_id: HashMap<_, _> = entries.iter().map(|entry| (entry.id, entry)).collect();
    let mut seen = HashSet::new();
    let mut summary = Summary::default();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == koru_core::explore::KIND_FILE)
    {
        // Match the live Files tree, including a file beneath a trashed parent.
        let mut at = Some(entry.id);
        let mut ancestors = HashSet::new();
        let mut live = true;
        while let Some(id) = at {
            let Some(ancestor) = by_id.get(&id) else {
                break;
            };
            if !ancestors.insert(id) || ancestor.trashed_at.is_some() {
                live = false;
                break;
            }
            at = ancestor.parent;
        }
        if !live {
            continue;
        }
        match hash(entry) {
            Ok(hash) if seen.insert(hash.clone()) => match request(&hash) {
                Ok(true) => summary.queued += 1,
                Ok(false) => summary.skipped += 1,
                Err(()) => summary.failed += 1,
            },
            Ok(_) => summary.skipped += 1,
            Err(()) => summary.failed += 1,
        }
    }
    summary
}

impl<H: Host> App<H> {
    pub(super) fn rescan_workspace_files(&mut self, workspace: koru_core::WorkspaceId) {
        let workspace = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let spawned = std::thread::Builder::new()
            .name("koru-reading-rescan".into())
            .spawn(move || {
                let result = (|| {
                    let client = client_at_segment(&segment, root.as_deref()).map_err(|_| ())?;
                    let entries = client.documents(workspace).map_err(|_| ())?;
                    Ok(enqueue(
                        &entries,
                        |entry| workspace_file_hash(&client, workspace, entry.id).map_err(|_| ()),
                        |hash| {
                            let reading = client.reading(workspace, hash).map_err(|_| ())?;
                            if !reading.is_some_and(|reading| reading.offers.read_again) {
                                return Ok(false);
                            }
                            client
                                .read_again(workspace, hash)
                                .map(|_| true)
                                .map_err(|_| ())
                        },
                    ))
                })();
                let _ = tx.send(ImportMessage::ReadingRescanFinished(result));
                let _ = proxy.send_event(Wake::Import);
            });
        let key = if spawned.is_ok() {
            koru_i18n::Key::FilesRescanStarted
        } else {
            koru_i18n::Key::FilesRescanFailed
        };
        self.notice(koru_i18n::t(crate::accessibility::ui_lang(), key).to_owned());
    }
}

#[cfg(test)]
mod tests;
