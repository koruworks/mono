// SPDX-License-Identifier: MPL-2.0

//! The trust root's own suite. Its full behaviour — every refusal code, the
//! policy guard — is also pinned by `koru-voice-models`' trust tests, which
//! predate the extraction and now run against this code through voice's typed
//! descriptor.

use super::*;
use crate::descriptor::ModelDescriptor;
use crate::fixtures::{self, Signer};
use crate::token::Token;

#[test]
fn a_token_descriptor_verifies_and_a_rerouted_one_does_not() {
    let signer = Signer::new();
    let published = fixtures::descriptor();
    let signature = signer.sign(&published);
    signer.trusted.verify(&published, &signature).unwrap();

    let rerouted = ModelDescriptor {
        engine: Token::parse("whisper").unwrap(),
        ..published
    };
    assert_eq!(
        signer
            .trusted
            .verify(&rerouted, &signature)
            .unwrap_err()
            .code,
        ModelErrorCode::SignatureInvalid
    );
}

#[test]
fn an_unpinned_key_and_an_empty_set_are_different_answers() {
    let ours = Signer::new();
    let theirs = Signer::new();
    let descriptor = fixtures::descriptor();
    assert_eq!(
        ours.trusted
            .verify(&descriptor, &theirs.sign(&descriptor))
            .unwrap_err()
            .code,
        ModelErrorCode::SignatureUnknownKey
    );
    let empty = TrustedKeys::from_public_keys::<&str>(&[]).unwrap();
    assert_eq!(
        empty
            .verify(&descriptor, &ours.sign(&descriptor))
            .unwrap_err()
            .code,
        ModelErrorCode::NoTrustRoot
    );
}

/// Moving the trust root did not mint a key. The shipped set is still empty,
/// and a stable build still cannot reach the digest-only arm.
#[test]
fn the_pinned_set_is_still_empty_and_stable_still_refuses() {
    assert!(MODEL_SIGNING_KEYS.is_empty());
    assert!(TrustedKeys::pinned().is_empty());
    assert_eq!(
        TrustPolicy::digest_only_on(Some("stable"))
            .unwrap_err()
            .code,
        ModelErrorCode::NoTrustRoot
    );
}
