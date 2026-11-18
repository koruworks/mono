// SPDX-License-Identifier: MPL-2.0

use super::*;

const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn the_reader_token_is_a_valid_token_and_is_not_the_text_readers() {
    assert_eq!(reader_id().as_str(), "pp-ocr");
    assert_ne!(reader_id(), ReaderId::text());
}

/// **`cargo test --workspace` compiles no runtime**: the default feature set
/// is empty, and each runtime dependency is optional, so nothing native is in
/// an ordinary build's graph.
#[test]
fn the_runtime_is_off_by_default_and_every_runtime_dependency_is_optional() {
    assert!(
        MANIFEST.contains("\ndefault = []\n"),
        "the default feature set must stay empty"
    );
    for name in ["oar-ocr", "ort", "image", "rayon"] {
        let line = MANIFEST
            .lines()
            .find(|line| line.starts_with(&format!("{name} = ")))
            .unwrap_or_else(|| panic!("{name} is not declared"));
        assert!(line.contains("optional = true"), "{name} must be optional");
    }
}

/// The pins Plan 0.1 settled: `oar-ocr` 0.10.2 needs `ort` exactly rc.13, and
/// `ort` must be built for API 27 with nothing that downloads.
#[test]
fn ort_and_oar_ocr_are_pinned_exactly_with_api_27_and_no_download() {
    let ort = MANIFEST
        .lines()
        .find(|line| line.starts_with("ort = "))
        .unwrap();
    assert!(ort.contains("\"=2.0.0-rc.13\""));
    assert!(ort.contains("default-features = false"));
    assert!(ort.contains("\"api-27\""));
    assert!(!ort.contains("download"));
    assert!(!ort.contains("load-dynamic"));
    let oar = MANIFEST
        .lines()
        .find(|line| line.starts_with("oar-ocr = "))
        .unwrap();
    assert!(oar.contains("\"=0.10.2\""));
    assert!(oar.contains("default-features = false"));
}

#[test]
fn a_build_without_the_runtime_says_so_and_loads_nothing() {
    if RECOGNITION_BUILT {
        return;
    }
    // A directory that does not exist: nothing is looked at.
    let error = match load(&ModelDir::new("no-such-model"), &Settings::default()) {
        Ok(_) => panic!("a build without the runtime cannot load a reader"),
        Err(error) => error,
    };
    assert_eq!(error.code, FailureCode::Unsupported);
}
