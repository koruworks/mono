// SPDX-License-Identifier: MPL-2.0

//! **What the reader's model publishes to the origin: the keys, and the JSON
//! at each of them.**
//!
//! Built from `koru-models`' own types and serialised — never assembled as
//! JSON — for the reason `voice/catalog.rs` gives: a field renamed in the
//! model store must break this build, not produce a catalog every desktop
//! declines.
//!
//! | Key | What | Cached |
//! | --- | --- | --- |
//! | `models/<id>/<version>/<digest>.zip` | the archive | immutable, one year |
//! | `models/<id>/<version>.json` | one `CatalogEntry` | 30 s at the edge |
//! | `read/catalog-v1.json` | the reader's `Catalog` | 30 s at the edge |
//!
//! The archive and descriptor keys are voice's layout, in the same `models/`
//! namespace (model ids are unique across both tables, and a test holds it).
//! **The catalog is the reader's own**, at `read/` beside `voice/`: each
//! feature reads its own catalog (`CatalogSource::resolve_from`), so a speech
//! build never lists a reader and the other way round. The cache headers are
//! voice's constants, not copies, because the reasoning is the same.
//!
//! **The signature field holds voice's `UNSIGNED` sentence.** No model-signing
//! key exists; this tool mints none. Under the digest-only policy nightly and
//! beta use, the signature is skipped and the signed `size` and `digest` are
//! checked against the bytes; under `TrustPolicy::SIGNED` (stable) the entry is
//! refused, as every model is until the key exists.

use super::table::ReadModel;
use crate::cloud::HOST;
use crate::voice::catalog::UNSIGNED;
use koru_models::{
    CATALOG_SCHEMA_VERSION, Catalog, CatalogEntry, DESCRIPTOR_SCHEMA_VERSION, LanguageTag,
    LicenceRef, ModelDescriptor, ModelId, ModelSignature, ModelVersion, Sha256Digest, Token,
};

/// Where the reader's catalog goes. Its public address is
/// `https://updates.koru.do/read/catalog-v1.json`, which the installer (OCR
/// Plan 2.4) names as its default, overridable by its own variable.
pub const CATALOG_KEY: &str = "read/catalog-v1.json";

/// `models/<id>/<version>/<digest>.zip`, voice's layout.
pub fn archive_key(model: &ReadModel, digest: &str) -> String {
    format!("models/{}/{}/{digest}.zip", model.id, model.version)
}

/// `models/<id>/<version>.json`, holding one `CatalogEntry`.
pub fn descriptor_key(model: &ReadModel) -> String {
    format!("models/{}/{}.json", model.id, model.version)
}

/// The public URL an entry points a downloader at.
pub fn archive_url(model: &ReadModel, digest: &str) -> String {
    format!("https://{HOST}/{}", archive_key(model, digest))
}

/// The catalog's public URL.
pub fn catalog_url() -> String {
    format!("https://{HOST}/{CATALOG_KEY}")
}

/// **The descriptor every field of which a signature will one day cover.**
///
/// Every newtype checks its own rule at construction and
/// `ModelDescriptor::validate` checks what no single field can know, so a bad
/// tag or token fails on this machine rather than in an installer.
pub fn descriptor(model: &ReadModel, size: u64, digest: &str) -> Result<ModelDescriptor, String> {
    let languages = model
        .languages
        .iter()
        .map(|tag| LanguageTag::parse(*tag).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let descriptor = ModelDescriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION,
        id: ModelId::parse(model.id).map_err(|error| error.to_string())?,
        version: ModelVersion::parse(model.version).map_err(|error| error.to_string())?,
        engine: Token::parse(model.engine).map_err(|error| error.to_string())?,
        tier: Token::parse(model.tier).map_err(|error| error.to_string())?,
        languages,
        size,
        digest: Sha256Digest::parse(digest).map_err(|error| error.to_string())?,
        working_set_bytes: model.working_set_bytes,
        licence: LicenceRef::parse(model.licence).map_err(|error| error.to_string())?,
        attribution: model.attribution.to_string(),
    };
    descriptor.validate().map_err(|error| error.to_string())?;
    // The canonical bytes are what a key will one day sign; rendering them now
    // is free and proves they can be.
    descriptor
        .canonical_bytes()
        .map_err(|error| error.to_string())?;
    Ok(descriptor)
}

/// One catalog entry for the archive of this `size` and `digest`.
pub fn entry(model: &ReadModel, size: u64, digest: &str) -> Result<CatalogEntry, String> {
    Ok(CatalogEntry {
        descriptor: descriptor(model, size, digest)?,
        signature: ModelSignature::parse(UNSIGNED).map_err(|error| error.to_string())?,
        url: archive_url(model, digest)
            .parse()
            .map_err(|error| format!("{}: {error}", model.id))?,
        mirrors: Vec::new(),
    })
}

/// The catalog document, validated the way every desktop will validate it.
pub fn catalog(entries: Vec<CatalogEntry>) -> Result<Catalog, String> {
    let catalog = Catalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        entries,
    };
    catalog.validate().map_err(|error| error.to_string())?;
    Ok(catalog)
}

/// **The two JSON documents a publish writes**, byte for byte — the same
/// bytes `package` stages to disk and `publish` uploads, so what was checked
/// on disk is what reaches the origin.
pub struct Documents {
    pub entry: CatalogEntry,
    pub descriptor: Vec<u8>,
    pub catalog: Vec<u8>,
}

pub fn documents(model: &ReadModel, size: u64, digest: &str) -> Result<Documents, String> {
    let entry = entry(model, size, digest)?;
    let descriptor = crate::voice::catalog::render(&entry)?;
    let catalog = crate::voice::catalog::render(&catalog(vec![entry.clone()])?)?;
    Ok(Documents {
        entry,
        descriptor,
        catalog,
    })
}
