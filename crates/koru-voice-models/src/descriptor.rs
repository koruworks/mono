// SPDX-License-Identifier: MPL-2.0

//! **A speech model's signed descriptor, in speech's own words.**
//!
//! The descriptor, its canonical bytes and every rule about them live in
//! [`koru_models::descriptor`], which a model that is not a speech model is
//! described with too. There, `engine`, `tier` and `languages` are validated
//! tokens; here they are [`EngineId`], [`ModelTier`] and [`VoiceLanguage`],
//! because voice's worker, its engine adapters and its catalog chooser branch on
//! those types. This module is the map between the two, and nothing else:
//!
//! - [`ModelDescriptor::to_model`] writes each typed field as the token it has
//!   always been signed as — `EngineId::as_str`, `ModelTier::as_str`,
//!   `VoiceLanguage::tag` — so [`ModelDescriptor::canonical_bytes`] is the
//!   generic form's bytes, which are the bytes every speech descriptor has
//!   ever been signed over. `descriptor/projection_tests.rs` builds the same
//!   descriptor both ways for every vector this crate has and compares them,
//!   against the canonicalisation as it was written before the extraction.
//! - [`ModelDescriptor::from_model`] reads tokens back, and refuses a token
//!   speech has no word for — a tier that is not `compact` or
//!   `high_accuracy`, a language that is not one of the eight — rather than
//!   guessing one. A model that is not a speech model is therefore never a
//!   speech model by accident.
//!
//! The struct and its JSON are exactly what they were, so a catalog, a
//! `.koru-model.json` and every caller that builds one by hand are unchanged.

use crate::error::{ModelError, ModelErrorCode};
use crate::tier::ModelTier;
use koru_models::{LanguageTag, Token};
use koru_voice::{EngineId, ModelId, VoiceLanguage};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::Path;

pub use koru_models::descriptor::{
    DESCRIPTOR_SCHEMA_VERSION, LicenceRef, ModelSignature, ModelVersion, Sha256Digest,
};

/// **Everything about a speech model that is signed**, typed for speech.
///
/// Not the archive, and not the digest alone. Verifying an entry is one
/// signature check over these fields followed by one digest check over the
/// bytes: the signature says *this is what we published*, the digest says
/// *these are those bytes*.
///
/// This is also what is written beside the installed bytes at install time, so
/// that loading a model needs no catalog, no `HEAD` and no network — offline is
/// the ordinary state of this crate rather than a degraded one.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ModelDescriptor {
    /// Which canonical form these fields are in. Checked before anything is
    /// canonicalised or verified.
    pub schema_version: u32,
    pub id: ModelId,
    pub version: ModelVersion,
    /// Which runtime these bytes are for. **Signed**, because verified bytes
    /// handed to the wrong native parser is a crash at best and a parser
    /// exploit at worst.
    pub engine: EngineId,
    pub tier: ModelTier,
    /// What this model can transcribe. Non-empty, and without duplicates.
    pub languages: Vec<VoiceLanguage>,
    /// The archive's length in bytes.
    pub size: u64,
    /// The archive's SHA-256, which is also the last component of the installed
    /// path.
    pub digest: Sha256Digest,
    /// **What a device needs to run it, declared rather than measured.**
    ///
    /// Milestone 4's device gate is a *pre-download* decision — a phone cannot
    /// measure a model it has not downloaded — so this number is the only
    /// evidence it has, which is exactly why it is inside the envelope.
    pub working_set_bytes: u64,
    /// The SPDX identifier the weights are published under.
    pub licence: LicenceRef,
    /// Who to credit, as the licence requires. Free text, and the only field
    /// escaped in the canonical form.
    pub attribution: String,
}

impl ModelDescriptor {
    /// **The generic descriptor this one projects** — the same fields, with
    /// `engine`, `tier` and `languages` written as the tokens they are signed
    /// as.
    ///
    /// Fallible only in its type: every speech identity is a legal token. The
    /// error is `descriptor_invalid`, which is what verifying a value the
    /// generic rules refuse would answer anyway.
    pub fn to_model(&self) -> Result<koru_models::ModelDescriptor, ModelError> {
        Ok(koru_models::ModelDescriptor {
            schema_version: self.schema_version,
            id: koru_models::ModelId::parse(self.id.as_str())?,
            version: self.version.clone(),
            engine: Token::parse(self.engine.as_str())?,
            tier: Token::parse(self.tier.as_str())?,
            languages: self
                .languages
                .iter()
                .map(|language| LanguageTag::parse(language.tag()))
                .collect::<Result<_, _>>()?,
            size: self.size,
            digest: self.digest.clone(),
            working_set_bytes: self.working_set_bytes,
            licence: self.licence.clone(),
            attribution: self.attribution.clone(),
        })
    }

    /// **The speech descriptor a generic one names**, or a refusal when it
    /// names something speech has no word for.
    ///
    /// Exact, never lenient: a language tag must be one of the eight spelled
    /// as `VoiceLanguage` spells it, and a tier must be `ModelTier`'s own
    /// token, so a descriptor round-trips to the bytes it came from.
    pub fn from_model(model: koru_models::ModelDescriptor) -> Result<ModelDescriptor, ModelError> {
        let refuse = |what: String| Err(ModelErrorCode::DescriptorInvalid.with(what));
        let Some(tier) = ModelTier::parse(model.tier.as_str()) else {
            return refuse(format!("tier {} is not a speech tier", model.tier));
        };
        let mut languages = Vec::with_capacity(model.languages.len());
        for tag in &model.languages {
            match VoiceLanguage::ALL
                .into_iter()
                .find(|language| language.tag() == tag.as_str())
            {
                Some(language) => languages.push(language),
                None => return refuse(format!("language {tag} is not one voice transcribes")),
            }
        }
        let id = ModelId::parse(model.id.as_str()).map_err(|e| {
            ModelErrorCode::DescriptorInvalid.with(format!("model id: {}", e.message))
        })?;
        let engine = EngineId::parse(model.engine.as_str()).map_err(|e| {
            ModelErrorCode::DescriptorInvalid.with(format!("engine: {}", e.message))
        })?;
        Ok(ModelDescriptor {
            schema_version: model.schema_version,
            id,
            version: model.version,
            engine,
            tier,
            languages,
            size: model.size,
            digest: model.digest,
            working_set_bytes: model.working_set_bytes,
            licence: model.licence,
            attribution: model.attribution,
        })
    }

    /// Check every field's own rule and the relationships between them — the
    /// generic descriptor's rules, applied to what this one projects.
    pub fn validate(&self) -> Result<(), ModelError> {
        self.to_model()?.validate()
    }

    /// **The bytes that are signed** — the generic canonical form of what this
    /// descriptor projects, which is byte for byte the form speech descriptors
    /// have always been signed over. See [`koru_models::ModelDescriptor::canonical_bytes`].
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ModelError> {
        self.to_model()?.canonical_bytes()
    }

    /// The last three components of an installed model's path, in order.
    ///
    /// `<id>/<version>/<digest>` — cache identity is the digest (ADR-250), and
    /// all three are already validated as path components by the types that
    /// hold them, which is why this can hand back `&str` rather than a
    /// `Result`.
    pub fn path_components(&self) -> [&str; 3] {
        [
            self.id.as_str(),
            self.version.as_str(),
            self.digest.as_str(),
        ]
    }

    /// Whether this model can transcribe what a session asked for.
    ///
    /// [`Requested::Auto`](koru_voice::Requested) is satisfied by any model,
    /// because deciding whether the *engine* can detect a language is
    /// [`EngineInfo::detects_language`](koru_voice::EngineInfo)'s answer and
    /// not a property of the bytes.
    pub fn covers(&self, requested: koru_voice::Requested) -> bool {
        match requested.fixed() {
            None => true,
            Some(language) => self.languages.contains(&language),
        }
    }

    /// **The second half of verification: are these the bytes we signed for?**
    /// Length first, then the streaming hash — see
    /// [`koru_models::ModelDescriptor::verify_archive`].
    pub fn verify_archive(&self, path: &Path) -> Result<(), ModelError> {
        self.to_model()?.verify_archive(path)
    }
}

/// What lets [`crate::TrustedKeys::verify`] check a speech descriptor: the
/// signature is verified over the generic descriptor it projects.
impl koru_models::AsDescriptor for ModelDescriptor {
    fn as_descriptor(&self) -> Result<Cow<'_, koru_models::ModelDescriptor>, ModelError> {
        self.to_model().map(Cow::Owned)
    }
}

/// The extraction's proof: the same descriptor built through the typed path
/// and the token path canonicalises to the same bytes as before, and a
/// signature made before the change still verifies.
#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod tests;
