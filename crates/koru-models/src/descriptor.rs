// SPDX-License-Identifier: MPL-2.0

//! **What is signed: every field of a model that changes behaviour, consent or
//! licence compliance, in a byte sequence two implementations can produce.**
//!
//! The tempting envelope is the digest, and it is too small. A catalog entry
//! carries fields that do real harm without a single byte of the archive
//! changing: an unsigned `engine` routes verified bytes into the wrong native
//! parser, an unsigned `working_set_bytes` understates what a phone needs and
//! defeats a device gate, an unsigned `languages` changes what the product
//! claims to support, and an unsigned `attribution` changes what a licence
//! notice says about somebody else's work.
//! [ADR-250](../../../docs/decisions/ADR-226-250.md) argues each of those; this
//! module is the shape it settled on.
//!
//! ## Why the canonical form is written by hand
//!
//! *Whatever this `serde_json` emitted* is a specification only for programs
//! that link this `serde_json`. The signing tool will one day not be this
//! program — a release pipeline, a third party signing their own weights — and
//! at that point the signed bytes have to be something a person can implement
//! from a paragraph. So [`ModelDescriptor::canonical_bytes`] is a fixed-order,
//! LF-terminated, tab-separated line format, and its first line names the
//! schema version so a reader knows before parsing whether it understands what
//! follows.
//!
//! ```text
//! koru-model-descriptor/1
//! id\tparakeet-tdt-v3
//! version\t1.2.0
//! engine\tsherpa
//! tier\tcompact
//! languages\ten,es,pt-BR,de,fr,ar,zh-Hans,ja
//! size\t648291840
//! digest\t9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
//! working_set_bytes\t1073741824
//! licence\tApache-2.0
//! attribution\tk2-fsa, Apache-2.0
//! ```
//!
//! Only `attribution` is free text, so only `attribution` is escaped —
//! backslash, newline, carriage return and tab. Every other field's own charset
//! rule already excludes the separators, which is why those rules are enforced
//! at construction and not at signing time: a value that cannot hold a tab
//! cannot smuggle a field boundary into the envelope.
//!
//! ## `engine`, `tier` and `languages` are tokens, and the bytes are the same
//!
//! This form was first written in a speech crate, with those three fields as
//! speech types. They are [`Token`]s and [`LanguageTag`]s here so a model that
//! is not a speech model can be described, and **the bytes each one
//! contributes are exactly the string the speech type wrote** — so every
//! descriptor signed before the extraction verifies after it, with no
//! re-signing. `koru-voice-models` holds the test that builds the same
//! descriptor both ways and compares the bytes.
//!
//! ## The one rule about a version this build does not know
//!
//! It is refused, and it is refused **before** any signature is checked.
//! Canonicalising a shape we do not understand produces bytes the signer never
//! signed, so the failure would arrive as
//! [`SignatureInvalid`](crate::ModelErrorCode::SignatureInvalid) — an attack
//! report for what is only an old build.

use crate::error::{ModelError, ModelErrorCode};
use crate::token::{LanguageTag, ModelId, Token, identifier};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::Path;

/// The version of the canonical form in [`ModelDescriptor::canonical_bytes`].
///
/// A new field is a bump, a re-sign of every catalog entry, and a build that
/// answers [`ModelErrorCode::SchemaUnsupported`] until it is updated. That cost
/// is why the field list was argued through in ADR-250 rather than grown one
/// need at a time.
pub const DESCRIPTOR_SCHEMA_VERSION: u32 = 1;

/// The first line of the canonical form, without its version.
const CANONICAL_MAGIC: &str = "koru-model-descriptor";

/// Which build of a model — `1.2.0`, `2026-03-int8`.
///
/// **A path component**, so its charset is checked rather than assumed:
/// installed bytes live under `<id>/<version>/<digest>/`. The rule is
/// [`ModelId`]'s, applied by the same function rather than restated.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelVersion(String);

impl ModelVersion {
    /// Check and wrap. The only constructor, for the reason `ModelId` gives:
    /// an unchecked `From<String>` would be the shortest path and would
    /// therefore be the one used.
    pub fn parse(value: impl Into<String>) -> Result<ModelVersion, ModelError> {
        let value = value.into();
        identifier("a model version", &value)?;
        Ok(ModelVersion(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelVersion {
    type Error = ModelError;

    fn try_from(value: String) -> Result<ModelVersion, ModelError> {
        ModelVersion::parse(value)
    }
}

impl From<ModelVersion> for String {
    fn from(value: ModelVersion) -> String {
        value.0
    }
}

impl std::fmt::Display for ModelVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A SHA-256, as 64 lowercase hex characters.
///
/// **The cache identity** (ADR-250), so it is also a path component — which is
/// free here, because 64 hex characters are legal on every filesystem and
/// identical in every case-folding. Lowercase is enforced rather than accepted
/// so that one set of bytes has exactly one directory name.
///
/// Held as a `String` rather than `[u8; 32]` deliberately: it is compared,
/// logged and turned into a path far more often than it is hashed, and
/// `koru_update::sha256_file` — the function that produces it —
/// already answers lowercase hex.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> Result<Sha256Digest, ModelError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ModelErrorCode::DescriptorInvalid
                .with("a digest is not 64 hexadecimal characters".to_string()));
        }
        if value.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(ModelErrorCode::DescriptorInvalid
                .with("a digest is not lowercase; one set of bytes has one directory name"));
        }
        Ok(Sha256Digest(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Hash a file, answering its length beside its digest.
    ///
    /// **`koru_update::sha256_file`, not a second streamer.** The length comes
    /// back for free and is what [`ModelDescriptor::verify_archive`] checks
    /// first, because a disagreeing length names a truncated download rather
    /// than a substituted one.
    pub fn of_file(path: &Path) -> Result<(u64, Sha256Digest), ModelError> {
        let (size, hex) = koru_update::sha256_file(path).map_err(|e| {
            ModelErrorCode::Io.with(format!("could not hash {}: {e}", path.display()))
        })?;
        Ok((size, Sha256Digest::parse(hex)?))
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = ModelError;

    fn try_from(value: String) -> Result<Sha256Digest, ModelError> {
        Sha256Digest::parse(value)
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> String {
        value.0
    }
}

impl std::fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An SPDX licence identifier — `Apache-2.0`, `CC-BY-4.0`, `LicenseRef-…`.
///
/// **Not a path component**, so uppercase is kept: SPDX identifiers are
/// case-sensitive in practice and lowercasing `Apache-2.0` would produce a
/// string that names no licence. What is enforced is that it is short, ASCII,
/// and cannot hold a canonical-form separator.
///
/// It is inside the signature envelope because a licence a catalog can change
/// is a licence nobody agreed to, and because it is what a *Third-party
/// notices* surface will list.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LicenceRef(String);

impl LicenceRef {
    pub fn parse(value: impl Into<String>) -> Result<LicenceRef, ModelError> {
        let value = value.into();
        let bad = |why: &str| {
            Err(ModelErrorCode::DescriptorInvalid.with(format!("a licence reference {why}")))
        };
        if value.is_empty() {
            return bad("is empty");
        }
        if value.chars().count() > 64 {
            return bad("is longer than 64 characters");
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+' | '_'))
        {
            return bad("is not ASCII letters, digits, '-', '.', '+' or '_'");
        }
        Ok(LicenceRef(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LicenceRef {
    type Error = ModelError;

    fn try_from(value: String) -> Result<LicenceRef, ModelError> {
        LicenceRef::parse(value)
    }
}

impl From<LicenceRef> for String {
    fn from(value: LicenceRef) -> String {
        value.0
    }
}

impl std::fmt::Display for LicenceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A Minisign signature block, as it appears in a `.minisig` file.
///
/// A newtype rather than a bare `String` so that the thing a caller passes to
/// [`crate::TrustedKeys::verify`] cannot be a URL, a digest or an attribution
/// line by accident. It is deliberately *not* validated beyond being non-empty:
/// whether it decodes is [`minisign_verify::Signature::decode`]'s answer, and
/// pre-judging it here would be a second parser.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelSignature(String);

impl ModelSignature {
    pub fn parse(value: impl Into<String>) -> Result<ModelSignature, ModelError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ModelErrorCode::SignatureMalformed.with("the signature is empty"));
        }
        Ok(ModelSignature(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelSignature {
    type Error = ModelError;

    fn try_from(value: String) -> Result<ModelSignature, ModelError> {
        ModelSignature::parse(value)
    }
}

impl From<ModelSignature> for String {
    fn from(value: ModelSignature) -> String {
        value.0
    }
}

/// **Everything about a model that is signed.**
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
    /// exploit at worst — and checked at load by [`crate::LoadedModel::open`].
    pub engine: Token,
    /// The feature's quality choice for this model — voice's `compact` or
    /// `high_accuracy`. Never a model name.
    pub tier: Token,
    /// What this model can read or transcribe. Non-empty, and without
    /// duplicates.
    pub languages: Vec<LanguageTag>,
    /// The archive's length in bytes.
    pub size: u64,
    /// The archive's SHA-256, which is also the last component of the installed
    /// path.
    pub digest: Sha256Digest,
    /// **What a device needs to run it, declared rather than measured.**
    ///
    /// A device gate is a *pre-download* decision — a phone cannot measure a
    /// model it has not downloaded — so this number is the only evidence it
    /// has, which is exactly why it is inside the envelope.
    pub working_set_bytes: u64,
    /// The SPDX identifier the weights are published under.
    pub licence: LicenceRef,
    /// Who to credit, as the licence requires. Free text, and the only field
    /// escaped in the canonical form.
    pub attribution: String,
}

impl ModelDescriptor {
    /// Check every field's own rule and the relationships between them.
    ///
    /// The newtypes have already refused a bad `id`, `version`, token, tag,
    /// `digest` or `licence` at construction; what is left is what no single
    /// field can know — an unknown schema version, an empty or duplicated
    /// language list, a zero size, an attribution carrying a control character
    /// the canonical escape does not name.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema_version != DESCRIPTOR_SCHEMA_VERSION {
            return Err(ModelErrorCode::SchemaUnsupported.with(format!(
                "descriptor schema {} is not {DESCRIPTOR_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        let bad = |why: String| Err(ModelErrorCode::DescriptorInvalid.with(why));
        if self.languages.is_empty() {
            return bad("a model declares no languages".into());
        }
        for (at, language) in self.languages.iter().enumerate() {
            if self.languages[..at].contains(language) {
                return bad(format!("language {language} is declared twice"));
            }
        }
        if self.size == 0 {
            return bad("a model archive of zero bytes".into());
        }
        if self.working_set_bytes == 0 {
            return bad("a working set of zero bytes".into());
        }
        if self.attribution.trim().is_empty() {
            return bad("a model with no attribution".into());
        }
        if let Some(c) = self
            .attribution
            .chars()
            .find(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return bad(format!(
                "attribution carries control character U+{:04X}",
                c as u32
            ));
        }
        Ok(())
    }

    /// **The bytes that are signed.**
    ///
    /// Fixed order, LF endings, tab between a field name and its value, and a
    /// header line naming the schema version. Deterministic by construction
    /// rather than by trusting a serializer's field order, because the signer
    /// will one day not be this program.
    ///
    /// Refuses an unknown schema version rather than canonicalising a shape it
    /// does not understand — see the module note: doing otherwise turns *your
    /// build is old* into *somebody tampered with this*.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ModelError> {
        if self.schema_version != DESCRIPTOR_SCHEMA_VERSION {
            return Err(ModelErrorCode::SchemaUnsupported.with(format!(
                "cannot canonicalise descriptor schema {}; this build knows {DESCRIPTOR_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        let languages = self
            .languages
            .iter()
            .map(LanguageTag::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let mut out = String::new();
        out.push_str(CANONICAL_MAGIC);
        out.push('/');
        out.push_str(&self.schema_version.to_string());
        out.push('\n');
        let mut line = |name: &str, value: &str| {
            out.push_str(name);
            out.push('\t');
            out.push_str(value);
            out.push('\n');
        };
        line("id", self.id.as_str());
        line("version", self.version.as_str());
        line("engine", self.engine.as_str());
        line("tier", self.tier.as_str());
        line("languages", &languages);
        line("size", &self.size.to_string());
        line("digest", self.digest.as_str());
        line("working_set_bytes", &self.working_set_bytes.to_string());
        line("licence", self.licence.as_str());
        line("attribution", &escape(&self.attribution));
        Ok(out.into_bytes())
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

    /// **The second half of verification: are these the bytes we signed for?**
    ///
    /// Length first — it is answerable from metadata and a disagreement names a
    /// truncated download rather than a substituted one — then the streaming
    /// hash. This says nothing about the signature; that is
    /// [`crate::TrustedKeys::verify`]'s job, and it runs *first*, because
    /// hashing 650 MB to discover the descriptor was forged is work nobody
    /// needed to do.
    pub fn verify_archive(&self, path: &Path) -> Result<(), ModelError> {
        let (size, digest) = Sha256Digest::of_file(path)?;
        if size != self.size {
            return Err(ModelErrorCode::SizeMismatch.with(format!(
                "{} is {size} bytes, the descriptor says {}",
                path.display(),
                self.size
            )));
        }
        if digest != self.digest {
            return Err(ModelErrorCode::DigestMismatch.with(format!(
                "{} hashes to {digest}, the descriptor says {}",
                path.display(),
                self.digest
            )));
        }
        Ok(())
    }
}

/// **A feature's typed view of a descriptor, and the bytes it signs as.**
///
/// A feature crate keeps its own descriptor type — `koru-voice-models` has one
/// whose `engine` is an `EngineId` — and this is how that type is verified
/// without a second canonical form: it answers the [`ModelDescriptor`] it
/// projects, and the signature is checked over *that*. One implementation of
/// the signed bytes, however many typed views of them.
///
/// Fallible because a projection may hold a value this crate's rules refuse;
/// the error is the one verification would have answered.
pub trait AsDescriptor {
    fn as_descriptor(&self) -> Result<Cow<'_, ModelDescriptor>, ModelError>;
}

impl AsDescriptor for ModelDescriptor {
    fn as_descriptor(&self) -> Result<Cow<'_, ModelDescriptor>, ModelError> {
        Ok(Cow::Borrowed(self))
    }
}

/// Escape the one free-text field. Backslash first, or the escapes escape each
/// other.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
