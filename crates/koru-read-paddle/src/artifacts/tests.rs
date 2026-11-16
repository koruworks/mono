// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **One ONNX Runtime in the tree**: the library this crate links comes out of
/// the archive voice already pins, and the version is the same.
#[test]
fn every_library_is_inside_an_archive_voice_pins() {
    use koru_voice_sherpa::artifacts as voice;
    assert_eq!(SHERPA_ONNX_VERSION, voice::SHERPA_ONNX_VERSION);
    for library in ONNXRUNTIME {
        let archive = voice::artifact_for(library.target_os, library.target_arch)
            .unwrap_or_else(|| panic!("no voice archive for {}", library.target));
        assert_eq!(archive.stem, library.archive_stem);
        assert_eq!(archive.target, library.target);
        assert_eq!(
            format!("{}.tar.bz2", library.archive_dir_name()),
            archive.file_name(),
            "the archive's top directory is its file name without the extension"
        );
    }
}

#[test]
fn every_library_is_pinned_by_a_lowercase_sha256_and_a_size() {
    for library in ONNXRUNTIME {
        assert!(library.is_pinned(), "{} is not pinned", library.target);
        assert!(
            library
                .sha256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }
}

#[test]
fn the_library_is_extracted_where_ort_lib_path_points() {
    let library = library_for("windows", "x86_64").unwrap();
    let root = Path::new("repo");
    assert_eq!(
        library.lib_dir(root),
        root.join("third-party/sherpa-onnx/1.13.6")
            .join("sherpa-onnx-v1.13.6-win-x64-static-MT-Release-no-tts-lib")
            .join("lib")
    );
    assert_eq!(
        library.member(),
        "sherpa-onnx-v1.13.6-win-x64-static-MT-Release-no-tts-lib/lib/onnxruntime.lib"
    );
    assert_eq!(ORT_LIB_PATH, "ORT_LIB_PATH");
}

/// Linux and Windows on ARM have no archive, so no library and no
/// recognition.
#[test]
fn only_pinned_targets_have_a_library() {
    assert!(library_for("linux", "x86_64").is_none());
    assert!(library_for("windows", "aarch64").is_none());
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        assert_eq!(library_for_this_build(), Some(&ONNXRUNTIME[0]));
    }
}

/// The provisioning scripts carry the same row: the pin is declared in three
/// places and a mismatch fails here, not in a release.
#[test]
fn the_fetch_scripts_carry_the_same_pin() {
    let scripts = [
        include_str!("../../../../scripts/setup/fetch-voice-runtime.ps1"),
        include_str!("../../../../scripts/setup/fetch-voice-runtime.sh"),
    ];
    for library in ONNXRUNTIME {
        for script in scripts {
            assert!(script.contains(library.sha256), "a script lacks the pin");
            assert!(script.contains(&library.size.to_string()));
            assert!(script.contains(library.file_name));
        }
    }
}
