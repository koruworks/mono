// SPDX-License-Identifier: MPL-2.0

//! **Which models exist, where their verified bytes are on this machine, and
//! how they get there — for any feature that downloads one.**
//!
//! This is the mechanism voice dictation has installed speech models with since
//! its first release, extracted from `koru-voice-models` so that a second kind
//! of model — the one that reads text out of pictures — is installed by the
//! same code rather than a copy of it. `koru-voice-models` is now a typed
//! projection over this crate; nothing voice does changed.
//!
//! It knows **no engine, no audio, no channel and no feature.** It hands out a
//! directory and a signed descriptor; loading them into a runtime is an engine
//! adapter's job. A crate that linked an inference runtime could not be
//! depended on by a settings page, and one that knew which feature it served
//! could not serve a second.
//!
//! ## What a consumer supplies, and what it gets
//!
//! | supplies | as |
//! | --- | --- |
//! | where its catalog is | [`CatalogSource::resolve_from`] — its own variable's value, and its own default URL |
//! | where its cache is | [`ModelPaths::at`] — a root it resolved from its own variable, `--root`, or [`paths::local_data_base`] |
//! | whose consent opens its store | a [`koru_voice_consent::ConsentFile`] — `VOICE`, `READ` |
//! | how much a model must prove | a [`TrustPolicy`], chosen once where the store is built |
//!
//! | gets | |
//! | --- | --- |
//! | [`ModelDescriptor`] | every field that is **signed**, and the canonical bytes it signs as; `engine`, `tier` and `languages` are [`Token`]s and [`LanguageTag`]s the consumer maps to its own types |
//! | [`AsDescriptor`] | how a consumer's typed descriptor is verified without a second canonical form |
//! | [`CatalogEntry`] / [`Catalog`] | the descriptor, plus the unsigned facts about where to get it |
//! | [`TrustedKeys`] / [`TrustPolicy`] | the pinned model-signing key, and the guarded, loud, pre-key second answer |
//! | [`ModelStore`] / [`ModelLease`] / [`LoadedModel`] | install, lease, load |
//! | [`DiskModelStore`] | atomic publish, a per-digest cross-process install lock, leases, prune, remove, crash recovery |
//! | [`HttpSource`] | the resumable downloader: a digest-keyed `.part`, `If-Range` with `Content-Range` validation, explicit 200 / 206 / 416, coalesced progress |
//! | [`ModelErrorCode`] | one closed list of stable tokens; a consumer maps it into its own vocabulary |
//!
//! ## Four things it is easy to get wrong
//!
//! **A digest is not a trust root.** It is delivered by the same catalog as the
//! URL, so whoever can serve a bad model can serve its matching digest. The
//! signature is over a canonical [`ModelDescriptor`] covering every
//! behaviour-bearing field, and
//! [ADR-250](../../../docs/decisions/ADR-226-250.md#adr-250-voicemodels--models-are-signed-by-one-pinned-model-signing-key-over-a-canonical-descriptor-and-the-cache-is-keyed-by-the-digest)
//! argues field by field why each one is inside the envelope.
//!
//! **The signed bytes did not move when the code did.** The three fields that
//! became tokens contribute exactly the strings they contributed before, so a
//! descriptor signed before the extraction verifies after it. A new field, or a
//! changed spelling, is a schema bump and a re-sign of every catalog — never an
//! edit.
//!
//! **`MODEL_SIGNING_KEYS` is empty.** Minting the key is an operations decision
//! nobody has taken, so under [`TrustPolicy::SIGNED`] every install is refused
//! with [`ModelErrorCode::NoTrustRoot`]; nightly and beta use the guarded
//! digest-only policy, which a stable build cannot reach. Every verification
//! vector here is built from a *generated* key pair.
//!
//! **Consent is per feature.** A store is built naming its feature's consent
//! record and refuses a grant from any other: `Granted::covers` compares only a
//! licence and a size, and a speech model somebody agreed to is not a reader
//! they agreed to.
//!
//! ## What it deliberately does not do
//!
//! No inference, no audio, no engine loading, no channel, no environment
//! variable, no UI and no translated string — the codes here are branched on,
//! and a surface looks up its own words in `koru-i18n`.

pub mod catalog;
pub mod descriptor;
pub mod disk;
pub mod error;
pub mod paths;
pub mod source;
pub mod store;
pub mod token;
pub mod trust;

/// The cross-process locking, leasing and staging primitives. Private on
/// purpose: they are a *mechanism*, and the guarantees they exist for —
/// atomic publish, one install per digest, a lease a `prune` cannot ignore —
/// are [`DiskModelStore`]'s to make.
mod lock;
/// Progress coalescing. Private for the same reason: the rule is a promise
/// [`InstallProgress`] makes on behalf of everything that emits one.
mod progress;

pub use catalog::{CATALOG_SCHEMA_VERSION, Catalog, CatalogEntry, CatalogSource};
pub use descriptor::{
    AsDescriptor, DESCRIPTOR_SCHEMA_VERSION, LicenceRef, ModelDescriptor, ModelSignature,
    ModelVersion, Sha256Digest,
};
pub use disk::{DiskModelStore, InstalledMetadata, METADATA_FILE, METADATA_SCHEMA_VERSION};
pub use error::{ModelError, ModelErrorCode};
pub use paths::ModelPaths;
pub use source::{DirectorySource, HttpSource, ModelSource, NoNetworkSource};
pub use store::{
    InstallProgress, InstallStage, InstalledModel, LoadedModel, ModelLease, ModelStore,
    anything_installed, file_inside,
};
pub use token::{LanguageTag, ModelId, Token};
pub use trust::{MODEL_SIGNING_KEYS, TrustPolicy, TrustedKeys};

#[cfg(test)]
mod fixtures;
/// A local fixture HTTP server. **No test in this crate reaches the network**,
/// and this is what makes that true of the resume suite as well as of the rest.
#[cfg(test)]
mod testserver;
