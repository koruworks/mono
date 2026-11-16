// SPDX-License-Identifier: MPL-2.0

//! **Which build this is, and where its data lives.**
//!
//! The leaf of the dependency graph: nothing here links anything, so anything
//! may link this. That is the whole design constraint, and it is worth stating
//! plainly because it is easy to erode — every future addition has to justify
//! itself against "could a crate that only wants to know its channel afford
//! this?".
//!
//! ## Why it exists
//!
//! [`ReleaseChannel`] used to live in `koru-update`, beside the update client.
//! That is a reasonable home for it right up until something *other than the
//! updater* needs to know what channel it is — at which point that thing links
//! `reqwest`, `zip` and `minisign-verify` to reach an enum with four variants.
//! `apps/koru-store-server` did exactly that, which is how it got written down as
//! TECHDEBT 19.
//!
//! ## What a channel decides
//!
//! Everything about where data goes. Two builds on one machine share nothing:
//!
//! ```text
//! %LOCALAPPDATA%\Koru\stable\
//! %LOCALAPPDATA%\Koru\nightly\
//! %LOCALAPPDATA%\Koru\internal-debug\
//! ```
//!
//! The same logical workspace may exist in several of them at once. Those are
//! **replicas** — same immutable id, separate physical state, converging one
//! day through ordinary sync. They must never open each other's files, and
//! separate directories is how that is made impossible rather than merely
//! unlikely.

mod build_info;
mod channel;
mod store;
pub mod tint;

pub use build_info::{BuildInfo, RELEASE_NOTES_FILE, build_info};
pub use channel::ReleaseChannel;
pub use store::{
    SEGMENTS, ai_root_under, artifacts_root_beside, channel_root, channel_root_of,
    delegate_root_beside, events_root_beside, looks_synced, segment, segment_of, shared_root,
    store_root, store_root_of, store_root_under,
};
