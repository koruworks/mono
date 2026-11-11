// SPDX-License-Identifier: MPL-2.0

//! **What a reader is asked and what it answers, the synced reading document
//! and how two devices' writes to it converge, and what the reading worker
//! says on its pipe.**
//!
//! The contract of "text in pictures and PDFs" (TECHNICAL D5). It links
//! **Loro and serde, and no inference runtime, no PDF library, no image
//! decoder, no socket and no node model** — `koru-voice`'s charter, one feature
//! over, and for the same reason: four components must agree about a reading
//! (the reader adapter, the worker, the Store daemon's coordinator and mirror,
//! the bench), so a renamed field breaks every build rather than drifting into
//! a silent `None`, and none of them may be forced to link another's runtime
//! to say so.
//!
//! ## What is here
//!
//! | | |
//! | --- | --- |
//! | [`reading`] | [`Reading`] and its tokens — [`ReadingStatus`], [`Reason`], [`Source`], [`ReaderId`] — plus [`GENERATION`] and the two page caps |
//! | [`document`] | [`ReadingDoc`]: the reading document's Loro shape and the only code that writes it |
//! | [`reader`] | [`Reader`], the seam a recognizer plugs into |
//! | [`wire`] | the worker's messages: [`Request`], [`Event`], [`WorkerStatus`], [`FailureCode`] |
//!
//! ## The reading document is not a node tree
//!
//! It is a child document of a file or a page, of manifest kind
//! `koru-reading`, with an id derived from its parent (TECHNICAL D9, §7). Its
//! only root is `doc.get_map("readings")`; nothing here knows what a node is,
//! and `koru-core` and `koru-projection` never walk one. That kind token and
//! the derived id belong to `koru-core`, which owns manifest kinds; this crate
//! owns what is inside.
//!
//! ## What it deliberately does not do
//!
//! No reading, no I/O, no transport — only shapes and the rules for writing
//! them. No translated string: every token here is branched on, and a surface
//! looks up its own words in `koru-i18n`.

/// Declares a closed token set whose serde spelling and stored spelling are one
/// table, so the wire and the document cannot drift apart.
macro_rules! tokens {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $token:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $token)] $variant,)+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant,)+];

            /// The token, as stored and as sent.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $token,)+
                }
            }

            /// The value a token names, or `None` for one this build does not
            /// know: a newer build may have written it, and one entry a build
            /// cannot understand must not make a whole document unreadable.
            pub fn parse(token: &str) -> Option<$name> {
                match token {
                    $($token => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

pub mod document;
pub mod reader;
pub mod reading;
pub mod wire;

pub use document::{Outcome, ReadingDoc, Write, is_blob_hash, pages_key};
pub use reader::{Picture, ReadError, Reader, ReaderInfo, Recognized};
pub use reading::{
    GENERATION, PageText, READ_PAGE_CAP, READER_TEXT, ReaderId, Reading, ReadingStatus, Reason,
    Source, TEXT_PAGE_CAP,
};
pub use wire::{
    BOOTSTRAP_VERSION, Event, FRAME_VERSION, FailureCode, JobId, PROTOCOL_VERSION, Request, Want,
    WorkerBootstrap, WorkerStatus,
};
