// SPDX-License-Identifier: MPL-2.0

//! Stage an incoming update against recovered content without changing it.
use super::*;

/// Verified immutable intent bytes, not physical durability or current authority.
pub struct StagedObject {
    bytes: Vec<u8>,
    proof: ContentProof,
}
impl StagedObject {
    pub fn into_parts(self) -> (Vec<u8>, ContentProof) {
        (self.bytes, self.proof)
    }
}

/// Whether an incoming Loro blob is rooted at the empty document: no change
/// it carries depends on a change it does not carry.
///
/// That is what a client's creation of a unit looks like. A client that
/// joins a unit Cloud does not hold sends a snapshot of its whole history
/// (`client_store::export`), and a client whose fresh document joins a unit
/// another attempt has just created sends updates since that attempt's
/// version, which are still every change of its own fresh history. Both are
/// rooted at the empty document. An edit is not: its first change depends on
/// changes already in Cloud. A shallow snapshot is not either: it starts at
/// its shallow root. A blob with no change in it creates nothing.
///
/// This reads the blob's header and change metadata only. It is not a
/// validation: staging still checks the content.
pub fn starts_from_empty(payload: &[u8]) -> Result<bool, Error> {
    let meta = LoroDoc::decode_import_blob_meta(payload, true).map_err(|_| Error::Loro)?;
    Ok(meta.change_num > 0 && meta.start_frontiers.is_empty())
}

/// A first full-history checkpoint from actual received content, never an
/// empty fallback. Existing-unit conflicts are still resolved by SQL reserve.
pub fn stage_initial_checkpoint(
    metadata: Metadata,
    payload: &[u8],
    limits: Limits,
) -> Result<StagedObject, Error> {
    if metadata.kind != Kind::Checkpoint
        || metadata.sequence == 0
        || metadata.covers_through_sequence != 0
    {
        return Err(Error::Chain);
    }
    limits.payload(Kind::Checkpoint, payload.len() as u64)?;
    let doc = LoroDoc::new();
    applied(&doc, payload)?;
    let version = doc.state_vv().encode();
    let frontiers = doc.state_frontiers().encode();
    let bytes = encode(
        metadata,
        Body {
            payload,
            predecessor_version: &[],
            result_version: &version,
            checkpoint_frontiers: &frontiers,
        },
        limits,
    )?;
    let decoded = decode(&bytes, limits)?;
    let key = logical_key(metadata)?;
    let bound = decoded.bind(ExpectedObject {
        metadata,
        commitments: decoded.commitments(),
        logical_key: &key,
    })?;
    let proof = verify_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        HeadCommitment {
            result_version_digest: bound.commitments().result_version_digest,
        },
        limits,
    )?;
    Ok(StagedObject { bytes, proof })
}

impl RestoredContent {
    /// A later full checkpoint at this exact recovered segment head. The SQL
    /// capture supplies allocation/coverage; no new version encoding replaces
    /// the bytes already committed for that head.
    pub fn stage_checkpoint(&self, metadata: Metadata) -> Result<StagedObject, Error> {
        if metadata.dataset != self.proof.dataset
            || metadata.unit != self.proof.unit
            || metadata.cloud_unit_id != self.proof.cloud_unit_id
            || metadata.kind != Kind::Checkpoint
            || metadata.covers_through_sequence == 0
            || Some(metadata.covers_through_sequence) != self.proof.last_replayed_segment_sequence
            || metadata.sequence
                <= self
                    .proof
                    .checkpoint_object_sequence
                    .max(metadata.covers_through_sequence)
            || self.object_ids.contains(&metadata.object_id)
        {
            return Err(Error::Chain);
        }
        let payload = self.snapshot()?;
        let frontiers = self.doc.state_frontiers().encode();
        let bytes = encode(
            metadata,
            Body {
                payload: &payload,
                predecessor_version: &[],
                result_version: &self.result_version,
                checkpoint_frontiers: &frontiers,
            },
            self.limits,
        )?;
        let decoded = decode(&bytes, self.limits)?;
        let key = logical_key(metadata)?;
        let bound = decoded.bind(ExpectedObject {
            metadata,
            commitments: decoded.commitments(),
            logical_key: &key,
        })?;
        let proof = verify_chain(
            bound,
            CheckpointOrigin::LaterCheckpoint,
            &[],
            self.proof.head,
            self.limits,
        )?;
        Ok(StagedObject { bytes, proof })
    }

    /// None means every incoming operation is already in the verified head.
    /// Refusals leave the recovered document unchanged. The caller must still
    /// reserve, write, read back and conditionally publish before acknowledging.
    pub fn stage_segment(
        &self,
        metadata: Metadata,
        payload: &[u8],
    ) -> Result<Option<StagedObject>, Error> {
        if metadata.dataset != self.proof.dataset
            || metadata.unit != self.proof.unit
            || metadata.cloud_unit_id != self.proof.cloud_unit_id
            || metadata.kind != Kind::Segment
            || metadata.covers_through_sequence != 0
            || metadata.sequence
                <= self
                    .proof
                    .checkpoint_object_sequence
                    .max(self.proof.last_replayed_segment_sequence.unwrap_or(0))
        {
            return Err(Error::Scope);
        }
        self.limits.payload(Kind::Segment, payload.len() as u64)?;
        let meta = LoroDoc::decode_import_blob_meta(payload, true).map_err(|_| Error::Loro)?;
        // Profile validators additionally inspect the incoming snapshot in
        // isolation so LWW cannot hide another profile's metadata on merge.
        if matches!(
            metadata.unit,
            Unit::ProfileIdentity | Unit::ProfilePrivate | Unit::ProfilePersonal { .. }
        ) {
            let snapshot = self
                .doc
                .export(ExportMode::snapshot())
                .map_err(|_| Error::Loro)?;
            let model = ProfileModel::from_snapshot(metadata, &snapshot)?.ok_or(Error::Scope)?;
            model.stage_payload(metadata, payload)?;
        }
        let staged = self.doc.fork();
        applied(&staged, payload)?;
        if !staged.state_vv().includes_vv(&meta.partial_end_vv) {
            return Err(Error::Ancestors);
        }
        workspace_model(&staged, metadata)?;
        if staged.state_vv() == self.doc.state_vv() {
            return Ok(None);
        }
        // Preserve the reconstruction's unique-object invariant even before
        // SQL reservation independently refuses a reused object id.
        if self.object_ids.contains(&metadata.object_id) {
            return Err(Error::Chain);
        }
        // Capture once: exact bytes become both the object field and receipt.
        let result_version = staged.state_vv().encode();
        let bytes = encode(
            metadata,
            Body {
                payload,
                predecessor_version: &self.result_version,
                result_version: &result_version,
                checkpoint_frontiers: &[],
            },
            self.limits,
        )?;
        let decoded = decode(&bytes, self.limits)?;
        let proof = ContentProof {
            head: HeadCommitment {
                result_version_digest: decoded.commitments().result_version_digest,
            },
            head_object_sha256: decoded.commitments().object_sha256,
            last_replayed_segment_sequence: Some(metadata.sequence),
            ..self.proof
        };
        Ok(Some(StagedObject { bytes, proof }))
    }
}

#[cfg(test)]
mod tests;
