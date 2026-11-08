// SPDX-License-Identifier: MPL-2.0

//! **Which documents are waiting on a file Koru Cloud does not hold yet**, put
//! into what a person reads: each document's title and the devices it is
//! waiting for.
//!
//! One function for every surface that asks. The desktop's
//! `GET /v1/workspaces/{id}/cloud-sync/waiting` route and the phone's
//! in-process `store.cloud_waiting` both call [`documents`], so the two cannot
//! name a holder, sort a list or spell an untitled document two ways.
//!
//! Four inputs, none of them new: the blob engine's grouping (its
//! `object_missing` parks and measured missing coverage mapped through the
//! canonical walk, handed over as [`WaitingFiles`]); the manifest listing, which is where every other
//! listing reads a title; and two records of *who holds these bytes* — the
//! Store's durable origin (the device that first held them) and the peer
//! runtime's in-memory directory of the devices that advertised them this
//! session. Both are device ids, merged per blob and turned into names
//! through the authorization snapshot that already carries every peer's
//! display name.
//!
//! The origin alone named only the device that *created* a file, so a phone
//! waited on the laptop that took a picture while the desktop beside it held
//! every one of them. The directory is what names the desktop.
//!
//! **A holder is named only when it is known.** A device this device does
//! not recognise, this device itself, or a blob nobody recorded an origin or
//! an advertisement for contributes no name, and a document whose files have
//! none says so with an empty list rather than a guess.
//!
//! Read-only: nothing here clears a park or starts a pass.

use std::collections::HashMap;

use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::BlobHash;
use koru_store_wire::CloudWaitingDocument;

use crate::state::{KoruStore, WaitingFiles};

/// Every blob the waiting documents lack, each once — what a caller asks the
/// peer runtime's directory about before calling [`documents`].
pub fn hashes(waiting: &[WaitingFiles]) -> Vec<BlobHash> {
    let mut hashes: Vec<BlobHash> = Vec::new();
    for hash in waiting.iter().flat_map(|entry| &entry.blobs) {
        if !hashes.contains(hash) {
            hashes.push(hash.clone());
        }
    }
    hashes
}

/// The waiting documents, most files first, then by title, then by id.
///
/// `advertised` is, per blob, the storage ids of the devices the peer runtime
/// saw advertise it this session; empty where no runtime is running. It is
/// merged with each blob's durable origin, so a device is named whether it
/// created the file or only holds a copy, and named once either way.
///
/// `authorization` is the snapshot the device's control plane installed, or
/// `None` before one has arrived — which names nobody rather than guessing.
pub fn documents(
    store: &mut KoruStore,
    workspace: WorkspaceId,
    waiting: Vec<WaitingFiles>,
    advertised: &HashMap<BlobHash, Vec<String>>,
    authorization: Option<&koru_sync_control::AuthorizationSnapshot>,
) -> koru_store_core::Result<Vec<CloudWaitingDocument>> {
    let mut documents = Vec::with_capacity(waiting.len());
    if !waiting.is_empty() {
        let (local, names) = peer_names(authorization);
        let titles: HashMap<_, String> = store
            .documents(workspace)?
            .into_iter()
            .map(|entry| (entry.id, entry.title))
            .collect();
        for entry in waiting {
            let mut devices: Vec<String> = Vec::new();
            for hash in &entry.blobs {
                devices.extend(store.blob_origin(workspace, hash).ok().flatten());
                devices.extend(advertised.get(hash).into_iter().flatten().cloned());
            }
            let mut holders: Vec<String> = devices
                .into_iter()
                .filter(|device| Some(device) != local.as_ref())
                .filter_map(|device| names.get(&device).cloned())
                .collect();
            holders.sort();
            holders.dedup();
            documents.push(CloudWaitingDocument {
                document: entry.document,
                title: titles.get(&entry.document).cloned().unwrap_or_default(),
                missing_files: u32::try_from(entry.blobs.len()).unwrap_or(u32::MAX),
                holders,
            });
        }
    }
    documents.sort_by(|a, b| {
        b.missing_files
            .cmp(&a.missing_files)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.document.to_storage().cmp(&b.document.to_storage()))
    });
    Ok(documents)
}

/// This device's own id, and every authorized peer's display name, both in
/// storage form. Empty without a snapshot, which names nobody.
fn peer_names(
    snapshot: Option<&koru_sync_control::AuthorizationSnapshot>,
) -> (Option<String>, HashMap<String, String>) {
    let Some(snapshot) = snapshot else {
        return (None, HashMap::new());
    };
    let names = snapshot
        .peers
        .iter()
        .filter_map(|peer| {
            let name = peer.display_name.as_deref()?.trim();
            (!name.is_empty()).then(|| (peer.device_id.to_storage(), name.to_owned()))
        })
        .collect();
    (Some(snapshot.local_device.to_storage()), names)
}
