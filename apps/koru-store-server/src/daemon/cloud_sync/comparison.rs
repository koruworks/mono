// SPDX-License-Identifier: MPL-2.0

//! Cloud uses the peer inventory's durable revision leaves. Keep this seam:
//! a manifest describes membership, NOT the content versions of its children.
//! Never replace this with `manifest unchanged => workspace synced`, nor with
//! opening every document on a heartbeat. Read positions from the journal index,
//! compare bounded batches with the worker, and reconcile only differing units.
use super::*;
use koru_replication::protocol::{CLOUD_COMPARE_LIMIT, CloudDocumentDigest};
use koru_store_core::sync_digest;

#[allow(clippy::too_many_arguments)]
pub(super) async fn documents<'a, R, W, J, F>(
    sync: &mut Reconciler<'a, R, W>,
    workspace: WorkspaceId,
    manifest: DocumentId,
    entries: Vec<DocumentId>,
    worker: &J,
    authorize: &Authorize,
    deferrals: &DeferralLedger,
    versions: &Versions,
    outcome: &mut Outcome,
    progress: &mut F,
    rooms: &mut Option<rooms::Rooms<J>>,
) -> Result<(), Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    J: StoreJobs,
    F: FnMut(Progress),
{
    let gate = authorize.clone();
    let positions: HashMap<_, _> = store_result(worker.submit_store(move |store| {
        gate(Unit::Document {
            workspace_id: cloud(workspace.as_uuid()),
            document_id: cloud(manifest.as_uuid()),
        })
        .map_err(Error::Authority)?;
        store
            .document_positions(workspace)
            .map_err(|_| Error::Unavailable)
    }))
    .await??
    .into_iter()
    .collect();
    let entries = entries
        .into_iter()
        .filter(|id| *id != manifest)
        .collect::<Vec<_>>();
    for page in entries.chunks(CLOUD_COMPARE_LIMIT) {
        if let Some(rooms) = rooms {
            outcome.remote_hint |= rooms.drain(sync, deferrals, versions, progress).await?;
        }
        let request = page
            .iter()
            .filter_map(|id| {
                positions.get(id).map(|version| CloudDocumentDigest {
                    document_id: *id,
                    digest: sync_digest::document(workspace, *id, sync_digest::revision(version)),
                })
            })
            .collect::<Vec<_>>();
        let matching = if request.is_empty() {
            std::collections::HashSet::new()
        } else {
            let (matching, hinted) = sync.compare_documents(request).await?;
            outcome.remote_hint |= hinted;
            matching
        };
        tracing::debug!(
            compared = page.len(),
            unchanged = matching.len(),
            "cloud sync: compared durable document revisions"
        );
        for document in page {
            let unit = Unit::Document {
                workspace_id: cloud(workspace.as_uuid()),
                document_id: cloud(document.as_uuid()),
            };
            authorize(unit).map_err(Error::Authority)?;
            if matching.contains(document) {
                // This is an equality proof for the sampled durable revision,
                // never a fabricated transfer Ack. Later edits still invalidate it.
                outcome.documents_synced = outcome.documents_synced.saturating_add(1);
                outcome.settled.push(unit);
                outcome.documents.push(DocumentPass {
                    unit,
                    state: positions[document].clone(),
                    carried: true,
                });
                ledger(deferrals, |held| held.cleared(unit));
                progress(Progress::Documents {
                    synced: outcome.documents_synced,
                    required: None,
                });
                // Cloud's durable revision equals the sampled one.
                progress(Progress::Confirmed {
                    unit,
                    version: positions[document].clone(),
                });
                progress(Progress::Settled {
                    unit,
                    upload_deferred: false,
                    download_deferred: false,
                });
            } else {
                if let Some(rooms) = rooms {
                    outcome.remote_hint |= rooms.drain(sync, deferrals, versions, progress).await?;
                }
                ensure_document(worker, workspace, *document).await?;
                drive_unit(sync, unit, deferrals, versions, outcome, progress).await?;
            }
        }
    }
    // An edit can race the network comparison. The projection's commit wake is
    // asynchronous, so check the durable positions again before claiming these
    // sampled revisions as current. This still never opens document content.
    let gate = authorize.clone();
    let current: HashMap<_, _> = store_result(worker.submit_store(move |store| {
        gate(Unit::Document {
            workspace_id: cloud(workspace.as_uuid()),
            document_id: cloud(manifest.as_uuid()),
        })
        .map_err(Error::Authority)?;
        let mut positions = store
            .document_positions(workspace)
            .map_err(|_| Error::Unavailable)?;
        positions.push((
            manifest,
            store
                .version(workspace, manifest)
                .map_err(|_| Error::Unavailable)?
                .encode(),
        ));
        Ok::<_, Error>(positions)
    }))
    .await??
    .into_iter()
    .collect();
    let mut changed = 0usize;
    let mut reordered = 0usize;
    for proof in &mut outcome.documents {
        let Unit::Document { document_id, .. } = proof.unit else {
            continue;
        };
        if !proof.carried {
            continue;
        }
        let current = current.get(&DocumentId::from_uuid(document_id.as_uuid()));
        if current.is_some_and(|state| same_revision(state, &proof.state)) {
            reordered += usize::from(current != Some(&proof.state));
        } else {
            if changed == 0 {
                tracing::info!(%workspace, %document_id,
                    current_bytes = current.map(Vec::len), proof_bytes = proof.state.len(),
                    "cloud sync: first invalidated durable document proof");
            }
            proof.carried = false;
            outcome.documents_synced = outcome.documents_synced.saturating_sub(1);
            outcome.remote_hint = true;
            changed += 1;
        }
    }
    if changed != 0 || reordered != 0 {
        tracing::info!(%workspace, changed, reordered, "cloud sync: rechecked durable document proofs");
    }
    progress(Progress::Documents {
        synced: outcome.documents_synced,
        required: None,
    });
    Ok(())
}

// Loro encodes a hash map, so byte order is not a document revision. Keep
// the cheap byte comparison, but decode before invalidating a carried proof.
// An absent/invalid position still cannot establish current coverage.
fn same_revision(left: &[u8], right: &[u8]) -> bool {
    if left == right {
        return true;
    }
    match (
        loro::VersionVector::decode(left),
        loro::VersionVector::decode(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
