// SPDX-License-Identifier: MPL-2.0

//! **The model store's codes, and the one translation into the session's
//! vocabulary.**
//!
//! [`ModelErrorCode`] and [`ModelError`] are `koru-models`' — one closed list
//! for every feature that installs a model — and are re-exported here so
//! voice's callers name them as they always have.
//!
//! ## Why the session mapping is here and not there
//!
//! [`koru_voice::ErrorCode`] is the *session* vocabulary: the codes a client
//! matches exhaustively while a dictation is running. Installing a model is not
//! a dictation, and fails in ways a session has no words for — a signature from
//! a key nobody pinned, a catalog reusing an id, a download somebody cancelled.
//! So there are two closed enums and **one** total mapping between them,
//! [`SessionCodeExt::session_code`]: a worker that fails to get a model answers
//! its session with that, and keeps the specific code for its log and its
//! `status` table. The mapping is a `match` with no wildcard, so a new code in
//! `koru-models` is a compile error here rather than a silent
//! `ModelLoadFailed`.
//!
//! It is voice's, so it lives in voice's crate, as an extension trait — the
//! one change a caller of `code.session_code()` sees is that the trait must be
//! in scope (`use koru_voice_models::SessionCodeExt`).

pub use koru_models::error::{ModelError, ModelErrorCode};

/// **What a session says when getting a model failed.**
pub trait SessionCodeExt {
    /// The whole translation between the two vocabularies, in one place and
    /// with no wildcard arm. The split that matters is
    /// [`koru_voice::ErrorCode::ModelMissing`]'s own: *only one of them is
    /// fixed by downloading something*. So everything about acquiring bytes —
    /// not installed, offline, a failed download, a failed install — is
    /// `ModelMissing`, and everything about bytes that are here and unusable is
    /// `ModelLoadFailed`.
    fn session_code(self) -> koru_voice::ErrorCode;
}

impl SessionCodeExt for ModelErrorCode {
    fn session_code(self) -> koru_voice::ErrorCode {
        use koru_voice::ErrorCode;
        match self {
            ModelErrorCode::NotInstalled
            | ModelErrorCode::Offline
            // Nothing is published at the address, which is a reason there are
            // no bytes rather than a reason the bytes here are unusable --
            // `ModelMissing` is the half of the split that a download fixes.
            | ModelErrorCode::CatalogUnavailable
            | ModelErrorCode::DownloadFailed
            | ModelErrorCode::InstallFailed => ErrorCode::ModelMissing,
            ModelErrorCode::LanguageUnsupported => ErrorCode::LanguageUnsupported,
            // The session vocabulary has its own word for this, and it is the
            // one a host branches on to open the consent step rather than to
            // print a sentence.
            ModelErrorCode::ConsentRequired => ErrorCode::ConsentRequired,
            ModelErrorCode::NoTrustRoot
            | ModelErrorCode::SignatureMalformed
            | ModelErrorCode::SignatureUnknownKey
            | ModelErrorCode::SignatureInvalid
            | ModelErrorCode::SchemaUnsupported
            | ModelErrorCode::DescriptorInvalid
            | ModelErrorCode::CatalogInvalid
            | ModelErrorCode::DigestMismatch
            | ModelErrorCode::SizeMismatch
            | ModelErrorCode::IdentityConflict
            | ModelErrorCode::EngineMismatch
            | ModelErrorCode::PathEscape
            | ModelErrorCode::Leased
            | ModelErrorCode::Cancelled
            | ModelErrorCode::Io => ErrorCode::ModelLoadFailed,
        }
    }
}

/// What a session is told about a model failure.
pub trait SessionErrorExt {
    /// The code translates (see [`SessionCodeExt::session_code`]) and the
    /// message travels, so a worker never invents text.
    fn session_error(&self) -> koru_voice::VoiceError;
}

impl SessionErrorExt for ModelError {
    fn session_error(&self) -> koru_voice::VoiceError {
        self.code.session_code().with(self.message.clone())
    }
}

#[cfg(test)]
mod tests;
