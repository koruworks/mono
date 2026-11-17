// SPDX-License-Identifier: MPL-2.0

//! A person's Bots on their linked desktops (M8, P8.3).
//!
//! [`feed`] is the owner's half that needs no stream: what a page holds, who
//! is never sent, and when a page may be built. The server that reads a
//! linked desktop's hello and writes pages to its stream, the receipts of
//! what it asks for, and the copy a linked desktop keeps are separate pieces
//! and are not here.

pub(crate) mod feed;
