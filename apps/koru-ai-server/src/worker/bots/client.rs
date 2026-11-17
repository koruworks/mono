// SPDX-License-Identifier: MPL-2.0

//! A person's Bots on their linked desktops (M8, P8.3).
//!
//! [`feed`] is the owner's half that needs no stream: what a page holds, who
//! is never sent, and when a page may be built. [`owner`] is the server on
//! the computer that runs the Bots: it answers a linked desktop's hello,
//! writes pages to its stream, runs the commands it may send, each behind a
//! durable receipt, and forwards a running reply while it watches. The copy
//! a linked desktop keeps of another computer's Bots, the stream it follows
//! that computer's feed with and the commands it sends there are [`mirror`].

pub(crate) mod feed;
pub(crate) mod mirror;
pub(crate) mod owner;
