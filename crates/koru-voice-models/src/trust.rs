// SPDX-License-Identifier: MPL-2.0

//! **The trust root**, which is `koru-models`' — one pinned model-signing key
//! for every model Koru installs, and the guarded digest-only policy nightly
//! and beta use while that key does not exist. Re-exported here so voice's
//! callers name it as they always have.
//!
//! [`TrustedKeys::verify`] takes a speech [`ModelDescriptor`](crate::ModelDescriptor)
//! as it always did: the descriptor answers the generic descriptor it projects
//! (`koru_models::AsDescriptor`), and the signature is checked over that one
//! canonical form. [`koru_models::trust`]'s note is where the reasoning about
//! the key, rotation and the stable-build guard lives.
//!
//! The guard reads `KORU_RELEASE_CHANNEL` when **`koru-models`** compiles, and
//! `koru-release` builds every package in one `cargo` invocation under one
//! environment, so a stable build of voice still carries the refusal with no
//! cooperation from any caller.

pub use koru_models::trust::{MODEL_SIGNING_KEYS, TrustPolicy, TrustedKeys};

// The suite beside this module predates the extraction and names what it
// verifies through `super::*`; these are those names.
#[cfg(test)]
use crate::{
    descriptor::{ModelDescriptor, ModelSignature},
    error::ModelErrorCode,
};

#[cfg(test)]
mod tests;
