// SPDX-License-Identifier: MPL-2.0

//! **Which speech models exist, where their verified bytes are on this
//! machine, and how they get there — in speech's own words.**
//!
//! The mechanism — the signed descriptor and its canonical bytes, the catalog
//! document, the trust root, the resumable download, and the disk store with
//! its locks, leases and atomic publish — is [`koru_models`], extracted from
//! this crate so that a second kind of model (the one that reads text out of
//! pictures) is installed by the same code. **This crate is the typed
//! projection over it**, and nothing voice does changed in the move: the same
//! catalog address and `KORU_VOICE_CATALOG`, the same cache and
//! `KORU_VOICE_MODELS`, the same files on disk, the same signed bytes, the same
//! consent file, and the same public names.
//!
//! What is voice's, and therefore here:
//!
//! | | |
//! | --- | --- |
//! | [`ModelDescriptor`] | the signed descriptor with `engine: EngineId`, `tier: ModelTier`, `languages: Vec<VoiceLanguage>`; it maps to and from the generic descriptor's tokens, and its canonical bytes are the generic form's |
//! | [`CatalogEntry`] / [`Catalog`] | the typed catalog, and [`Catalog::choose`] — a language and a tier to a model |
//! | [`CatalogSource`] | `KORU_VOICE_CATALOG`, then [`DEFAULT_CATALOG_URL`] |
//! | [`ModelPaths`] | `KORU_VOICE_MODELS`, then `--root`'s `voice/models`, then the channel-independent default |
//! | [`ModelStore`] / [`ModelLease`] / [`LoadedModel`] | install, lease, load, in voice's types — the interfaces the worker consumes |
//! | [`DiskModelStore`] | the generic disk store, opened only by a grant from `voice-consent.conf` |
//! | [`choose_installed`] / [`anything_installed`] | what can be used right now, derived from the disk |
//! | [`SessionCodeExt`] / [`SessionErrorExt`] | the one total mapping from the store's codes into the session's |
//!
//! And re-exported from [`koru_models`] so voice's callers name them as they
//! always have: [`ModelVersion`], [`Sha256Digest`], [`LicenceRef`],
//! [`ModelSignature`], [`TrustedKeys`], [`TrustPolicy`], [`MODEL_SIGNING_KEYS`],
//! [`ModelError`], [`ModelErrorCode`], [`InstallProgress`], [`InstallStage`],
//! [`HttpSource`], [`DirectorySource`], [`NoNetworkSource`],
//! [`InstalledMetadata`]; and [`ModelTier`] from `koru-voice`, as before.
//!
//! ## What it deliberately does not do
//!
//! No inference, no audio, no engine loading. No channel: the model cache is
//! channel-independent. No UI and no translated string — the codes here are
//! branched on, and a surface looks up its own words in `koru-i18n`.

pub mod catalog;
pub mod descriptor;
pub mod disk;
pub mod error;
pub mod paths;
pub mod source;
pub mod store;
pub mod tier;
pub mod trust;

pub use catalog::{
    CATALOG_SCHEMA_VERSION, Catalog, CatalogEntry, CatalogSource, DEFAULT_CATALOG_URL,
};
pub use descriptor::{
    DESCRIPTOR_SCHEMA_VERSION, LicenceRef, ModelDescriptor, ModelSignature, ModelVersion,
    Sha256Digest,
};
pub use disk::{DiskModelStore, InstalledMetadata, METADATA_FILE, METADATA_SCHEMA_VERSION};
pub use error::{ModelError, ModelErrorCode, SessionCodeExt, SessionErrorExt};
pub use paths::ModelPaths;
pub use source::{DirectorySource, HttpSource, ModelSource, NoNetworkSource};
pub use store::{
    InstallProgress, InstallStage, InstalledModel, LoadedModel, ModelLease, ModelStore,
    anything_installed, choose_installed,
};
pub use tier::ModelTier;
pub use trust::{MODEL_SIGNING_KEYS, TrustPolicy, TrustedKeys};

#[cfg(test)]
mod fixtures;
