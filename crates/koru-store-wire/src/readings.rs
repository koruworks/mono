// SPDX-License-Identifier: MPL-2.0

//! **One reading, as `GET /v1/readings/{workspace}/{hash}` answers it** (OCR
//! TECHNICAL §10): the words read out of one file or picture in one
//! workspace, with what says how they were read.
//!
//! Tokens are strings, as the reading document stores them (`koru-read`'s
//! `ReadingStatus`, `Reason`, `Source`, `ReaderId`): this crate carries the
//! shape and interprets none of it, so a newer build's token arrives intact
//! rather than failing to parse.

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
}

/// One page's words. Lines are separated by `\n`, in reading order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadingPage {
    /// From 1.
    pub page: u32,
    pub text: String,
}
