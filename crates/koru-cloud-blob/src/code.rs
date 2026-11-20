// SPDX-License-Identifier: MPL-2.0

//! The contract's §7 registry: lowercase `snake_case`, stable, branched on by
//! clients, rendered through the localized text registry — never surfaced raw.
//!
//! Every code is classed for the deferral rule the document pass already uses,
//! because "retry this file" and "retry this session" are different answers and
//! a client that cannot tell them apart either gives up too early or spins.

use serde::{Deserialize, Serialize};

/// What a refusal is a statement *about*, which is what decides whose progress
/// it defers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deferral {
    /// About the thing carried: this blob defers, the session continues.
    Unit,
    /// About the session: everything in it waits.
    Session,
}

macro_rules! codes {
    ($( $variant:ident => $text:literal, $class:ident, $status:expr, $doc:literal; )*) => {
        /// A stable code a client branches on. Its `Display` and its serialized
        /// form are the same lowercase token, in one place, so a route handler
        /// and a client cannot spell one of them differently.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum BlobCode {
            $( #[doc = $doc] $variant, )*
        }

        impl BlobCode {
            pub const ALL: &'static [Self] = &[$( Self::$variant ),*];

            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $text, )* }
            }

            /// Whether this refusal defers one unit or the whole session.
            pub const fn deferral(self) -> Deferral {
                match self { $( Self::$variant => Deferral::$class, )* }
            }

            /// The HTTP status a byte or control route answers with, or `None`
            /// for the two client-side conditions that never reach the wire.
            pub const fn status(self) -> Option<u16> {
                match self { $( Self::$variant => $status, )* }
            }

            pub fn parse(value: &str) -> Option<Self> {
                let mut found = None;
                let mut index = 0;
                while index < Self::ALL.len() {
                    if Self::ALL[index].as_str() == value {
                        found = Some(Self::ALL[index]);
                    }
                    index += 1;
                }
                found
            }
        }

        impl std::fmt::Display for BlobCode {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

codes! {
    MalformedRange => "malformed_range", Unit, Some(400),
        "`Content-Range` unparseable, inverted, or over `CHUNK`.";
    RangeConflict => "range_conflict", Unit, Some(409),
        "Not the next contiguous offset, or overlapping different bytes.";
    LengthMismatch => "length_mismatch", Unit, Some(400),
        "Declared total is not the capability's `len`, or the body is not the range.";
    HashMismatch => "hash_mismatch", Unit, Some(422),
        "Verification failed; nothing was published.";
    WrongRegion => "wrong_region", Session, Some(403),
        "The capability's region is not this deployment's.";
    WrongWorkspace => "wrong_workspace", Session, Some(403),
        "The capability's workspace is not the addressed resource's.";
    WrongObject => "wrong_object", Unit, Some(403),
        "The capability's `sub` is not the addressed upload or locator. A \
         denial must not teach an unauthorised caller that a locator exists, \
         so a locator that is simply absent answers this too.";
    StaleIncarnation => "stale_incarnation", Session, Some(403),
        "The dataset incarnation has been replaced.";
    StaleEpoch => "stale_epoch", Session, Some(403),
        "The Cloud-access epoch has moved.";
    CapabilityExpired => "capability_expired", Session, Some(401),
        "Outside `nbf`…`exp`.";
    CapabilityInvalid => "capability_invalid", Session, Some(401),
        "Signature, type, audience or claim shape — including an account \
         bearer token sent to a byte route.";
    UploadExpired => "upload_expired", Session, Some(410),
        "The session is terminal, aborted or reclaimed.";
    ObjectMissing => "object_missing", Unit, Some(404),
        "Cloud holds no bytes for this blob. **Ordinarily it is a wait, not a          fault**: the only device holding the bytes has not uploaded them          yet, and no device that lacks them can change that by asking again.          It is an inconsistency only where Cloud once answered for the          locator and no longer does, which is the quarantine path's reading          of the same word and is a different event with a different remedy.";
    ObjectTooLarge => "object_too_large", Unit, Some(413),
        "The declared size is above the one ceiling, refused at allocation.";
    StorageUnavailable => "storage_unavailable", Session, Some(503),
        "The volume cannot serve; retryable.";
    ModeRefusesTransport => "mode_refuses_transport", Session, None,
        "The Workspace's transport mode forbids this. Client-side; never on the wire.";
    RelayPathRefused => "relay_path_refused", Unit, None,
        "A peer byte transfer on a non-direct path. Client-side; never on the wire.";
    CloudPolicyUnavailable => "cloud_policy_unavailable", Unit, Some(422),
        "Current authoritative owner policy is unavailable.";
    CloudPlanRequired => "cloud_plan_required", Unit, Some(422),
        "Current owner policy does not permit ordinary writes.";
    CloudDocumentCountLimit => "cloud_document_count_limit", Unit, Some(422),
        "The configured retained document count bound refuses this upload.";
    CloudStoredDocumentBytesLimit => "cloud_stored_document_bytes_limit", Unit, Some(422),
        "The configured retained document bytes bound refuses this upload.";
    CloudPhysicalBytesLimit => "cloud_physical_bytes_limit", Unit, Some(422),
        "The configured physical bytes bound refuses this upload.";
    CloudStagingBytesLimit => "cloud_staging_bytes_limit", Unit, Some(422),
        "The configured staging bytes bound refuses this upload.";
    CloudRemovalCapacityLimit => "cloud_removal_capacity_limit", Unit, Some(422),
        "The configured restricted removal reserve is exhausted.";
    FileExceedsPlanAllowance => "file_exceeds_plan_allowance", Unit, Some(422),
        "This fresh file is larger than the current whole finite allowance.";
    UsageUnknown => "usage_unknown", Unit, Some(422),
        "Complete supported owner usage and holds cannot be proved.";
    AccountOverQuota => "account_over_quota", Unit, Some(422),
        "Prospective owner usage plus other live holds exceeds the allowance.";
}

impl BlobCode {
    /// Untranslated developer text. A client branches on the code and shows its
    /// own localized product sentence; this never reaches a person.
    pub const fn message(self) -> &'static str {
        match self {
            Self::MalformedRange => "the content range could not be admitted",
            Self::RangeConflict => "the range is not the next contiguous offset",
            Self::LengthMismatch => "the declared length does not match",
            Self::HashMismatch => "the uploaded bytes do not hash to the bound address",
            Self::WrongRegion => "the capability names another region",
            Self::WrongWorkspace => "the capability names another Workspace",
            Self::WrongObject => "the capability does not name this object",
            Self::StaleIncarnation => "the dataset incarnation has been replaced",
            Self::StaleEpoch => "the Cloud access epoch has moved",
            Self::CapabilityExpired => "the object capability is outside its window",
            Self::CapabilityInvalid => "the object capability was not admitted",
            Self::UploadExpired => "the upload session is no longer open",
            Self::ObjectMissing => "Cloud does not hold these bytes yet",
            Self::ObjectTooLarge => "the declared size is above the ceiling",
            Self::StorageUnavailable => "the object volume cannot serve this request",
            Self::ModeRefusesTransport => "the Workspace transport mode forbids this",
            Self::RelayPathRefused => "a peer byte transfer needs a direct path",
            Self::CloudPolicyUnavailable
            | Self::CloudPlanRequired
            | Self::CloudDocumentCountLimit
            | Self::CloudStoredDocumentBytesLimit
            | Self::CloudPhysicalBytesLimit
            | Self::CloudStagingBytesLimit
            | Self::CloudRemovalCapacityLimit
            | Self::FileExceedsPlanAllowance
            | Self::UsageUnknown
            | Self::AccountOverQuota => "Cloud admission refused",
        }
    }
}

/// Map the shared credential verifier's content-free observation onto this
/// registry. Only region has its own code; everything else about a credential
/// is `capability_invalid` or `capability_expired`, because a caller must not
/// learn *which* claim it got wrong.
pub fn from_verification_error(
    error: koru_account_session::TokenCapabilityVerificationError,
) -> BlobCode {
    use koru_account_session::TokenCapabilityVerificationError as Error;
    match error {
        Error::WrongRegion => BlobCode::WrongRegion,
        Error::CredentialNotYetValid | Error::CredentialExpired => BlobCode::CapabilityExpired,
        Error::Malformed
        | Error::UnknownKey
        | Error::Signature
        | Error::InvalidClaims
        | Error::UnsupportedVersion
        | Error::WrongPurpose
        | Error::WrongAudience
        | Error::WrongIssuer => BlobCode::CapabilityInvalid,
    }
}

#[cfg(test)]
mod tests;
