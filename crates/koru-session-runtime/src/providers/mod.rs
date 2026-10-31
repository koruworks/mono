// SPDX-License-Identifier: MPL-2.0

//! Provider-neutral managed-agent boundary.
//!
//! Provider wire values, process arguments, and native identifiers stop in
//! this module. Raw PTYs deliberately do not implement [`AgentControl`].

mod acp;
mod acp_failure;
mod catalog;
mod codex;
mod contract;
mod delegation;
mod discovery;
mod event_queue;
mod jsonl;
mod process;
mod readiness;
mod registry;
pub use readiness::ProviderReadinessCache;

pub use acp::*;
pub use acp_failure::*;
pub use catalog::{
    CATALOG_FORMAT, CatalogDrop, CatalogStore, MAX_CATALOG_FILE_BYTES, MemoryCatalogStore,
    ModelSlot, ModelSlots, ProviderCatalogCache,
};
pub use codex::*;
pub use contract::*;
pub use delegation::*;
pub use discovery::*;
pub use event_queue::*;
pub use jsonl::*;
pub use registry::*;

#[cfg(test)]
pub(super) mod acp_fake;
#[cfg(test)]
pub(crate) mod bridge_log_live;
