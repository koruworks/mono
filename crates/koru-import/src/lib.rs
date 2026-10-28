// SPDX-License-Identifier: MPL-2.0

//! Source-independent workspace import contract.
//!
//! Each format lives in its own `koru-import-*` crate. This crate owns only the
//! lifecycle the app can present consistently: source selection, inspection,
//! preview, conversion, progress and cancellation.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use koru_store_core::bundle::Bundle;
use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};

// No PartialEq: `claims_zip` is a function pointer, and comparing those by
// address is meaningless across codegen units. Nothing compares descriptors.
#[derive(Clone, Debug)]
pub struct ImporterDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub source: SourceRequest,
    /// The menu row label when the format is listed in import menus; `None`
    /// keeps it out of every menu (service importers such as `files`, or the
    /// legacy whole-workspace restore).
    pub menu_name: Option<&'static str>,
    /// The file-open dialog's title for `SourceRequest::{File, Files}`.
    /// Unused by remote (`Google`) sources, which run their own picker.
    pub picker_title: &'static str,
    /// Drop routing for a `.zip`: given the archive's entry names, claims the
    /// file for this importer. Evaluated in registration order; a zip no
    /// sniffer claims falls to the importer advertising `zip` without one.
    pub claims_zip: Option<fn(&[String]) -> bool>,
    /// The import produces one container document that everything else nests
    /// under (a OneNote notebook, a Notion export). The completion notice
    /// offers that root rather than the first file that happened to land.
    pub roots_import: bool,
    /// Whether the shell deduplicates file entries in this import against
    /// blobs already known to the workspace.
    pub dedups_known_files: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceRequest {
    Directory,
    File {
        extensions: &'static [&'static str],
    },
    Files {
        extensions: &'static [&'static str],
    },
    /// A remote source the importer authenticates and selects against itself
    /// (no file dialog, no path). Google Docs is the first: the importer runs
    /// OAuth and a browser picker, then carries the chosen documents through
    /// its own `PreparedImport`.
    Google,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportSource {
    Directory(PathBuf),
    Files(Vec<PathBuf>),
    Google,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityMode {
    Keep,
    Copy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    pub identity: IdentityMode,
    pub accept_warnings: bool,
}

/// The result of a format conversion. Workspace import remains available for
/// the old one-document format; note/service importers add documents to the
/// workspace that is already open.
pub enum ImportProduct {
    Workspace(Bundle),
    Documents(ImportedContent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedContent {
    pub documents: Vec<ImportedDocument>,
    pub warnings: Vec<ImportWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedDocument {
    /// Client-minted identity. Importers need the final id while constructing
    /// routed attachment references and parent links.
    pub id: DocumentId,
    pub source: PathBuf,
    pub title: String,
    /// Stable manifest token (`page` or `file`).
    pub kind: String,
    /// MIME type for file entries. Mirrored into the workspace listing so UI
    /// rows never need to open the opaque snapshot merely to choose an icon.
    pub mime: Option<String>,
    /// Another entry in this batch, created before this one.
    pub parent: Option<DocumentId>,
    /// Whether this entry belongs in Trash when it is first registered.
    /// `trashed_at` preserves a source timestamp when one is available; the
    /// host supplies the import instant when it is not.
    pub trashed: bool,
    pub trashed_at: Option<koru_store_core::model::Timestamp>,
    /// A complete opaque Koru document snapshot. The Store does not interpret
    /// it; it commits these bytes while registering the manifest row.
    pub snapshot: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportWarning {
    pub source: PathBuf,
    /// A stable, localizable classification. Existing importers may leave this
    /// absent and keep their English diagnostic in `message`.
    pub code: Option<ImportWarningCode>,
    /// Values for the localized template's `{}` placeholders, in order.
    pub arguments: Vec<String>,
    pub message: String,
}

/// Stable warning vocabulary shared by format crates and the host.
///
/// Keeping this closed makes the host's code-to-localization-key mapping
/// exhaustive: adding a warning cannot silently ship an untranslated branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportWarningCode {
    EnexEnvelopeTruncated,
    EnexSecurityRejected,
    InvalidSourceTimestamp,
    EvernoteLinkAmbiguous,
    EvernoteTaskPayloadUnsupported,
    EvernoteReminderNotMapped,
    InlineCheckboxSimplified,
    EvernoteMissingResource,
    EvernoteInvalidResource,
    EvernoteMimeMismatch,
    EvernoteUnknownElement,
    EvernoteMergedTableCells,
}

impl ImportWarningCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnexEnvelopeTruncated => "enex_envelope_truncated",
            Self::EnexSecurityRejected => "enex_security_rejected",
            Self::InvalidSourceTimestamp => "invalid_source_timestamp",
            Self::EvernoteLinkAmbiguous => "evernote_link_ambiguous",
            Self::EvernoteTaskPayloadUnsupported => "evernote_task_payload_unsupported",
            Self::EvernoteReminderNotMapped => "evernote_reminder_not_mapped",
            Self::InlineCheckboxSimplified => "inline_checkbox_simplified",
            Self::EvernoteMissingResource => "evernote_missing_resource",
            Self::EvernoteInvalidResource => "evernote_invalid_resource",
            Self::EvernoteMimeMismatch => "evernote_mime_mismatch",
            Self::EvernoteUnknownElement => "evernote_unknown_element",
            Self::EvernoteMergedTableCells => "evernote_merged_table_cells",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredBlob {
    pub hash: String,
    pub mime: String,
}

/// Maximum decoded size of one resource accepted by an importer.
///
/// This matches the Store upload boundary. Keeping the limit at the format
/// seam lets a streaming decoder stop before it creates a body the Store must
/// reject.
pub const MAX_IMPORT_BLOB_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The storage edge a format uses for local media. Keeping this beside the
/// importer mirrors export's `BlobSource` and keeps format crates independent
/// of the Store client and transport.
pub trait BlobSink: Send + Sync {
    fn put(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<StoredBlob, ImportError>;

    /// Store a stream whose factory can recreate it from byte zero for retry.
    /// Legacy in-memory sinks keep working through this collecting default;
    /// the Store-backed sink overrides it with a streaming HTTP body.
    fn put_from(
        &self,
        factory: &(dyn Fn() -> Box<dyn std::io::Read + Send> + Send + Sync),
        mime: &str,
        filename: Option<&str>,
    ) -> Result<StoredBlob, ImportError> {
        let mut bytes = Vec::new();
        factory()
            .read_to_end(&mut bytes)
            .map_err(|error| ImportError::new(error.to_string()))?;
        self.put(bytes, mime, filename)
    }
}

/// Worker-owned publication edge for a workspace's hidden system document.
///
/// Format crates only see opaque snapshots. They can stage tag definitions in
/// memory without linking the Store or UI, then publish an accepted source at
/// its transaction boundary. Implementations may block: imports call this on
/// their worker, never on the answering thread.
pub trait TagSink: Send + Sync {
    /// The current canonical system snapshot, or `None` when the workspace has
    /// never needed a system document.
    fn snapshot(&self) -> Result<Option<Vec<u8>>, ImportError>;

    /// Absorb and publish one accepted source's staged system snapshot.
    /// `register_system` is true only when the stage began without a system
    /// row and actually created a tag.
    fn publish(&self, snapshot: &[u8], register_system: bool) -> Result<(), ImportError>;
}

pub struct NoTagSink;

impl TagSink for NoTagSink {
    fn snapshot(&self) -> Result<Option<Vec<u8>>, ImportError> {
        Ok(None)
    }

    fn publish(&self, _: &[u8], _: bool) -> Result<(), ImportError> {
        Err(ImportError::new("this import has no tag destination"))
    }
}

pub struct ImportContext<'a> {
    /// Required when an importer emits routed references to imported content.
    pub target_profile: Option<ProfileId>,
    /// Required by document importers; absent for whole-workspace imports.
    pub target_workspace: Option<WorkspaceId>,
    /// Titles already present in the destination before this job starts.
    /// Importers that allocate a visible container can suffix deterministically
    /// without querying the Store themselves.
    pub occupied_titles: &'a BTreeSet<String>,
    pub blobs: &'a dyn BlobSink,
    pub tags: &'a dyn TagSink,
}

pub struct NoBlobSink;

impl BlobSink for NoBlobSink {
    fn put(&self, _: Vec<u8>, _: &str, _: Option<&str>) -> Result<StoredBlob, ImportError> {
        Err(ImportError::new("this import has no blob destination"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewNode {
    pub title: String,
    pub children: Vec<PreviewNode>,
    pub nodes: usize,
    pub trashed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPreview {
    pub native_workspace_id: Option<koru_store_core::ids::WorkspaceId>,
    pub workspace_name: String,
    pub documents: Vec<PreviewNode>,
    pub document_count: usize,
    pub node_count: usize,
    pub image_count: usize,
    pub warnings: Vec<String>,
    pub identity_modes: Vec<IdentityMode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportStage {
    Inspecting,
    Converting,
    ReadingMedia,
    BuildingBundle,
}

pub trait ImportProgress: Send + Sync {
    fn update(&self, stage: ImportStage, completed: usize, total: Option<usize>);
    fn cancelled(&self) -> bool;
    /// An importer may end its own flow for reasons that are not a failure —
    /// a user closing a picker, for example. Reporting that here lets the
    /// shell skip the "could not import" dialog and quietly clear the busy
    /// state, rather than string-matching a cancelled error.
    fn cancel(&self) {}
}

pub struct NoProgress;

impl ImportProgress for NoProgress {
    fn update(&self, _: ImportStage, _: usize, _: Option<usize>) {}
    fn cancelled(&self) -> bool {
        false
    }
}

#[derive(Default)]
pub struct CancellationToken(AtomicBool);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub trait Importer: Send + Sync {
    fn descriptor(&self) -> &'static ImporterDescriptor;
    fn inspect(
        &self,
        source: ImportSource,
        progress: &dyn ImportProgress,
    ) -> Result<Box<dyn PreparedImport>, ImportError>;
}

pub trait PreparedImport: Send {
    fn preview(&self) -> &ImportPreview;
    fn build(
        self: Box<Self>,
        options: ImportOptions,
        context: &ImportContext<'_>,
        progress: &dyn ImportProgress,
    ) -> Result<ImportProduct, ImportError>;
}

pub struct ImportRegistry {
    importers: Vec<Box<dyn Importer>>,
}

impl ImportRegistry {
    pub fn new(importers: Vec<Box<dyn Importer>>) -> Result<Self, ImportError> {
        let mut ids = std::collections::HashSet::new();
        for importer in &importers {
            if !ids.insert(importer.descriptor().id) {
                return Err(ImportError::new(format!(
                    "duplicate importer id: {}",
                    importer.descriptor().id
                )));
            }
        }
        Ok(Self { importers })
    }

    pub fn importers(&self) -> &[Box<dyn Importer>] {
        &self.importers
    }

    pub fn get(&self, id: &str) -> Option<&dyn Importer> {
        self.importers
            .iter()
            .find(|importer| importer.descriptor().id == id)
            .map(Box::as_ref)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportErrorCode {
    StoreUpdateRequired,
    /// The importer is compiled in but this build carries no configuration
    /// for it — a community build without a Google OAuth registration, for
    /// the Google Docs importer. Nothing was attempted.
    NotConfigured,
}

impl ImportErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StoreUpdateRequired => "import_store_update_required",
            Self::NotConfigured => "import_not_configured",
        }
    }
}

#[derive(Debug)]
pub struct ImportError {
    code: Option<ImportErrorCode>,
    message: String,
}

impl ImportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }
    pub fn coded(code: ImportErrorCode) -> Self {
        Self {
            code: Some(code),
            message: code.as_str().into(),
        }
    }
    pub fn code(&self) -> Option<ImportErrorCode> {
        self.code
    }
    pub fn cancelled() -> Self {
        Self::new("import cancelled")
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ImportError {}

impl From<String> for ImportError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ImportError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;
    static DESC: ImporterDescriptor = ImporterDescriptor {
        id: "stub",
        name: "Stub",
        source: SourceRequest::Directory,
        menu_name: None,
        picker_title: "Import a stub",
        claims_zip: None,
        roots_import: false,
        dedups_known_files: false,
    };
    impl Importer for Stub {
        fn descriptor(&self) -> &'static ImporterDescriptor {
            &DESC
        }
        fn inspect(
            &self,
            _: ImportSource,
            _: &dyn ImportProgress,
        ) -> Result<Box<dyn PreparedImport>, ImportError> {
            Err(ImportError::new("unused"))
        }
    }

    #[test]
    fn registry_refuses_duplicate_stable_ids() {
        assert!(ImportRegistry::new(vec![Box::new(Stub), Box::new(Stub)]).is_err());
    }

    #[test]
    fn registry_finds_a_format_by_stable_id() {
        let registry = ImportRegistry::new(vec![Box::new(Stub)]).unwrap();
        assert_eq!(registry.get("stub").unwrap().descriptor().name, "Stub");
        assert!(registry.get("missing").is_none());
    }
}
