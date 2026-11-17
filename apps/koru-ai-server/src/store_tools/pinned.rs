// SPDX-License-Identifier: MPL-2.0

//! A page read at one immutable Store version (M6): what a Bot is handed back
//! from the Bot it asked.
//!
//! The version id is never the model's. It comes from the saved-result entry
//! of the child Run, which holds the id the Store returned when the child
//! saved. The read is made under the *reader's* own authority, checked now:
//! the same check its `read_document` passes. There is no path here that reads
//! the live body of the page, so an absent or damaged version is reported
//! absent or damaged and is never replaced by the page as it is today.
//!
//! One Store hop takes the bounded bytes. The checksum and the rendering,
//! which are integrity work, run after it, off the Store owner.
use super::native::NativeStoreAuthority;
use super::*;
use koru_store_core::ids::DocumentVersionId;
use koru_store_core::ports::{BoundedDocumentVersion, MAX_HISTORY_REVISION_BYTES};
use serde_json::Value;

/// Why a pinned version cannot be handed over. The token is
/// `details.reason` of `result_version_unavailable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinnedUnavailable {
    /// The version row is not in the Store, or it is another page's.
    Missing,
    /// The bytes do not match the version's checksum, or do not open as a page.
    Corrupt,
    /// The result was saved without a version id.
    NotPinned,
    /// The snapshot is above the read bound.
    TooLarge,
}
impl PinnedUnavailable {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
            Self::NotPinned => "not_pinned",
            Self::TooLarge => "too_large",
        }
    }
}

/// Why a pinned read gave no page. None of these carries content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinnedReadError {
    /// The reader's own authority refuses the page now, or the page is in
    /// Trash or hidden. Restoring it makes it readable again.
    NotReadable,
    /// The version cannot be handed over. Never the latest version instead.
    Unavailable(PinnedUnavailable),
    /// The Store did not answer. An outage is never a missing version.
    StoreUnavailable,
}
impl PinnedReadError {
    /// The tool code this answers.
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotReadable => "result_not_readable",
            Self::Unavailable(_) => "result_version_unavailable",
            Self::StoreUnavailable => "native_owner_unavailable",
        }
    }
    pub const fn reason(self) -> Option<PinnedUnavailable> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::NotReadable | Self::StoreUnavailable => None,
        }
    }
    pub const fn retryable(self) -> bool {
        matches!(self, Self::StoreUnavailable)
    }
}

/// One page as it was at `version_id`, rendered as `read_document` renders
/// the live page. Content: it goes to the reader's tool reply and nowhere
/// else.
#[derive(Clone, PartialEq)]
pub struct PinnedPage {
    pub document: DocumentId,
    pub version_id: DocumentVersionId,
    pub captured_at_ms: i64,
    pub content: Value,
}
impl std::fmt::Debug for PinnedPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedPage")
            .field("document", &self.document)
            .field("version_id", &self.version_id)
            .finish_non_exhaustive()
    }
}

/// What the one Store hop found, before any integrity work.
enum Fetched {
    Version(Box<BoundedDocumentVersion>),
    Refused(PinnedReadError),
}

/// Read `document` at `version` under `authority`.
///
/// 1. The reader's read check, the one its `read_document` passes: the
///    workspace, the person and the placement, then that the page is visible
///    and not in Trash.
/// 2. The version by id, bounded.
/// 3. The checksum against the bytes; the bounded read does not verify it.
/// 4. The version's own page must be `document`.
/// 5. The snapshot, rendered by the renderer `read_document` uses.
///
/// `version` is `None` for a result saved before versions were kept.
pub fn read_pinned_page(
    client: StoreWorkerClient,
    authority: NativeStoreAuthority,
    document: DocumentId,
    version: Option<DocumentVersionId>,
) -> Result<PinnedPage, PinnedReadError> {
    let workspace = authority.workspace;
    let fetched = client
        .with_store(move |store| {
            if authority.check(store, workspace).is_err() {
                return Fetched::Refused(PinnedReadError::NotReadable);
            }
            let visible = match live_entry_state(store, workspace, document) {
                Ok(state) => state,
                Err(refusal) => return Fetched::Refused(refusal),
            };
            let Some(version) = version else {
                // The check above still decides first: a page the reader may
                // not read says so, whatever its entry holds.
                return Fetched::Refused(if visible {
                    PinnedReadError::Unavailable(PinnedUnavailable::NotPinned)
                } else {
                    PinnedReadError::NotReadable
                });
            };
            // The row's identity, without its bytes.
            let known = match store.document_version_revision(
                workspace,
                version,
                MAX_HISTORY_REVISION_BYTES,
            ) {
                Ok(known) => known,
                Err(_) => {
                    return Fetched::Refused(if visible {
                        PinnedReadError::Unavailable(PinnedUnavailable::Corrupt)
                    } else {
                        PinnedReadError::NotReadable
                    });
                }
            };
            let Some(known) = known else {
                // No such version. A page that is gone took its versions with
                // it; that is `missing`, not a refusal to read.
                return Fetched::Refused(PinnedReadError::Unavailable(PinnedUnavailable::Missing));
            };
            if !visible {
                // The page is in Trash or hidden and its version is still
                // there: not readable until it is restored.
                return Fetched::Refused(PinnedReadError::NotReadable);
            }
            if known.document != document {
                return Fetched::Refused(PinnedReadError::Unavailable(PinnedUnavailable::Missing));
            }
            match store.bounded_document_version(workspace, version) {
                Ok(Some(found)) => Fetched::Version(Box::new(found)),
                Ok(None) => {
                    Fetched::Refused(PinnedReadError::Unavailable(PinnedUnavailable::Missing))
                }
                // The row is there and its identity reads, but its bytes do
                // not come back within the bound.
                Err(_) => {
                    Fetched::Refused(PinnedReadError::Unavailable(PinnedUnavailable::TooLarge))
                }
            }
        })
        .map_err(|_| PinnedReadError::StoreUnavailable)?;
    match fetched {
        Fetched::Refused(refusal) => Err(refusal),
        Fetched::Version(found) => verified(*found, document).map_err(PinnedReadError::Unavailable),
    }
}

/// Whether the page is visible and not in Trash. `Err` when it is there and
/// is not a page: that is never readable as one.
fn live_entry_state(
    store: &mut koru_store_server::WorkerStore,
    workspace: WorkspaceId,
    document: DocumentId,
) -> Result<bool, PinnedReadError> {
    match store.document_entry_live(workspace, document) {
        Ok(Some(entry)) if entry.kind == "page" => Ok(true),
        Ok(Some(_)) | Err(_) => Err(PinnedReadError::NotReadable),
        Ok(None) => Ok(false),
    }
}

/// The integrity work, off the Store owner: the checksum, the page the
/// version belongs to, and the rendering. Pure, so each refusal is tested on
/// the value alone.
pub(crate) fn verified(
    found: BoundedDocumentVersion,
    document: DocumentId,
) -> Result<PinnedPage, PinnedUnavailable> {
    if koru_store_core::lifecycle::checksum(&found.snapshot) != found.checksum {
        return Err(PinnedUnavailable::Corrupt);
    }
    if found.document != document {
        return Err(PinnedUnavailable::Missing);
    }
    let content = koru_mcp_server::native::render_pinned_document(document, &found.snapshot)
        .map_err(|_| PinnedUnavailable::Corrupt)?;
    Ok(PinnedPage {
        document,
        version_id: found.id,
        captured_at_ms: found.captured_at.millis(),
        content,
    })
}

#[cfg(test)]
mod tests;
