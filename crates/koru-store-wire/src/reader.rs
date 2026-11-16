// SPDX-License-Identifier: MPL-2.0

//! **The reader that finds words in pictures and scanned pages: whether it is
//! on this computer, what downloading it would mean, and how far a download
//! has got** (OCR TECHNICAL D16, §7 "Preferences and consent", §10).
//!
//! The Store daemon installs the model, because the offer appears in three
//! places (an import's summary, `Ctrl+P`, the Agent-hosted `Ctrl+Shift+K`
//! window) and in Settings, and a download has to outlive a closed window. So
//! every surface asks the Store one question — [`ReaderState`] — and sends it
//! one of these answers:
//!
//! | a person… | route | body |
//! | --- | --- | --- |
//! | looks | `GET /v1/readings/reader` | — |
//! | presses **Turn on** (not downloaded) | `POST /v1/readings/reader/install` | [`InstallReader`] |
//! | presses **Stop** | `POST /v1/readings/reader/stop` | — |
//! | presses **Turn off** | `POST /v1/readings/reader/off` | — |
//! | presses **Turn on** (downloaded, off) | `POST /v1/readings/reader/on` | — |
//! | presses **Remove** | `DELETE /v1/readings/reader` | — |
//! | presses **Not now** | `POST /v1/readings/reader/decline` | — |
//!
//! and an import that finished calls `POST /v1/readings/reader/imported` with
//! [`ImportFinished`], which is what lets Koru offer once more after a *Not
//! now*.
//!
//! ## Off is not removed (OCR TECHNICAL §7, §9 "The reader is turned off")
//!
//! **Turn off** stops reading and keeps the download; [`ReaderState::turned_off`]
//! says so. Whether the reader is on is machine-local, like the consent it
//! follows. While it is off, nothing is recognized (text layers and plain
//! files are still read), nothing is offered, and what was read stays
//! findable; **Turn on** needs no download. **Remove** deletes the download,
//! whether the reader was on or off.
//!
//! ## Tokens, never sentences
//!
//! Every state here is a token a surface branches on and words in its own
//! language through `koru-i18n`. Sizes are bytes; a surface rounds them with
//! `koru_voice_consent::megabytes` so the offer and the consent record agree.
//!
//! ## What is never here
//!
//! A file's name, a title, a path or a word read from anything. The licence
//! and attribution are the model's own published text, from its descriptor.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// **Everything a surface needs to draw the offer, the Settings row and a
/// download's progress**, in one answer that is a read of memory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderState {
    /// The model on this computer, verified, or `None`. Asked of the disk by
    /// the Store's installer, never remembered in a file.
    pub installed: Option<InstalledReader>,
    /// Why this computer cannot use a reader at all, or `None` when nothing
    /// says it cannot. A surface that sees `Some` shows no **Turn on**
    /// anywhere and says plainly that the reader is not set up here — an
    /// install would be refused with `reader_unavailable`.
    pub unavailable: Option<ReaderUnavailable>,
    /// What downloading the reader would mean — the terms a person is shown
    /// and agrees to, exactly — once the Store has read its catalog. `None`
    /// until then, and when the catalog could not be read ([`Self::catalog`]).
    pub offer: Option<ReaderTerms>,
    /// Whether the Store has read the reader's catalog, and if it could not,
    /// why. A surface showing the offer offline says *needs a one-time
    /// download; Koru will offer it when you are online* while this is
    /// `failed` with `offline`.
    pub catalog: CatalogState,
    /// The download, while one runs and after it ends, until the next starts.
    pub install: Option<InstallProgress>,
    /// Whether the worker on this computer is recognizing right now: a reader
    /// installed, the worker started with it, and its first status said so.
    /// `false` while nothing has needed it yet, which is not a fault.
    pub recognizing: bool,
    /// *Not now* was answered, and no import has brought a picture or a
    /// scanned PDF since. Search answers carry no offer while it is set; the
    /// Settings row ignores it.
    pub offer_declined: bool,
    /// **The reader is downloaded and turned off** on this computer: only
    /// ever `true` with [`Self::installed`] set. Nothing is recognized and no
    /// surface offers it; **Turn on** (`POST /v1/readings/reader/on`) needs
    /// no download. Absent from an older Store, which had no off but removal.
    #[serde(default)]
    pub turned_off: bool,
    /// The totals for **this computer**, every workspace on it: what the
    /// Settings row shows.
    ///
    /// While the reader is off, what waits for it is counted as it is with no
    /// reader at all: in `unread` and `needs_reader`. A search answer's
    /// `offer` is `false` (it is downloaded), so the line in search says
    /// nothing about those items — *Still reading* counts only what is being
    /// read, and nothing waiting for the reader is.
    pub counts: MachineReadingCounts,
}

/// A model on this computer, from its signed descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstalledReader {
    pub model: String,
    pub version: String,
    /// What it took to download, as its descriptor declares it.
    pub download_bytes: u64,
    pub licence: String,
    pub attribution: String,
}

/// **What a person agrees to**: the offer's terms, from the catalog's signed
/// descriptor. Sent back verbatim in [`InstallReader::terms`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderTerms {
    pub model: String,
    pub version: String,
    /// The SPDX identifier the weights are published under.
    pub licence: String,
    /// Who the licence requires be credited.
    pub attribution: String,
    /// The download, in bytes — the real size, never a number in a sentence.
    pub download_bytes: u64,
    /// The languages it reads, as BCP 47 tags (`und-Latn`, `zh`, `ja`). The
    /// Settings row says which languages are read.
    pub languages: Vec<String>,
}

/// Why a reader cannot be used on this computer. Stable tokens.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReaderUnavailable {
    /// This build may install no model: it pins no model-signing key and is a
    /// stable build, where the unsigned arm is refused (TECHNICAL §8).
    NoTrustRoot,
    /// This host runs no reader at all: a phone, the integration host.
    NoWorker,
    /// The read worker is not beside the Store, or will not start.
    WorkerMissing,
    /// The read worker here was built without recognition (Linux, Windows on
    /// ARM, a build made without the feature): text-layer PDFs and plain-text
    /// files are read, pictures never are.
    NotBuilt,
    /// The reader's catalog lists no model this build can use.
    NoModel,
    /// A reader is installed and the worker could not load it, twice. Remove
    /// it and turn it on again.
    WillNotLoad,
}

impl ReaderUnavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoTrustRoot => "no_trust_root",
            Self::NoWorker => "no_worker",
            Self::WorkerMissing => "worker_missing",
            Self::NotBuilt => "not_built",
            Self::NoModel => "no_model",
            Self::WillNotLoad => "will_not_load",
        }
    }
}

/// Whether the reader's catalog has been read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CatalogState {
    /// `unread` (nothing has needed it yet), `reading`, `ready` or `failed`.
    pub state: CatalogStage,
    /// With `failed`: the model store's own code (`offline`,
    /// `catalog_unavailable`, `catalog_invalid`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogStage {
    #[default]
    Unread,
    Reading,
    Ready,
    Failed,
}

/// **Where a download has got to.** Polled; the Store coalesces nothing a
/// surface has to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstallProgress {
    pub stage: InstallStage,
    /// Bytes done of [`Self::total`] while `downloading`; zero otherwise.
    pub received: u64,
    /// Bytes expected; zero when the stage has no total.
    pub total: u64,
    /// With `failed`: the model store's code (`offline`, `download_failed`,
    /// `digest_mismatch`, `consent_required`, `leased`, …). A person presses
    /// **Try again**, which is another install; a partial download resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// The stages of an install as a person sees them, in order, and how one
/// ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallStage {
    /// Accepted; the installer has not reached the network yet.
    Starting,
    Downloading,
    /// Re-hashing the whole download. Its own stage, because on a slow disk a
    /// bar sitting at 100% reads as a hang.
    Verifying,
    /// Unpacking and moving it into place.
    Installing,
    /// Asked to stop, and stopping.
    Stopping,
    /// Removing the model from disk.
    Removing,
    // ---- how the last one ended
    Installed,
    /// **Stop** was pressed. Not a failure.
    Stopped,
    Removed,
    Failed,
}

impl InstallStage {
    /// Whether this is still going.
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Downloading
                | Self::Verifying
                | Self::Installing
                | Self::Stopping
                | Self::Removing
        )
    }
}

/// **The totals for this computer**, every workspace on it.
///
/// An item is one readable blob referenced by a live entry — a file, or a
/// picture in a page — and every item is in exactly one of the first five
/// counts:
///
/// `total = read + partly_read + could_not_be_read + unread`
///
/// where `unread` is still to be read here, or waiting (see
/// `koru_store_wire::search::ReadingCounts`, which is the same numbers for a
/// query's workspaces).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineReadingCounts {
    pub total: u64,
    /// Finished: words found, or a picture with none (a photograph of a dog
    /// is never counted as a failure).
    pub read: u64,
    /// Stopped at a page limit (`partial/page-cap`): found by the pages read.
    pub partly_read: u64,
    /// Found by name only: a password, damage (true on every device), or
    /// something this computer could not do (a format it cannot open, a file
    /// that crashed the worker) after its retries.
    pub could_not_be_read: u64,
    /// [`Self::could_not_be_read`] by reason: `password`, `damaged`,
    /// `unsupported`, `crashed`, `memory-limit`, `hostile`, `internal`,
    /// `unreadable`.
    pub could_not_be_read_by_reason: BTreeMap<String, u64>,
    /// Still to be read, or waiting.
    pub unread: u64,
    /// Of `unread`, waiting for the reader to be installed.
    pub needs_reader: u64,
    /// Of `unread`, whose bytes are not on this computer.
    pub not_here: u64,
}

/// The body of `POST /v1/readings/reader/install`: **the terms the person was
/// shown**, as `GET /v1/readings/reader` gave them, and where they answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstallReader {
    pub terms: ReaderTerms,
    /// Which surface the person answered in. Recorded nowhere; it decides
    /// only that the Settings row may install whatever a *Not now* said.
    pub from: InstallFrom,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallFrom {
    /// The one line in search, or an import's summary.
    Offer,
    /// The **Text in pictures and PDFs** row in Settings, which can always
    /// turn the reader on.
    Settings,
}

/// The body of `POST /v1/readings/reader/imported`: an import through an
/// importer finished, and this is how many pictures and scanned PDFs it
/// brought. A non-zero count makes Koru offer once more after a *Not now*
/// (EXPERIENCE §3); adding or pasting single files never calls this.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportFinished {
    pub pictures_and_scans: u64,
}

// ------------------------------------------------- the items behind the counts

/// **The items behind the Settings row's *could not be read* and *partly
/// read* lists**: the answer to `GET /v1/readings/reader/items` (OCR Plans 2.5
/// and 3.5).
///
/// **Ids and tokens only.** No file name, no title, no path crosses from the
/// Store's reading code: a surface names each item from the workspace listing
/// it already reads, the way a search surface names a hit's document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderItems {
    /// Every workspace on this computer, workspace by workspace, each in the
    /// order its listing gives.
    pub items: Vec<ReaderItem>,
    /// More items exist than [`READER_ITEMS_LIMIT`]; the list is the first
    /// that many. The count on the row is still the whole number.
    #[serde(default)]
    pub truncated: bool,
}

/// The most items one answer carries. A computer with thousands of files that
/// could not be read is told the first thousand and that there are more; a
/// list that long is not read by anybody, and the count says the rest.
pub const READER_ITEMS_LIMIT: usize = 1_000;

/// One item: an entry and the blob it holds that ended this way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderItem {
    pub workspace: koru_store_core::ids::WorkspaceId,
    /// The entry the blob belongs to: the file itself, or the page the
    /// picture is in. Where picking the item goes.
    pub document: koru_store_core::ids::DocumentId,
    /// The blob's address (`b3:<hex>`).
    pub hash: String,
    pub kind: ReaderItemKind,
    /// Why, as a token. *Could not be read*: `password`, `damaged` (true on
    /// every device), or a failure true on this computer only —
    /// `unsupported`, `crashed`, `memory-limit`, `hostile`, `internal`,
    /// `unreadable`. *Partly read*: `page-cap`, or empty while a person's
    /// *Read all pages* is under way. A surface words the tokens it knows and
    /// says something general for one it does not.
    pub reason: String,
    /// *Partly read*: how far it got — the pages read so far. Absent for an
    /// item that could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages_done: Option<u32>,
    /// What a person may ask of this item on this computer now, decided as
    /// `GET /v1/readings/{workspace}/{hash}` decides it for a menu: *Read all
    /// pages* beside a partly read item, *Read again* beside one that could
    /// not be read. Absent from an older Store: nothing offered.
    #[serde(default)]
    pub offers: crate::readings::ReadingOffers,
    /// A person's request for this item is waiting or under way here. A
    /// surface asks again while one is, to show how far it got.
    #[serde(default)]
    pub requested: bool,
}

/// How an item ended. **A surface ignores a kind it does not know.**
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReaderItemKind {
    /// Found by name only.
    CouldNotBeRead,
    /// Stopped at a page limit, found by the pages read — or being read to
    /// its end because a person asked (OCR Plan 3.5). It leaves the list when
    /// it finishes.
    PartlyRead,
}

#[cfg(test)]
mod tests;
