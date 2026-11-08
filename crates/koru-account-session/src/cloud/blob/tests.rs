// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::TokenCapabilityKeyRing;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};

const NOW_MILLIS: i64 = 1_800_000_000_000;
const HASH: &str = "b3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn id(byte: u8) -> CloudId {
    let mut bytes = [byte; 16];
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    CloudId::from_bytes(bytes)
}

fn ring() -> TokenCapabilityKeyRing {
    TokenCapabilityKeyRing::new(
        Region::Us,
        "regional".into(),
        [("regional".to_string(), SigningKey::from_bytes(&[7; 32]))],
    )
    .unwrap()
}

fn verifier() -> TokenCapabilityVerifier {
    ring()
        .verifier("https://us.example.invalid".into())
        .unwrap()
}

fn claims(purpose: BlobPurpose) -> BlobCapabilityClaimsV1 {
    let nbf = blob_capability_not_before(NOW_MILLIS);
    BlobCapabilityClaimsV1 {
        v: 1,
        pur: purpose.as_str().into(),
        iss: "https://us.example.invalid".into(),
        aud: "us.data.koru.do".into(),
        rgn: Region::Us,
        acc: id(1),
        wsp: id(2),
        inc: id(3),
        eph: 7,
        sub: match purpose {
            BlobPurpose::Write => id(4),
            BlobPurpose::Read => id(6),
        },
        loc: id(6),
        bh: HASH.into(),
        len: 4_194_304,
        nbf,
        exp: blob_capability_expiry(nbf).unwrap(),
        jti: id(5),
    }
}

fn mint(claims: &BlobCapabilityClaimsV1) -> Vec<u8> {
    ring()
        .sign_blob_capability("https://us.example.invalid", claims)
        .unwrap()
        .compact()
        .to_vec()
}

/// A capability minted for writing verifies for writing, and the claims come
/// back whole — the verifier projects nothing out of them.
#[test]
fn a_freshly_minted_write_capability_verifies_and_retains_every_claim() {
    let expected = claims(BlobPurpose::Write);
    let token = mint(&expected);
    let verified = verify_blob_capability(
        &verifier(),
        &token,
        BlobPurpose::Write,
        "us.data.koru.do",
        Region::Us,
        NOW_MILLIS,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(verified.claims()).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}

/// C05 and C06: purpose is compared exactly in both directions, because at
/// rung 7 one of these tokens is handed to a third party.
#[test]
fn purpose_is_compared_exactly_in_both_directions() {
    for (minted, used) in [
        (BlobPurpose::Read, BlobPurpose::Write),
        (BlobPurpose::Write, BlobPurpose::Read),
    ] {
        let token = mint(&claims(minted));
        assert_eq!(
            verify_blob_capability(
                &verifier(),
                &token,
                used,
                "us.data.koru.do",
                Region::Us,
                NOW_MILLIS
            )
            .unwrap_err(),
            Error::WrongPurpose,
            "{} used as {}",
            minted.as_str(),
            used.as_str()
        );
    }
}

/// C07: a capability for another host is refused by this one.
#[test]
fn an_audience_naming_another_host_is_refused() {
    let mut value = claims(BlobPurpose::Write);
    value.aud = "evil.example.com".into();
    let token = mint(&value);
    assert_eq!(
        verify_blob_capability(
            &verifier(),
            &token,
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Us,
            NOW_MILLIS
        )
        .unwrap_err(),
        Error::WrongAudience
    );
}

/// C03/C04: a region token is compared against the deployment's own, so a
/// capability minted for one region is refused by the other's worker. Both
/// directions, because the EU worker is the same code with another constant.
#[test]
fn a_capability_for_the_other_region_is_refused_in_both_directions() {
    let eu_ring = TokenCapabilityKeyRing::new(
        Region::Eu,
        "regional".into(),
        [("regional".to_string(), SigningKey::from_bytes(&[7; 32]))],
    )
    .unwrap();
    let mut eu = claims(BlobPurpose::Write);
    eu.rgn = Region::Eu;
    eu.aud = "eu.data.koru.do".into();
    let eu_token = eu_ring
        .sign_blob_capability("https://us.example.invalid", &eu)
        .unwrap()
        .compact()
        .to_vec();
    // The EU capability at the US worker.
    assert_eq!(
        verify_blob_capability(
            &verifier(),
            &eu_token,
            BlobPurpose::Write,
            "eu.data.koru.do",
            Region::Us,
            NOW_MILLIS
        )
        .unwrap_err(),
        Error::WrongRegion
    );
    // The US capability at the EU worker.
    let eu_verifier = eu_ring
        .verifier("https://us.example.invalid".into())
        .unwrap();
    let us_token = mint(&claims(BlobPurpose::Write));
    assert_eq!(
        verify_blob_capability(
            &eu_verifier,
            &us_token,
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Eu,
            NOW_MILLIS
        )
        .unwrap_err(),
        Error::WrongRegion
    );
}

/// C08 and C09: outside `nbf`…`exp` in either direction.
#[test]
fn the_window_is_closed_at_both_ends() {
    let value = claims(BlobPurpose::Write);
    let token = mint(&value);
    assert_eq!(
        verify_blob_capability(
            &verifier(),
            &token,
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Us,
            value.exp * 1000
        )
        .unwrap_err(),
        Error::CredentialExpired
    );
    assert_eq!(
        verify_blob_capability(
            &verifier(),
            &token,
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Us,
            value.nbf * 1000 - 1
        )
        .unwrap_err(),
        Error::CredentialNotYetValid
    );
}

/// C10: a tampered signature never verifies, whatever the claims say.
#[test]
fn a_tampered_signature_is_refused() {
    let token = mint(&claims(BlobPurpose::Write));
    let text = String::from_utf8(token).unwrap();
    let (input, signature) = text.rsplit_once('.').unwrap();
    let mut raw = URL_SAFE_NO_PAD.decode(signature).unwrap();
    raw[0] ^= 0x01;
    let tampered = format!("{input}.{}", URL_SAFE_NO_PAD.encode(raw));
    assert_eq!(
        verify_blob_capability(
            &verifier(),
            tampered.as_bytes(),
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Us,
            NOW_MILLIS
        )
        .unwrap_err(),
        Error::Signature
    );
}

/// C11: an unknown contract version is refused rather than interpreted. It has
/// to be hand-signed: `sign_blob_capability` refuses to mint one at all.
#[test]
fn an_unknown_contract_version_is_refused_and_cannot_be_minted() {
    let mut value = claims(BlobPurpose::Write);
    value.v = 2;
    assert!(
        ring()
            .sign_blob_capability("https://us.example.invalid", &value)
            .is_err()
    );
    let header = serde_json::json!({"alg":"EdDSA","typ":BLOB_CAPABILITY_TYPE,"kid":"regional"});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap())
    );
    let token = format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(
            SigningKey::from_bytes(&[7; 32])
                .sign(input.as_bytes())
                .to_bytes()
        )
    );
    assert_eq!(
        verify_blob_capability(
            &verifier(),
            token.as_bytes(),
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Us,
            NOW_MILLIS
        )
        .unwrap_err(),
        Error::UnsupportedVersion
    );
}

/// A sync credential is not a blob capability: the header type separates the
/// two families even though one ring signs both.
#[test]
fn a_sync_credential_header_type_is_not_admitted_as_a_blob_capability() {
    let value = claims(BlobPurpose::Write);
    let header =
        serde_json::json!({"alg":"EdDSA","typ":"koru-sync-credential+jwt","kid":"regional"});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap())
    );
    let token = format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(
            SigningKey::from_bytes(&[7; 32])
                .sign(input.as_bytes())
                .to_bytes()
        )
    );
    assert!(
        verify_blob_capability(
            &verifier(),
            token.as_bytes(),
            BlobPurpose::Write,
            "us.data.koru.do",
            Region::Us,
            NOW_MILLIS
        )
        .is_err()
    );
}

/// The two spellings never cross (D248): the claim carries the `b3:<hex>`
/// address, never the `b3-<hex>` filename, and the decoder refuses the latter.
#[test]
fn the_filename_spelling_of_a_hash_is_not_a_claim_value() {
    assert!(valid_blob_hash(HASH));
    assert!(!valid_blob_hash(&HASH.replace(':', "-")));
    assert!(!valid_blob_hash("b3:AAAA"));
    assert!(!valid_blob_hash("sha256:00"));
    assert!(!valid_blob_hash("b3:"));
    let mut value = claims(BlobPurpose::Write);
    value.bh = HASH.replace(':', "-");
    assert!(
        ring()
            .sign_blob_capability("https://us.example.invalid", &value)
            .is_err()
    );
    let json = serde_json::to_string(&value).unwrap();
    assert!(serde_json::from_str::<BlobCapabilityClaimsV1>(&json).is_err());
}

/// An unknown field is refused rather than ignored, in both directions.
#[test]
fn an_unknown_claim_is_refused() {
    let mut json: serde_json::Value = serde_json::to_value(claims(BlobPurpose::Write)).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("extra".into(), serde_json::json!(1));
    assert!(serde_json::from_value::<BlobCapabilityClaimsV1>(json).is_err());
}

/// A negative Cloud-access epoch is not a fence that can be compared, so it is
/// refused in deserialization rather than at some later `<`.
#[test]
fn a_negative_access_epoch_is_refused() {
    let mut json: serde_json::Value = serde_json::to_value(claims(BlobPurpose::Write)).unwrap();
    json["eph"] = serde_json::json!(-1);
    assert!(serde_json::from_value::<BlobCapabilityClaimsV1>(json).is_err());
}

/// Backdating spends lifetime and never adds any, and a worker running behind
/// the API still accepts a capability minted right now.
#[test]
fn backdating_shortens_the_window_and_covers_a_worker_running_behind() {
    let nbf = blob_capability_not_before(NOW_MILLIS);
    let exp = blob_capability_expiry(nbf).unwrap();
    assert!(exp * 1000 <= NOW_MILLIS + BLOB_CAPABILITY_DEFAULT_SECONDS * 1000);
    for behind in [1, 999, BLOB_CAPABILITY_NOT_BEFORE_BACKDATE_SECONDS * 1000] {
        assert!(nbf * 1000 <= NOW_MILLIS - behind, "{behind} ms behind");
    }
    assert_eq!(blob_capability_not_before(0), 0);
    assert!(blob_capability_expiry(0).is_some());
}

/// Zero length at a nonempty content address is still refused.
#[test]
fn a_zero_length_capability_cannot_be_minted() {
    let mut value = claims(BlobPurpose::Write);
    value.len = 0;
    assert!(
        ring()
            .sign_blob_capability("https://us.example.invalid", &value)
            .is_err()
    );
}

#[test]
fn the_empty_file_has_a_signed_capability() {
    let mut value = claims(BlobPurpose::Write);
    value.len = 0;
    value.bh = EMPTY_BLOB_HASH.to_owned();
    assert!(
        ring()
            .sign_blob_capability("https://us.example.invalid", &value)
            .is_ok()
    );
}

/// Signing refuses an issuer that is not this deployment's, so a claim cannot
/// name one hierarchy and be signed by another.
#[test]
fn signing_refuses_an_issuer_the_claims_do_not_name() {
    let value = claims(BlobPurpose::Write);
    assert!(
        ring()
            .sign_blob_capability("https://eu.example.invalid", &value)
            .is_err()
    );
}

/// A read capability names one object twice and both spellings must agree: its
/// `sub` is the locator, so a `loc` that says something else is a credential
/// for two different objects at once.
#[test]
fn a_read_capability_must_name_one_object_in_both_claims() {
    let mut value = claims(BlobPurpose::Read);
    assert!(
        ring()
            .sign_blob_capability("https://us.example.invalid", &value)
            .is_ok()
    );
    value.loc = id(9);
    assert!(
        ring()
            .sign_blob_capability("https://us.example.invalid", &value)
            .is_err()
    );
}

/// A write capability names the session in `sub` and the object it will become
/// in `loc`, and they are deliberately different: the worker publishes to the
/// locator the API minted and can learn it from nowhere else.
#[test]
fn a_write_capability_binds_the_session_and_the_object_it_will_become() {
    let value = claims(BlobPurpose::Write);
    assert_ne!(value.sub, value.loc);
    let token = mint(&value);
    let verified = verify_blob_capability(
        &verifier(),
        &token,
        BlobPurpose::Write,
        "us.data.koru.do",
        Region::Us,
        NOW_MILLIS,
    )
    .unwrap();
    assert_eq!(verified.claims().loc, value.loc);
}
