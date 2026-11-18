// SPDX-License-Identifier: MPL-2.0

//! **What every suite in this crate builds its vectors out of.**
//!
//! The same descriptor `koru-voice-models`' fixtures describe, written in
//! tokens — so the two crates' vectors canonicalise to the same bytes, and a
//! test here and a test there mean the same thing by *the fixture*.
//!
//! **The key pair is generated, not checked in**, for the reason the voice
//! fixtures give: a fixture signature stored as a string goes stale the first
//! time the canonical form changes. The one exception is
//! `descriptor/tests.rs`'s signature made *before* the extraction, which is
//! checked in precisely because it must not be regenerated.

use crate::catalog::{CATALOG_SCHEMA_VERSION, Catalog, CatalogEntry};
use crate::descriptor::{LicenceRef, ModelDescriptor, ModelSignature, ModelVersion, Sha256Digest};
use crate::token::{LanguageTag, ModelId, Token};
use crate::trust::TrustedKeys;
use minisign::{KeyPair, SecretKey, sign};
use std::io::Cursor;
use url::Url;

/// The digest of the three bytes `abc`, which is also what
/// [`archive_bytes`] contains.
pub const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

pub fn archive_bytes() -> &'static [u8] {
    b"abc"
}

/// A descriptor that passes every rule, for a test to then break one field of.
pub fn descriptor() -> ModelDescriptor {
    ModelDescriptor {
        schema_version: crate::DESCRIPTOR_SCHEMA_VERSION,
        id: ModelId::parse("parakeet-tdt-v3").unwrap(),
        version: ModelVersion::parse("1.2.0").unwrap(),
        engine: Token::parse("sherpa").unwrap(),
        tier: Token::parse("compact").unwrap(),
        languages: vec![
            LanguageTag::parse("en").unwrap(),
            LanguageTag::parse("zh-Hans").unwrap(),
        ],
        size: archive_bytes().len() as u64,
        digest: Sha256Digest::parse(ABC_SHA256).unwrap(),
        working_set_bytes: 1_073_741_824,
        licence: LicenceRef::parse("Apache-2.0").unwrap(),
        attribution: "k2-fsa".to_string(),
    }
}

/// A key pair, and the [`TrustedKeys`] that pins its public half.
pub struct Signer {
    pub trusted: TrustedKeys,
    secret: SecretKey,
    public: minisign::PublicKey,
}

impl Signer {
    pub fn new() -> Signer {
        let KeyPair { pk, sk } = KeyPair::generate_unencrypted_keypair().unwrap();
        let trusted = TrustedKeys::from_public_keys(&[pk.to_box().unwrap().to_string()]).unwrap();
        Signer {
            trusted,
            secret: sk,
            public: pk,
        }
    }

    /// Sign a descriptor's canonical bytes, the way the publisher will.
    pub fn sign(&self, descriptor: &ModelDescriptor) -> ModelSignature {
        let bytes = descriptor.canonical_bytes().unwrap();
        let signature = sign(
            Some(&self.public),
            &self.secret,
            Cursor::new(bytes),
            Some("koru model fixture"),
            Some("koru model fixture"),
        )
        .unwrap()
        .to_string();
        ModelSignature::parse(signature).unwrap()
    }
}

/// An entry the given signer vouches for.
pub fn entry(signer: &Signer, descriptor: ModelDescriptor) -> CatalogEntry {
    let signature = signer.sign(&descriptor);
    CatalogEntry {
        descriptor,
        signature,
        url: Url::parse("https://models.koru.do/v1/parakeet-1.2.0.zip").unwrap(),
        mirrors: Vec::new(),
    }
}

pub fn catalog(entries: Vec<CatalogEntry>) -> Catalog {
    Catalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        entries,
    }
}
