// SPDX-License-Identifier: MPL-2.0

//! **Stable codes for everything the model store can refuse.**
//!
//! `{ code, message }` again — the convention
//! [PRINCIPLES](../../../guide/contributing/PRINCIPLES.md) sets for every API in this
//! product. `code` is branched on and logged; `message` is untranslated
//! developer text nobody may show a person.
//!
//! **No feature's vocabulary is here.** A feature that runs a model has its own
//! closed list of what it tells *its* callers — voice's session `ErrorCode`, the
//! reader's own — and translates these into it once, in its own crate, with a
//! `match` that has no wildcard: `koru-voice-models`' `SessionCodeExt` is
//! voice's. Installing a model fails in ways no session has words for — a
//! signature from a key nobody pinned, a catalog reusing an id, a download
//! somebody cancelled — which is why this list is separate from every one of
//! them.

use serde::{Deserialize, Serialize};

/// Every way the model store can refuse or fail, as one closed list.
///
/// Lowercase ASCII with underscores: these strings are matched by a caller,
/// printed in a log, and one day used as a `koru-i18n` key.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelErrorCode {
    /// This build pins no model-signing key, so nothing can be verified and
    /// therefore nothing may be installed.
    ///
    /// **Distinct from every other signature failure on purpose.** The
    /// alternative — quietly checking the digest alone — is exactly the
    /// degradation [ADR-250](../../../docs/decisions/ADR-226-250.md) refuses,
    /// and it would be invisible. Saying `no_trust_root` is how a build without
    /// a key is a build that says so.
    NoTrustRoot,
    /// The signature is not a Minisign signature at all — it did not decode.
    SignatureMalformed,
    /// The signature decoded and names a key this build does not pin.
    ///
    /// Minisign compares key ids before it looks at any bytes, so this is
    /// answerable without knowing whether the signature would otherwise have
    /// verified. It is the code that means *you are probably running an old
    /// build*, which is why it is not folded into
    /// [`ModelErrorCode::SignatureInvalid`].
    SignatureUnknownKey,
    /// A key we pin made this signature, and it does not verify over this
    /// descriptor. The code that means *somebody changed something*.
    SignatureInvalid,
    /// The descriptor declares a `schema_version` this build does not know.
    ///
    /// Refused rather than verified against, because canonicalising a shape we
    /// do not understand would produce bytes the signer never signed, and the
    /// failure would arrive as [`ModelErrorCode::SignatureInvalid`] — an attack
    /// report for what is only an old build.
    SchemaUnsupported,
    /// A descriptor field fails its own rule: an id that is not a legal path
    /// component, a digest that is not 64 lowercase hex characters, an empty
    /// language list, a zero size.
    DescriptorInvalid,
    /// The catalog itself is malformed, or carries a URL that is not `https`.
    CatalogInvalid,
    /// **The catalog's address answered, and not with a catalog.** Any HTTP
    /// status that is not a success -- a 404 because nothing is published
    /// there yet, a 403, a 500.
    ///
    /// **Split from [`ModelErrorCode::CatalogInvalid`] because the two send a
    /// person to different places.** *Malformed* means the file we fetched is
    /// wrong and there is nothing they can do; *not published* means the
    /// address is answering and there is simply no catalog on it, which is
    /// what a build pointed at a host with nothing on it hits — and the first
    /// thing somebody who overrode a feature's catalog address should check.
    /// Both used to be `catalog_invalid`, and both used to become one sentence
    /// that said neither.
    CatalogUnavailable,
    /// The bytes hash to something other than the signed digest.
    DigestMismatch,
    /// The bytes are a different length than the signed size. Checked
    /// separately from the digest because it is answerable without reading the
    /// file, and because a length that disagrees names a truncated download
    /// rather than a substituted one.
    SizeMismatch,
    /// A catalog entry reuses an `id`/`version` already installed with
    /// *different* bytes. A hard failure, never a silent replace — see
    /// ADR-250.
    IdentityConflict,
    /// The model was built for another engine than the one asking to load it.
    /// The failure an unsigned `engine` field would have made silent.
    EngineMismatch,
    /// No installed or catalogued model covers the language that was asked
    /// for, at the tier that was asked for.
    LanguageUnsupported,
    /// The named model is not installed on this machine.
    NotInstalled,
    /// A path inside a model directory would leave it — an absolute path, a
    /// `..` component, a drive letter. Refused rather than normalised.
    PathEscape,
    /// The bytes could not be fetched: no route, a refusing mirror, a server
    /// answering something the resume logic cannot use.
    DownloadFailed,
    /// The bytes verified and could not be published — a staging directory that
    /// would not rename, a disk that filled, a concurrent installer that
    /// crashed mid-publish.
    InstallFailed,
    /// The operation needed the network and there is none. Never reached by
    /// loading an installed model: that path is offline by construction.
    Offline,
    /// A `prune` or `remove` was refused because another process holds a lease
    /// on the model. Channel-independence is what makes this reachable.
    Leased,
    /// The caller set the cancel flag. Not a failure; reported so a caller can
    /// tell it apart from one.
    Cancelled,
    /// Nobody has agreed to this download, or the grant on this machine does
    /// not cover this offer — a different licence, or more bytes than were on
    /// the screen.
    ///
    /// It is a *refusal* rather than a failure, and it is the last line of the
    /// consent gate rather than the first: `install` asks for a
    /// [`koru_voice_consent::Granted`] by value, so the ordinary way to not
    /// have consent is to be unable to call it at all. This is what is left —
    /// a token that exists and does not cover what is being asked for,
    /// including one recorded by another feature.
    ConsentRequired,
    /// The filesystem said no, and there is nothing more specific to say. The
    /// OS error goes in `message`.
    Io,
}

impl ModelErrorCode {
    /// Every code, so a test can walk them.
    pub const ALL: [ModelErrorCode; 22] = [
        ModelErrorCode::NoTrustRoot,
        ModelErrorCode::SignatureMalformed,
        ModelErrorCode::SignatureUnknownKey,
        ModelErrorCode::SignatureInvalid,
        ModelErrorCode::SchemaUnsupported,
        ModelErrorCode::DescriptorInvalid,
        ModelErrorCode::CatalogInvalid,
        ModelErrorCode::CatalogUnavailable,
        ModelErrorCode::DigestMismatch,
        ModelErrorCode::SizeMismatch,
        ModelErrorCode::IdentityConflict,
        ModelErrorCode::EngineMismatch,
        ModelErrorCode::LanguageUnsupported,
        ModelErrorCode::NotInstalled,
        ModelErrorCode::PathEscape,
        ModelErrorCode::DownloadFailed,
        ModelErrorCode::InstallFailed,
        ModelErrorCode::Offline,
        ModelErrorCode::Leased,
        ModelErrorCode::Cancelled,
        ModelErrorCode::ConsentRequired,
        ModelErrorCode::Io,
    ];

    /// The stable token, pinned by a test against what serde writes — otherwise
    /// a `rename_all` change would move the wire spelling and leave every log
    /// line behind.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelErrorCode::NoTrustRoot => "no_trust_root",
            ModelErrorCode::SignatureMalformed => "signature_malformed",
            ModelErrorCode::SignatureUnknownKey => "signature_unknown_key",
            ModelErrorCode::SignatureInvalid => "signature_invalid",
            ModelErrorCode::SchemaUnsupported => "schema_unsupported",
            ModelErrorCode::DescriptorInvalid => "descriptor_invalid",
            ModelErrorCode::CatalogInvalid => "catalog_invalid",
            ModelErrorCode::CatalogUnavailable => "catalog_unavailable",
            ModelErrorCode::DigestMismatch => "digest_mismatch",
            ModelErrorCode::SizeMismatch => "size_mismatch",
            ModelErrorCode::IdentityConflict => "identity_conflict",
            ModelErrorCode::EngineMismatch => "engine_mismatch",
            ModelErrorCode::LanguageUnsupported => "language_unsupported",
            ModelErrorCode::NotInstalled => "not_installed",
            ModelErrorCode::PathEscape => "path_escape",
            ModelErrorCode::DownloadFailed => "download_failed",
            ModelErrorCode::InstallFailed => "install_failed",
            ModelErrorCode::Offline => "offline",
            ModelErrorCode::Leased => "leased",
            ModelErrorCode::Cancelled => "cancelled",
            ModelErrorCode::ConsentRequired => "consent_required",
            ModelErrorCode::Io => "io",
        }
    }

    /// The code a token names. `None` rather than a catch-all: a code nobody
    /// declared is not a code.
    pub fn parse(token: &str) -> Option<ModelErrorCode> {
        ModelErrorCode::ALL
            .into_iter()
            .find(|c| c.as_str() == token)
    }

    /// Build the error with developer text attached.
    pub fn with(self, message: impl Into<String>) -> ModelError {
        ModelError {
            code: self,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ModelErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refusal or a failure: a code to branch on, and developer text that is
/// **never shown to a person**.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ModelError {
    pub code: ModelErrorCode,
    /// Untranslated, English, for a developer and a log. It may name an id, a
    /// digest, a stage or a count; it may never carry a transcript, a device's
    /// name or a person's filesystem layout beyond what a path already
    /// implies — see `docs/contributing/operations/LOGGING.md`.
    pub message: String,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ModelError {}

impl From<std::io::Error> for ModelError {
    fn from(error: std::io::Error) -> ModelError {
        ModelErrorCode::Io.with(error.to_string())
    }
}

#[cfg(test)]
mod tests;
