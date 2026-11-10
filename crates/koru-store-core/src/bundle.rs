// SPDX-License-Identifier: MPL-2.0

//! A workspace, out of the store and into one file.
//!
//! This module is the **shape** of a `.koru` bundle. Turning it into bytes is
//! `koru-store-sqlite`'s job, because that is where the zip lives; core defines
//! what is in one so that a second writer — a cloud export, a mobile share —
//! cannot quietly disagree about it.
//!
//! ```text
//! manifest.json          format_version, workspace, documents[], history[], blobs[]
//! documents/<id>.loro    one full snapshot per document
//! history/<id>.loro      retained immutable document versions
//! blobs/<hash>           the bytes
//! checksums.txt          one line per entry
//! ```
//!
//! **Snapshots, not journals.** A bundle carries the current snapshot plus the
//! immutable states named by user-visible history. The update journal remains
//! a transport detail of the store it came from.
//!
//! ## Restore and duplicate are different operations
//!
//! ```text
//! restore     same workspace id, same document ids
//!             -> another replica of one logical workspace, which must stay
//!                able to converge with its origin
//!
//! duplicate   new workspace id, new document ids
//!             -> an independent workspace that will never meet the original
//! ```
//!
//! Getting this wrong in either direction is bad in a way that takes a while to
//! notice. A "restore" that minted new ids gives you two workspaces that look
//! identical and never converge again. A "duplicate" that kept them gives you
//! two replicas of one workspace that will merge the moment they meet — which
//! is exactly what somebody asking for a copy did not want.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::errors::{Code, Error, Result};
use crate::ids::{DocumentId, DocumentVersionId, HistoryEntryId, WorkspaceId};
use crate::model::{Blob, DocumentKind, Timestamp};
use crate::ports::HistoryReason;
use crate::version::BUNDLE_FORMAT_VERSION;

/// What a bundle says about itself.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BundleManifest {
    pub format_version: u32,
    pub workspace: WorkspaceId,
    pub name: String,
    pub created_at: Timestamp,
    pub exported_at: Timestamp,
    /// Where this workspace came from, if it was itself derived.
    #[serde(default)]
    pub lineage: Vec<crate::model::Lineage>,
    pub documents: Vec<BundleDocument>,
    #[serde(default)]
    pub history_versions: Vec<BundleHistoryVersion>,
    #[serde(default)]
    pub history_entries: Vec<BundleHistoryEntry>,
    pub blobs: Vec<Blob>,
}

/// One document in a bundle.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BundleDocument {
    pub id: DocumentId,
    pub kind: DocumentKind,
    /// BLAKE3 of the snapshot, so a damaged entry is detectable before it is
    /// imported rather than after.
    pub checksum: String,
}

/// One immutable state named by one or more history markers.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BundleHistoryVersion {
    pub id: DocumentVersionId,
    pub document: DocumentId,
    pub version_vector: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub checksum: String,
    pub captured_at: Timestamp,
}

/// One visible point on a document's timeline.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BundleHistoryEntry {
    pub id: HistoryEntryId,
    pub document: DocumentId,
    pub version: DocumentVersionId,
    pub reason: HistoryReason,
    pub recorded_at: Timestamp,
    pub label: Option<String>,
    pub message: Option<String>,
    pub boundary: Option<String>,
    pub pinned: bool,
}

/// A whole workspace, in memory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bundle {
    pub manifest: BundleManifest,
    /// Snapshot bytes, in the same order as `manifest.documents`.
    pub snapshots: Vec<(DocumentId, Vec<u8>)>,
    /// Version bytes, in the same order as `manifest.history_versions`.
    pub history_snapshots: Vec<(DocumentVersionId, Vec<u8>)>,
    pub blobs: Vec<(crate::model::BlobHash, Vec<u8>)>,
}

/// How an incoming bundle should be treated.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    /// Another replica of the same logical workspace. Ids are preserved.
    Restore,
    /// An independent copy. New workspace id and new document ids.
    Duplicate,
}

impl ImportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restore => "restore",
            Self::Duplicate => "duplicate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "restore" => Some(Self::Restore),
            "duplicate" => Some(Self::Duplicate),
            _ => None,
        }
    }
}

/// What [`import_workspace`](crate::service::Store::import_workspace)
/// published: the Workspace, and whether its content was staged into a
/// repository this import claimed or into the one an interrupted Restore left
/// behind. A value, not a log line, so a caller cannot mistake a recovery for
/// a first import by accident.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ImportedWorkspace {
    pub workspace: WorkspaceId,
    pub publication: ImportPublication,
}

/// Where an import's content rows were staged before the publication.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ImportPublication {
    /// Into a repository this import claimed.
    Fresh,
    /// Into the repository an earlier Restore of the same bundle left behind
    /// when it crashed between staging and publication (root-catalog
    /// QUESTIONS.md Q6). Only a Restore resumes: a Duplicate mints a new id
    /// every time and never meets a leftover.
    Resumed,
}

impl ImportPublication {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Resumed => "resumed",
        }
    }
}

impl Bundle {
    /// Check the bundle is internally consistent before anything is written.
    ///
    /// Everything here is cheap and everything here is a reason not to start:
    /// importing half a workspace and then discovering the rest is unreadable
    /// leaves a workspace that looks present and is not.
    pub fn validate(&self) -> Result<()> {
        if self.manifest.format_version > BUNDLE_FORMAT_VERSION {
            return Err(Error::new(
                Code::BundleFormatUnsupported,
                format!(
                    "this bundle is format {} and this build understands {BUNDLE_FORMAT_VERSION}",
                    self.manifest.format_version
                ),
            ));
        }
        if !self
            .manifest
            .documents
            .iter()
            .any(|d| d.kind == DocumentKind::Manifest)
        {
            return Err(Error::new(
                Code::InvalidRequest,
                "the bundle has no manifest document",
            ));
        }
        let mut document_ids = HashSet::new();
        if self.snapshots.len() != self.manifest.documents.len() {
            return Err(Error::new(
                Code::InvalidRequest,
                "the bundle has unindexed snapshots",
            ));
        }
        for document in &self.manifest.documents {
            if !document_ids.insert(document.id) {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "the bundle lists a document twice",
                ));
            }
            let Some((_, bytes)) = self.snapshots.iter().find(|(id, _)| *id == document.id) else {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "the bundle lists a document it does not contain",
                ));
            };
            if crate::lifecycle::checksum(bytes) != document.checksum {
                return Err(Error::new(
                    Code::CorruptState,
                    "a document in the bundle does not match its checksum",
                ));
            }
            let probe = loro::LoroDoc::new();
            probe.import(bytes).map_err(|e| {
                Error::new(
                    Code::CorruptState,
                    format!("a document snapshot is unreadable: {e}"),
                )
            })?;
        }

        let carried_documents: HashSet<_> = self
            .manifest
            .documents
            .iter()
            .map(|document| document.id)
            .collect();
        if self.history_snapshots.len() != self.manifest.history_versions.len() {
            return Err(Error::new(
                Code::InvalidRequest,
                "the bundle has unindexed history snapshots",
            ));
        }
        let mut version_ids = HashSet::new();
        for version in &self.manifest.history_versions {
            if !version_ids.insert(version.id) || !carried_documents.contains(&version.document) {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "invalid history version index",
                ));
            }
            let Some((_, bytes)) = self
                .history_snapshots
                .iter()
                .find(|(id, _)| *id == version.id)
            else {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "the bundle lists a history version it does not contain",
                ));
            };
            if crate::lifecycle::checksum(bytes) != version.checksum {
                return Err(Error::new(
                    Code::CorruptState,
                    "a history version in the bundle does not match its checksum",
                ));
            }
            let probe = loro::LoroDoc::new();
            probe.import(bytes).map_err(|e| {
                Error::new(
                    Code::CorruptState,
                    format!("a history snapshot is unreadable: {e}"),
                )
            })?;
        }
        let mut entry_ids = HashSet::new();
        for entry in &self.manifest.history_entries {
            if !entry_ids.insert(entry.id)
                || !carried_documents.contains(&entry.document)
                || !version_ids.contains(&entry.version)
                || self
                    .manifest
                    .history_versions
                    .iter()
                    .find(|version| version.id == entry.version)
                    .is_none_or(|version| version.document != entry.document)
            {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "invalid history entry index",
                ));
            }
        }
        let manifest_id = self.manifest_document().expect("checked above");
        let manifest = crate::manifest::Manifest::from_snapshot(
            self.snapshot_of(manifest_id).expect("checked above"),
        )?;
        if manifest.workspace_id() != Some(self.manifest.workspace) {
            return Err(Error::new(
                Code::InvalidRequest,
                "the manifest names a different workspace",
            ));
        }
        let listed: HashSet<_> = manifest
            .entries()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        let carried: HashSet<_> = self
            .manifest
            .documents
            .iter()
            .filter(|document| document.kind == DocumentKind::Document)
            .map(|document| document.id)
            .collect();
        if listed != carried {
            return Err(Error::new(
                Code::InvalidRequest,
                "the manifest and carried documents disagree",
            ));
        }

        let mut blob_ids = HashSet::new();
        if self.blobs.len() != self.manifest.blobs.len() {
            return Err(Error::new(
                Code::InvalidRequest,
                "the bundle has unindexed blobs",
            ));
        }
        for blob in &self.manifest.blobs {
            if !blob_ids.insert(blob.hash.clone()) {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "the bundle lists a blob twice",
                ));
            }
            let Some((_, bytes)) = self.blobs.iter().find(|(hash, _)| *hash == blob.hash) else {
                return Err(Error::new(
                    Code::InvalidRequest,
                    "the bundle lists a blob it does not contain",
                ));
            };
            if blob.hash.algorithm() != crate::model::BlobHash::BLAKE3
                || blake3::hash(bytes).to_hex().as_str() != blob.hash.digest()
            {
                return Err(Error::new(
                    Code::BlobChecksumMismatch,
                    "a blob does not hash to its address",
                ));
            }
            if blob.size != bytes.len() as u64 {
                return Err(Error::new(
                    Code::BlobChecksumMismatch,
                    "a blob size does not match its metadata",
                ));
            }
        }
        Ok(())
    }

    pub fn manifest_document(&self) -> Option<DocumentId> {
        self.manifest
            .documents
            .iter()
            .find(|d| d.kind == DocumentKind::Manifest)
            .map(|d| d.id)
    }

    pub fn snapshot_of(&self, id: DocumentId) -> Option<&[u8]> {
        self.snapshots
            .iter()
            .find(|(other, _)| *other == id)
            .map(|(_, bytes)| bytes.as_slice())
    }

    pub fn history_snapshot_of(&self, id: DocumentVersionId) -> Option<&[u8]> {
        self.history_snapshots
            .iter()
            .find(|(other, _)| *other == id)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

#[cfg(test)]
mod tests;
