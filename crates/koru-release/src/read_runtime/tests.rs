// SPDX-License-Identifier: MPL-2.0

use super::*;

fn triples(list: &[&str]) -> Vec<String> {
    list.iter().map(|triple| triple.to_string()).collect()
}

/// **Recognition is built where its library is pinned, for the read worker
/// only.**
#[test]
fn the_feature_is_on_for_the_read_worker_on_a_pinned_target() {
    let packages = ["koru", "koru-read-worker"];
    assert_eq!(
        features(&packages, &triples(&["x86_64-pc-windows-msvc"])),
        Some("koru-read-worker/recognition")
    );
    assert_eq!(
        features(&packages, &triples(&["x86_64-unknown-linux-gnu"])),
        Some("koru-read-worker/recognition"),
        "Linux x86_64 links the library from its TTS-free archive"
    );
    assert_eq!(
        features(&packages, &triples(&["aarch64-unknown-linux-gnu"])),
        None,
        "Linux aarch64 has no TTS-free archive"
    );
    assert_eq!(
        features(&packages, &triples(&["aarch64-apple-darwin"])),
        None,
        "macOS is pinned when a Mac build first links it"
    );
    assert_eq!(
        features(&["koru"], &triples(&["x86_64-pc-windows-msvc"])),
        None,
        "a build without the read worker names no feature of it"
    );
}

#[test]
fn the_feature_is_the_read_workers_own() {
    let manifest = include_str!("../../../../apps/koru-read/Cargo.toml");
    assert!(manifest.contains("\nrecognition = [\"koru-read-paddle/runtime\"]\n"));
    assert!(manifest.contains("\ndefault = []\n"), "off by default");
    assert_eq!(FEATURE, format!("{PACKAGE}/recognition"));
}

#[test]
fn a_refusal_says_what_to_run() {
    let library = &artifacts::ONNXRUNTIME[0];
    let missing = require_canonical(
        library,
        &Linked::Missing(PathBuf::from("lib/onnxruntime.lib")),
    )
    .unwrap_err();
    assert!(missing.contains("fetch-voice-runtime"));
    let other = require_canonical(
        library,
        &Linked::NotCanonical {
            path: PathBuf::from("lib/onnxruntime.lib"),
            sha256: "00".repeat(32),
            size: 1,
        },
    )
    .unwrap_err();
    assert!(other.contains(library.sha256));
    assert!(other.contains("ORT_LIB_PATH"));
    assert_eq!(require_canonical(library, &Linked::Canonical), Ok(()));
    assert!(notice(library, &Linked::Canonical).is_none());
}

/// **What a release builds on this host is what the table says**: Windows x64
/// and Linux x86_64 turn recognition on and point the build at the fetched
/// library.
#[test]
fn this_host_builds_recognition_where_a_library_is_pinned() {
    let packages = ["koru", "koru-read-worker"];
    let pinned = cfg!(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ));
    assert_eq!(
        features_for_this_build(&packages).is_some(),
        pinned,
        "recognition follows the pin"
    );
    if pinned {
        let library = artifacts::library_for_this_build().unwrap();
        assert_eq!(library_for_triple(library.target), Some(library));
        if std::env::var_os(artifacts::ORT_LIB_PATH).is_none() {
            assert_eq!(
                lib_dir(Path::new("repo")),
                Some(library.lib_dir(Path::new("repo")))
            );
        }
    }
}
