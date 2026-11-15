// SPDX-License-Identifier: MPL-2.0

//! The specialized decision seam, on the same canonical Store handle. It is
//! not invoked by the actor until the fixed worker/ledger integration lands.

use super::*;
use koru_delegate_core::decisions::{DecisionPreparedProjection, DecisionProjectionIdentity};
use koru_delegate_document::RunProjectionStaging;
use koru_delegate_runtime::{AppliedRunProjection, RunProjectionIntent};

// One sequencing owner for the real Link adapter and hermetic acknowledgement
// observations. This is not a new connection, worker or public capability.
trait ProjectionIo {
    fn send(&mut self, doc: &RunDocument) -> PortResult<()>;
    fn settle(&mut self, doc: &RunDocument) -> PortResult<()>;
    fn acknowledged(&self) -> &VersionVector;
}
struct LinkIo<'a> {
    client: &'a Client,
    document: DocumentId,
    link: &'a mut Link,
}
impl ProjectionIo for LinkIo<'_> {
    fn send(&mut self, doc: &RunDocument) -> PortResult<()> {
        self.link.commit(doc)
    }
    fn settle(&mut self, doc: &RunDocument) -> PortResult<()> {
        settle_link(self.client, self.document, doc, self.link)
    }
    fn acknowledged(&self) -> &VersionVector {
        &self.link.acked
    }
}

fn prepare_with_io(
    io: &mut impl ProjectionIo,
    staging: &mut RunProjectionStaging,
    doc: &RunDocument,
    intent: &RunProjectionIntent,
) -> PortResult<DecisionPreparedProjection> {
    intent.validate()?;
    // Re-echo joined remote operations under an actual batch receipt. An
    // empty in-flight queue alone does not qualify the causal base.
    io.send(doc)?;
    io.settle(doc)?;
    if !covers(io.acknowledged(), &doc.version()) {
        return Err(PortError::new(
            Port::Documents,
            "projection_base_pending",
            "projection base is not acknowledged",
        ));
    }
    Ok(staging.prepare(doc, intent)?)
}
fn apply_with_io(
    io: &mut impl ProjectionIo,
    staging: &mut RunProjectionStaging,
    doc: &RunDocument,
    prepared: &DecisionPreparedProjection,
) -> PortResult<AppliedRunProjection> {
    staging.apply(doc, prepared)?;
    io.send(doc)?;
    io.settle(doc)?;
    // settle can import remote updates. Revalidate the attached marker/state
    // after checking actual acknowledged causal coverage.
    Ok(staging.acknowledge(doc, prepared, io.acknowledged())?)
}

impl StoreHandle {
    fn projection_location(&self, identity: &DecisionProjectionIdentity) -> PortResult<()> {
        location_matches(
            self.workspace,
            self.document,
            identity.workspace,
            identity.document,
        )
    }
    pub(super) fn prepare_projection(
        &mut self,
        intent: &RunProjectionIntent,
    ) -> PortResult<DecisionPreparedProjection> {
        self.projection_location(&intent.identity)?;
        prepare_with_io(
            &mut LinkIo {
                client: &self.client,
                document: self.document,
                link: &mut self.link,
            },
            &mut self.projection,
            &self.doc,
            intent,
        )
    }
    pub(super) fn apply_projection(
        &mut self,
        prepared: &DecisionPreparedProjection,
    ) -> PortResult<AppliedRunProjection> {
        self.projection_location(&prepared.identity)?;
        apply_with_io(
            &mut LinkIo {
                client: &self.client,
                document: self.document,
                link: &mut self.link,
            },
            &mut self.projection,
            &self.doc,
            prepared,
        )
    }
}
fn location_matches(
    workspace: WorkspaceId,
    document: DocumentId,
    offered_workspace: WorkspaceId,
    offered_document: DocumentId,
) -> PortResult<()> {
    if workspace != offered_workspace || document != offered_document {
        return Err(PortError::new(
            Port::Documents,
            "projection_subject",
            "projection location does not match the handle",
        ));
    }
    Ok(())
}
fn covers(actual: &VersionVector, expected: &VersionVector) -> bool {
    matches!(
        actual.partial_cmp(expected),
        Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
    )
}

#[cfg(test)]
pub(super) mod tests;
