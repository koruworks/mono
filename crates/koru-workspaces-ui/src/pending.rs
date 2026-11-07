// SPDX-License-Identifier: MPL-2.0

//! Correlation and projections for work that has not answered this window yet.

use koru_store_core::ids::{ProfileId, WorkspaceId};

/// Unique within one window controller; never reused after settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandId(pub(crate) u64);

pub(crate) enum Pending {
    Pin {
        workspace: WorkspaceId,
        pinned: bool,
    },
    Order {
        owner: ProfileId,
        ids: Vec<WorkspaceId>,
    },
    Appearance,
    KeepProfile(ProfileId),
    EnsureLive {
        owner: ProfileId,
        attempt: u64,
        reply: std::sync::mpsc::Sender<
            koru_store_core::errors::Result<koru_api_client::EnsuredWorkspace>,
        >,
    },
    CreateProfile(
        std::sync::mpsc::Sender<koru_store_core::errors::Result<koru_api_client::CreatedProfile>>,
    ),
}
