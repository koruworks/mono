// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_read::document::Corrections;
use koru_store_core::{
    errors::{Code, Error},
    model::Timestamp,
    ports::UpdateOrigin,
};
use koru_store_wire::readings::{CorrectReading, ReadingCorrectionVersion, ReadingCorrections};

pub(super) fn wire(history: &Corrections) -> ReadingCorrections {
    ReadingCorrections {
        selected: history.selected().map(|(id, _)| id.to_owned()),
        versions: history
            .heads()
            .into_iter()
            .filter_map(|id| {
                let version = history.0.get(&id)?;
                Some(ReadingCorrectionVersion {
                    id,
                    text: version.text.as_ref().map(|text| text.as_str().to_owned()),
                })
            })
            .collect(),
    }
}

impl ReadingPort {
    pub(super) fn correction_history(
        &self,
        workspace: WorkspaceId,
        hash: &str,
    ) -> Result<Corrections, Error> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| Error::bare(Code::Internal))?;
        let mut store = state
            .store
            .lock()
            .map_err(|_| Error::bare(Code::Internal))?;
        history(&mut store, workspace, hash)
    }
    pub(super) fn save_correction(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
        request: CorrectReading,
    ) -> Result<ReadingCorrections, Error> {
        let storage = |message| Error::new(Code::StorageFailed, message);
        // A request cannot manufacture a reading for arbitrary bytes.
        let holder = self
            .items
            .holder(workspace, hash.as_str())
            .map_err(storage)?
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        let parent = holder;
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| Error::bare(Code::Internal))?;
        let at = Timestamp::now();
        let (document, manifest_delta, delta, answer) = {
            let mut store = state
                .store
                .lock()
                .map_err(|_| Error::bare(Code::Internal))?;
            store.workspace(workspace)?;
            if !store
                .documents(workspace)?
                .iter()
                .any(|row| row.id == parent)
            {
                return Err(Error::bare(Code::DocumentNotFound));
            }
            let history = history(&mut store, workspace, hash.as_str())?;
            let mut expected = request.expected_heads.clone();
            expected.sort();
            expected.dedup();
            if expected != history.heads() {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "the correction changed; refresh before saving",
                ));
            }
            let ensured = store.ensure_reading(workspace, parent, at)?;
            let document = ensured.id;
            let doc = koru_read::ReadingDoc::from_doc(store.fork_document(workspace, document)?);
            let before = doc.version();
            if doc.generation(hash.as_str()).is_none() {
                doc.begin(
                    hash.as_str(),
                    &koru_read::ReaderId::text(),
                    koru_read::GENERATION,
                )
                .map_err(|e| storage(e.to_string()))?;
            }
            doc.merge_corrections(hash.as_str(), &history)
                .map_err(|e| storage(e.to_string()))?;
            let revision = uuid::Uuid::new_v4().to_string();
            let accepted = doc
                .correct(
                    hash.as_str(),
                    &revision,
                    &request.expected_heads,
                    request.text.map(koru_read::PageText::new),
                )
                .map_err(|e| storage(e.to_string()))?;
            if !accepted {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "the correction changed; refresh before saving",
                ));
            }
            let answer = wire(&doc.corrections(hash.as_str()));
            let delta = doc
                .export_updates_since(&before)
                .map_err(|e| storage(e.to_string()))?;
            store.apply_update(workspace, document, delta.clone(), at, UpdateOrigin::Store)?;
            store.flush(workspace, document, at)?;
            (document, ensured.delta, delta, answer)
        };
        if !manifest_delta.is_empty() {
            state.broadcast_manifest(workspace, manifest_delta);
        }
        state.broadcast_document(document, delta);
        state.notify_replication();
        Ok(answer)
    }
}

/// Read the live documents under the same Store lock as compare-and-save.
/// The derived mirror can lag a commit, so it cannot decide whether an edit is stale.
fn history(
    store: &mut koru_store_api::state::KoruStore,
    workspace: WorkspaceId,
    hash: &str,
) -> Result<Corrections, Error> {
    let entries = store.documents(workspace)?;
    let mut result = Corrections::default();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == koru_core::explore::KIND_READING)
    {
        if let Ok(fork) = store.fork_document(workspace, entry.id) {
            result.merge_from(&koru_read::ReadingDoc::from_doc(fork).corrections(hash));
        }
    }
    Ok(result)
}
