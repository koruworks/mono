// SPDX-License-Identifier: MPL-2.0

//! Provider- and transport-neutral account vocabulary.
//!
//! This crate describes hosted account metadata, not Koru content. Profiles
//! and devices retain their existing client-minted identifiers; an account
//! binds those identifiers without becoming their canonical store.

pub mod app_connections;
pub mod billing_history;
pub mod cloud_leases;
pub mod cloud_selection;
mod creation;
mod device_ownership;
mod email;
mod ids;
mod model;
mod password;
mod ports;
mod profile_lifecycle;
pub mod regional_bootstrap;
mod resolver;
mod secret;
pub mod subscription;
pub mod sync_issuance;
pub mod workspace_authorization;
pub mod workspace_lifecycle;

pub use creation::*;
pub use device_ownership::*;
pub use email::*;
pub use ids::{
    AccountSessionId, AppConnectionId, AuthFlowId, AuthIdentityId, IdParseError, KoruIdentityId,
};
pub use koru_device::{DeviceDescriptorV1, DeviceId};
pub use koru_store_core::{DocumentId, ProfileId, WorkspaceId};
pub use model::*;
pub use password::*;
pub use ports::*;
pub use profile_lifecycle::*;
pub use resolver::*;
pub use secret::{KeyedDigest, OpaqueSecret, SealedValue};

#[cfg(test)]
mod tests;
