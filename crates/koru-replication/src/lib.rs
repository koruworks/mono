// SPDX-License-Identifier: MPL-2.0

//! Transport-neutral Store replication behavior.
//!
//! The Iroh endpoint and QUIC adapter live in `koru-network-iroh`. This crate
//! owns Store synchronization frames, authorization fences, durable-ack
//! semantics, and transport-independent replication history types.

use std::collections::VecDeque;

use koru_device::DeviceId;

pub mod cloud;
pub mod protocol;
pub mod reconcile;
pub mod root_exchange;

pub use protocol::PEER_PROTOCOL_VERSION;

pub const STATE_EVENT_PAGE_SIZE: usize = 50;

/// Durable-change hints coalesce until this trailing quiet period has elapsed.
/// Shared by peer announcements and Cloud standing-session notifications.
pub const CHANGE_ANNOUNCEMENT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncEvent {
    pub sequence: u64,
    pub at_millis: i64,
    pub peer: DeviceId,
    pub stage: &'static str,
    pub workspace: Option<koru_store_core::WorkspaceId>,
    pub items: Option<usize>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateChangeEvent {
    pub event_id: Option<i64>,
    pub sequence: u64,
    pub at_millis: i64,
    pub peer: DeviceId,
    pub kind: String,
    pub profile: Option<koru_store_core::ProfileId>,
    pub workspace: Option<koru_store_core::WorkspaceId>,
    pub document: Option<koru_store_core::DocumentId>,
    pub document_name: Option<String>,
    pub blob_hash: Option<String>,
    pub detail: Option<String>,
    pub items: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateChangePage {
    /// Newest first.
    pub events: Vec<StateChangeEvent>,
    pub next_before: Option<i64>,
}

/// Direction of the high-level status event. Change announcements originate
/// from this Store; reconciliation stages describe state pulled from a peer.
pub fn sync_event_direction(stage: &str) -> &'static str {
    if stage.starts_with("change_") {
        "local_to_peer"
    } else {
        "peer_to_local"
    }
}

/// A version vector in a form short enough to log and still compare.
pub(crate) fn vv_tag(version: &[u8]) -> String {
    let hash = blake3::hash(version).to_hex().to_string();
    format!("{}:{}", version.len(), &hash[..8])
}

/// A bounded current-runtime history container used by network hosts.
pub type StateEventQueue = VecDeque<StateChangeEvent>;

#[cfg(test)]
mod boundary_tests {
    #[test]
    fn replication_source_names_no_iroh_type() {
        let source = include_str!("lib.rs");
        let protocol = include_str!("protocol.rs");
        let forbidden = ["iroh", "::"].concat();
        assert!(!source.contains(&forbidden));
        assert!(!protocol.contains(&forbidden));
    }
}
