// SPDX-License-Identifier: MPL-2.0

//! **Which speech models exist: the signed descriptor, plus the unsigned facts
//! about where to get it — and the one choice only speech can make.**
//!
//! The catalog's shape and its rules — a flat entry whose descriptor is signed
//! and whose URLs are not, a document that refuses plain HTTP and a repeated
//! identity, a size cap on the bytes fetched — are `koru-models`', and this
//! module applies them rather than restating them: [`Catalog::validate`] is the
//! generic validation of what this catalog projects, and
//! [`CatalogSource::fetch`] reads through the generic source.
//!
//! What stays here is what is voice's:
//!
//! - **where voice's catalog is** — [`DEFAULT_CATALOG_URL`], and
//!   `KORU_VOICE_CATALOG` to override it;
//! - **the typed entry**, whose descriptor speaks `EngineId`, `ModelTier` and
//!   `VoiceLanguage`;
//! - **[`Catalog::choose`]**, which resolves a language and a quality tier to a
//!   model — a question asked in speech's words.
//!
//! ## Forward compatibility, which is not the same as leniency
//!
//! A catalog may list a model whose `schema_version` this build does not know.
//! That entry must not stop the ones it *does* know from being installed, so
//! parsing keeps it and [`Catalog::choose`] skips it.

use crate::descriptor::{DESCRIPTOR_SCHEMA_VERSION, ModelDescriptor, ModelSignature, ModelVersion};
use crate::error::{ModelError, ModelErrorCode};
use crate::tier::ModelTier;
use crate::trust::TrustedKeys;
use koru_voice::{EngineId, ModelId, Requested};
use serde::{Deserialize, Serialize};
use url::Url;

pub use koru_models::catalog::CATALOG_SCHEMA_VERSION;

/// One speech model, as the catalog lists it.
///
/// Grouped by *what the signature covers* rather than listed flat, because that
/// grouping is the security property and a flat struct would make it a
/// comment.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Everything that is signed. Flattened on the wire, so the JSON carries
    /// `id`, `version`, `engine`, … beside `url` and `signature`.
    #[serde(flatten)]
    pub descriptor: ModelDescriptor,
    /// The signature over [`ModelDescriptor::canonical_bytes`] — over the
    /// descriptor, not merely the archive.
    pub signature: ModelSignature,
    /// Where to fetch the archive. **Outside the envelope.**
    pub url: Url,
    /// Further places the same bytes can be fetched from. Also outside the
    /// envelope, and tried in order after [`CatalogEntry::url`].
    #[serde(default)]
    pub mirrors: Vec<Url>,
}

impl CatalogEntry {
    pub fn id(&self) -> &ModelId {
        &self.descriptor.id
    }

    pub fn version(&self) -> &ModelVersion {
        &self.descriptor.version
    }

    pub fn engine(&self) -> &EngineId {
        &self.descriptor.engine
    }

    pub fn tier(&self) -> ModelTier {
        self.descriptor.tier
    }

    /// Every place these bytes can be fetched from, preferred first.
    pub fn sources(&self) -> Vec<&Url> {
        std::iter::once(&self.url)
            .chain(self.mirrors.iter())
            .collect()
    }

    /// **Is this entry from us?** The signature check over the canonical form
    /// of the descriptor, and nothing else — the archive is not involved and
    /// need not exist yet.
    pub fn verify(&self, keys: &TrustedKeys) -> Result<(), ModelError> {
        keys.verify(&self.descriptor, &self.signature)
    }

    /// **What a consent step would put on the screen**, as the thing a grant is
    /// recorded against.
    ///
    /// Built here rather than at the surface so that the sentence somebody
    /// reads and the record that is kept afterwards cannot disagree about what
    /// was offered. Every field of it is inside the signature envelope — the
    /// size, the licence and the attribution are signed for exactly this
    /// reason ([ADR-250](../../../docs/decisions/ADR-226-250.md)): a licence a
    /// catalog can change is a licence nobody agreed to.
    pub fn terms(&self) -> koru_voice_consent::Terms {
        koru_voice_consent::Terms {
            model: self.descriptor.id.as_str().to_owned(),
            version: self.descriptor.version.as_str().to_owned(),
            licence: self.descriptor.licence.as_str().to_owned(),
            attribution: self.descriptor.attribution.clone(),
            download_bytes: self.descriptor.size,
        }
    }

    /// Whether this build understands the shape this entry is written in.
    /// [`Catalog::choose`] uses it to skip a model from the future rather than
    /// refusing the whole catalog.
    pub fn understood(&self) -> bool {
        self.descriptor.schema_version == DESCRIPTOR_SCHEMA_VERSION
    }

    /// The generic entry this one projects — what the generic store installs.
    pub(crate) fn to_model(&self) -> Result<koru_models::CatalogEntry, ModelError> {
        Ok(koru_models::CatalogEntry {
            descriptor: self.descriptor.to_model()?,
            signature: self.signature.clone(),
            url: self.url.clone(),
            mirrors: self.mirrors.clone(),
        })
    }
}

/// The list of speech models a build knows how to offer.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub schema_version: u32,
    pub entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// Parse and structurally validate a catalog document.
    ///
    /// Signatures are **not** checked here. Verification is per entry, at the
    /// moment one is about to be installed, because a catalog whose fiftieth
    /// entry is signed by a retired key should still install the first
    /// forty-nine.
    pub fn parse(bytes: &[u8]) -> Result<Catalog, ModelError> {
        let catalog: Catalog = serde_json::from_slice(bytes)
            .map_err(|e| ModelErrorCode::CatalogInvalid.with(format!("catalog is invalid: {e}")))?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// What is wrong for everybody, as opposed to what is wrong for this build
    /// — the generic catalog's rules, applied to what this one projects.
    pub fn validate(&self) -> Result<(), ModelError> {
        koru_models::Catalog {
            schema_version: self.schema_version,
            entries: self
                .entries
                .iter()
                .map(CatalogEntry::to_model)
                .collect::<Result<_, _>>()?,
        }
        .validate()
    }

    /// The entry naming exactly this model build, if the catalog has one.
    pub fn find(&self, id: &ModelId, version: &ModelVersion) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|e| e.id() == id && e.version() == version)
    }

    /// **Choose a model for a language and a quality tier.**
    ///
    /// This is the resolution `koru_voice::SessionRequest` deliberately does
    /// not do — its doc says so: *"choosing one from a language and a quality
    /// tier is the model store's job, and a session that had to choose would
    /// need a catalog, which would put a downloader under a state machine that
    /// is meant to run on a phone's audio thread."* This is that job.
    ///
    /// Three rules, and each is a refusal to be clever:
    ///
    /// - **The tier is exact.** A `HighAccuracy` request that quietly settles
    ///   for `Compact` gives a person a worse result than they chose and no way
    ///   to find out. A caller that wants a fallback asks twice.
    /// - **`Requested::Auto` matches any model**, because whether a *language*
    ///   can be detected is `EngineInfo::detects_language`'s answer and not a
    ///   property of the weights.
    /// - **Catalog order is the publisher's preference order**, so the first
    ///   match wins. Deterministic, and it does not require inventing a
    ///   comparison over `ModelVersion`, which is an opaque label rather than
    ///   semver.
    pub fn choose(
        &self,
        requested: Requested,
        tier: ModelTier,
        engine: Option<&EngineId>,
    ) -> Option<&CatalogEntry> {
        self.entries.iter().find(|entry| {
            entry.understood()
                && entry.tier() == tier
                && entry.descriptor.covers(requested)
                && engine.is_none_or(|wanted| entry.engine() == wanted)
        })
    }
}

/// Where voice's catalog is published.
///
/// A constant rather than a configuration file: a build that could be pointed
/// at another catalog by anything short of an environment variable set on
/// purpose is a build somebody can be pointed away from. The signature is what
/// makes the *contents* trustworthy, and this is what makes the *default*
/// predictable.
pub const DEFAULT_CATALOG_URL: &str = "https://updates.koru.do/voice/catalog-v1.json";

/// **Where voice's catalog comes from**, resolved once rather than asked at
/// each use.
///
/// Two forms and no third. A path is what a test, an air-gapped install and a
/// `--catalog` on a command line all name; a URL is the product path. Both
/// arrive through `KORU_VOICE_CATALOG`, which is a *path if it names one that
/// exists* and a URL otherwise. The rule and the fetch are
/// [`koru_models::CatalogSource`]'s; this is the same two forms, read into
/// voice's typed catalog.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CatalogSource {
    File(std::path::PathBuf),
    Url(Url),
}

impl CatalogSource {
    /// `KORU_VOICE_CATALOG`, then [`DEFAULT_CATALOG_URL`].
    ///
    /// **Reading the world**, which is why it is a resolved value: a caller
    /// that asked twice could be answered twice, and a download that verified
    /// against one catalog and installed from another would be a hole nobody
    /// meant to open.
    pub fn resolve() -> Result<CatalogSource, ModelError> {
        koru_models::CatalogSource::resolve_from(
            koru_env::value(koru_env::name::KORU_VOICE_CATALOG),
            DEFAULT_CATALOG_URL,
        )
        .map(CatalogSource::from)
    }

    /// The rule with the value passed in, so it is testable without `set_var`
    /// — a process-global write that races every other test in the binary.
    pub fn parse(value: &str) -> Result<CatalogSource, ModelError> {
        koru_models::CatalogSource::parse(value).map(CatalogSource::from)
    }

    /// **Fetch and parse it.** Blocking, and therefore not from a frame loop —
    /// see [`crate::ModelStore`]'s note about which threads may call the world.
    ///
    /// Local-first is not weakened by this existing: nothing calls it while a
    /// model is installed. A catalog is read when somebody is about to be
    /// *offered* a download, which is a moment they asked for.
    pub fn fetch(&self) -> Result<Catalog, ModelError> {
        Catalog::parse(&koru_models::CatalogSource::from(self.clone()).read()?)
    }
}

impl From<koru_models::CatalogSource> for CatalogSource {
    fn from(source: koru_models::CatalogSource) -> CatalogSource {
        match source {
            koru_models::CatalogSource::File(path) => CatalogSource::File(path),
            koru_models::CatalogSource::Url(url) => CatalogSource::Url(url),
        }
    }
}

impl From<CatalogSource> for koru_models::CatalogSource {
    fn from(source: CatalogSource) -> koru_models::CatalogSource {
        match source {
            CatalogSource::File(path) => koru_models::CatalogSource::File(path),
            CatalogSource::Url(url) => koru_models::CatalogSource::Url(url),
        }
    }
}

#[cfg(test)]
mod tests;
