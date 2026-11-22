// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures::{self, Signer};

/// The wire shape is flat and the token fields are plain strings — the same
/// document a speech catalog already is.
#[test]
fn an_entry_is_flat_on_the_wire() {
    let entry = fixtures::entry(&Signer::new(), fixtures::descriptor());
    let json: serde_json::Value = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["engine"], "sherpa");
    assert_eq!(json["tier"], "compact");
    assert!(json.get("descriptor").is_none());
    let back: CatalogEntry = serde_json::from_value(json).unwrap();
    assert_eq!(back, entry);
}

#[test]
fn a_catalog_refuses_plain_http_and_a_repeated_identity() {
    let signer = Signer::new();
    let mut plain = fixtures::catalog(vec![fixtures::entry(&signer, fixtures::descriptor())]);
    plain.entries[0].url = Url::parse("http://models.invalid/m.zip").unwrap();
    assert_eq!(
        plain.validate().unwrap_err().code,
        ModelErrorCode::CatalogInvalid
    );
    let twice = fixtures::catalog(vec![
        fixtures::entry(&signer, fixtures::descriptor()),
        fixtures::entry(&signer, fixtures::descriptor()),
    ]);
    assert_eq!(
        twice.validate().unwrap_err().code,
        ModelErrorCode::CatalogInvalid
    );
}

/// The override wins; without one, the default the feature named. Neither is
/// this crate's — it is handed both.
#[test]
fn the_caller_names_the_catalog() {
    let default = "https://updates.koru.do/some-feature/catalog-v1.json";
    assert_eq!(
        CatalogSource::resolve_from(None, default).unwrap(),
        CatalogSource::Url(Url::parse(default).unwrap())
    );
    let elsewhere = "https://mirror.invalid/catalog.json";
    assert_eq!(
        CatalogSource::resolve_from(Some(elsewhere.to_string()), default).unwrap(),
        CatalogSource::Url(Url::parse(elsewhere).unwrap())
    );
    assert_eq!(
        CatalogSource::resolve_from(Some("http://plain.invalid/c.json".into()), default)
            .unwrap_err()
            .code,
        ModelErrorCode::CatalogInvalid
    );
}

/// The terms are the signed fields, so a consent sentence and the grant
/// recorded against it cannot disagree.
#[test]
fn an_entrys_terms_are_the_signed_fields() {
    let entry = fixtures::entry(&Signer::new(), fixtures::descriptor());
    let terms = entry.terms();
    assert_eq!(terms.model, "parakeet-tdt-v3");
    assert_eq!(terms.licence, "Apache-2.0");
    assert_eq!(terms.download_bytes, 3);
    assert!(entry.understood());
}
