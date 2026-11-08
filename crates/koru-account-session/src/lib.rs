// SPDX-License-Identifier: MPL-2.0

//! Provider-neutral account session and temporary-flow security.
//!
//! Persistence, time, entropy, transient encryption, keyed digests, and rate
//! limiting enter through core ports. The policy therefore performs no I/O.

mod account;
mod capability_keys;
mod capability_verification;
pub mod cloud;
mod jwt;
pub mod oauth;
mod password;
mod policy;
mod regional;
mod runtime;
mod service;

pub use capability_keys::TokenCapabilityKeyRing;
pub use capability_verification::{
    TokenCapabilityClaims, TokenCapabilityFields, TokenCapabilityRequirements,
    TokenCapabilityVerificationError, TokenCapabilityVerifier, VerifiedTokenCapability,
};
pub use jwt::{
    DEVICE_ATTESTATION_AUDIENCE, DEVICE_ATTESTATION_MAX_MILLIS, DEVICE_ATTESTATION_VERSION,
    DeviceAttestationClaimsV1, DeviceAttestationKeyRing, Ed25519KeyRing,
    InspectedDeviceAttestationV1, JwtVerificationError, SignedDeviceAttestationV1,
    VerifiedDeviceAttestationV1, inspect_device_attestation,
    inspect_device_attestation_with_policy, verify_access_token,
    verify_access_token_for_sync_issuance, verify_access_token_with_policy,
    verify_device_attestation, verify_device_attestation_with_policy,
};
pub use password::*;
pub use policy::*;
pub use regional::{CredentialIssuance, REGIONAL_CREDENTIAL_VERSION, RegionalCredentialPolicy};
pub use runtime::*;
pub use service::*;

// This crate's own suite. **Not** `mod tests`: `pub use account::*` already
// re-exports `account::tests` — the cross-crate test-support API — and a
// second `tests` at the root shadows it in test builds.
#[cfg(test)]
mod lib_tests;
pub use account::*;
