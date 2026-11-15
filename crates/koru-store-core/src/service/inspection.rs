// SPDX-License-Identifier: MPL-2.0

//! Detached committed local content. This is never a Cloud-head or authority
//! proof, and never a live editor handle.
use super::*;

/// Input bounds supplied by the native reader. These bound encoded bytes and
/// journal rows, not Loro's decoded heap. Replay runs outside Store ownership.
#[derive(Clone, Copy)]
pub struct DocumentInspectionBudget {
    pub payload_bytes: u64,
    pub journal_rows: u32,
    pub metadata_bytes: u32,
}

pub struct DetachedDocument {
    workspace: WorkspaceId,
    document: DocumentId,
    doc: loro::LoroDoc,
    version: Vec<u8>,
}
impl std::fmt::Debug for DetachedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DetachedDocument(<redacted>)")
    }
}
impl DetachedDocument {
    /// Backend boundary after a strict, read-only replay of an existing row.
    /// The original durable position must match the complete replay. A caller
    /// still needs current authority and original Cloud version evidence.
    pub fn from_replayed(
        workspace: WorkspaceId,
        document: DocumentId,
        doc: loro::LoroDoc,
        version: Vec<u8>,
        frontiers: &[u8],
    ) -> Result<Self> {
        let expected =
            loro::VersionVector::decode(&version).map_err(|_| Error::bare(Code::CorruptState))?;
        let expected_frontiers =
            loro::Frontiers::decode(frontiers).map_err(|_| Error::bare(Code::CorruptState))?;
        if doc.state_vv() != expected
            || doc.oplog_vv() != expected
            || doc.state_frontiers() != expected_frontiers
        {
            return Err(Error::bare(Code::CorruptState));
        }
        Ok(Self {
            workspace,
            document,
            doc,
            version,
        })
    }
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    pub fn document(&self) -> DocumentId {
        self.document
    }
    /// Exact local captured bytes, not a Cloud receipt or re-encoded VV.
    pub fn version(&self) -> &[u8] {
        &self.version
    }
    pub fn into_doc(self) -> loro::LoroDoc {
        self.doc
    }
}

pub type DocumentInspection = Box<dyn FnOnce() -> Result<DetachedDocument> + Send>;

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Capture a cold existing document without opening it in the editor,
    /// creating storage, repairing positions, or changing manifest membership.
    /// Run the returned job outside the Store lock, then revalidate authority.
    pub fn prepare_document_inspection(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        budget: DocumentInspectionBudget,
    ) -> Result<DocumentInspection> {
        self.workspace_including_deleted(workspace)?;
        Ok(self
            .backend
            .prepare_document_inspection(workspace, document, budget))
    }
}
