// SPDX-License-Identifier: MPL-2.0

//! **One reading, as `GET /v1/readings/{workspace}/{hash}` answers it** (OCR
//! TECHNICAL §10): the words read out of one file or picture in one
//! workspace, with what says how they were read.
//!
//! Tokens are strings, as the reading document stores them (`koru-read`'s
//! `ReadingStatus`, `Reason`, `Source`, `ReaderId`): this crate carries the
//! shape and interprets none of it, so a newer build's token arrives intact
//! rather than failing to parse.
//!
//! Two requests a person can make of one item (OCR Plan 3.2; TECHNICAL §10):
//!
//! | a person chooses | route | answer | refused |
//! | --- | --- | --- | --- |
//! | *Read all pages* | `POST /v1/readings/{workspace}/{hash}/read-all` | [`ReadRequested`] | `blob_not_here`, `reader_unavailable`, `invalid_request` |
//! | *Read again* | `POST /v1/readings/{workspace}/{hash}/read-again` | [`ReadRequested`] | the same |
//!
//! [`ReadingAnswer::offers`] says beforehand which of the two would be
//! accepted, so a surface offers only those.

use koru_store_core::ids::{DocumentId, WorkspaceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// **The winning reading of one blob in one workspace** — the one the index
/// finds it by: the highest generation, then the best status, then the one
/// that reached the most pages.
///
/// It carries a file's words, so it is only ever asked for one item at a
/// time, by someone who already holds the item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadingAnswer {
    pub workspace: WorkspaceId,
    /// The blob's address (`b3:<hex>`).
    pub hash: String,
    /// The entry whose reading document holds it: the file, or the page the
    /// picture is in.
    pub document: DocumentId,
    /// Which reader made it (`text`, …). Absent when only the derived mirror
    /// could be consulted.
    #[serde(default)]
    pub reader: Option<String>,
    pub generation: u32,
    /// `read`, `partial`, `empty` or `failed`; absent for a token this build
    /// cannot name.
    #[serde(default)]
    pub status: Option<String>,
    /// With `failed`: `password` or `damaged`; with `partial`: `page-cap`.
    #[serde(default)]
    pub reason: Option<String>,
    /// `text-layer`, `recognized`, `mixed` or `plain`.
    #[serde(default)]
    pub source: Option<String>,
    /// `0..=100`; absent for text, which has none to report.
    #[serde(default)]
    pub confidence: Option<u8>,
    /// With `partial`: how far reading has got.
    #[serde(default)]
    pub pages_done: Option<u32>,
    /// The words, page by page, in page order.
    pub pages: Vec<ReadingPage>,
    /// **What a person may ask of this item on this computer, now** (OCR
    /// Plan 3.2): whether a surface shows *Read all pages* and *Read again*.
    /// The Store decides, so no surface guesses from the tokens above. Absent
    /// from an older Store, which reads as nothing offered.
    #[serde(default)]
    pub offers: ReadingOffers,
}

/// Which of the two requests the Store would accept for one item here.
///
/// Each is `true` only where the request would be accepted: the item's bytes
/// are on this computer, the reader is installed when finishing it needs
/// recognition, and no request for it is already waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadingOffers {
    /// *Read all pages*: a PDF that stopped at a page limit (`partial`,
    /// `page-cap`). A PDF stopped at the text-layer cap whose words all came
    /// from its text layer needs no reader for it.
    #[serde(default)]
    pub read_all: bool,
    /// *Read again*: an item that has been read (`read`, `empty`) or whose
    /// reading failed (`failed`).
    #[serde(default)]
    pub read_again: bool,
}

/// Which request a person made (`POST …/read-all`, `POST …/read-again`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadRequestKind {
    ReadAll,
    ReadAgain,
}

/// **The answer to a request that was accepted**: recorded on this computer,
/// and met by the reading worker behind anything freshly added. Progress is
/// the item's reading, read with `GET /v1/readings/{workspace}/{hash}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadRequested {
    pub workspace: WorkspaceId,
    /// The blob's address (`b3:<hex>`).
    pub hash: String,
    pub kind: ReadRequestKind,
}

/// One page's words. Lines are separated by `\n`, in reading order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadingPage {
    /// From 1.
    pub page: u32,
    pub text: String,
}
