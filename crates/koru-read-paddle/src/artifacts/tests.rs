// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **One ONNX Runtime in the tree**: the library this crate links comes out of
/// the archive voice already pins or, on a target voice does not link yet, out
/// of the same upstream release's archive for that target, pinned here. The
/// version is the same either way.
#[test]
fn every_library_is_inside_a_pinned_archive() {
    use koru_voice_sherpa::artifacts as voice;
    assert_eq!(SHERPA_ONNX_VERSION, voice::SHERPA_ONNX_VERSION);
    for library in ONNXRUNTIME {
        let (stem, target, file_name) =
            match voice::artifact_for(library.target_os, library.target_arch) {
                Some(archive) => (archive.stem, archive.target, archive.file_name()),
                None => {
                    let archive = READER_ARCHIVES
                        .iter()
                        .find(|archive| {
                            archive.target_os == library.target_os
                                && archive.target_arch == library.target_arch
                        })
                        .unwrap_or_else(|| panic!("no pinned archive for {}", library.target));
                    (archive.stem, archive.target, archive.file_name())
                }
            };
        assert_eq!(stem, library.archive_stem);
        assert_eq!(target, library.target);
        assert_eq!(
            format!("{}.tar.bz2", library.archive_dir_name()),
            file_name,
            "the archive's top directory is its file name without the extension"
        );
    }
}

/// **A reader-only archive is one voice does not pin**, so the day Linux voice
/// adds its row to `NAMES`, this fails until the reader's copy is removed:
/// one archive, one pin.
#[test]
fn a_reader_only_archive_is_not_also_a_voice_archive() {
    use koru_voice_sherpa::artifacts as voice;
    for archive in READER_ARCHIVES {
        assert!(
            voice::artifact_for(archive.target_os, archive.target_arch).is_none(),
            "{} is pinned by voice too",
            archive.target
        );
        assert!(archive.is_pinned(), "{} is not pinned", archive.target);
        assert!(archive.stem.ends_with("-no-tts-lib") && archive.stem.contains("-static-"));
        assert!(
            archive
                .sha256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert!(
            ONNXRUNTIME
                .iter()
                .any(|library| library.archive_stem == archive.stem),
            "{} is pinned for no library",
            archive.target
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

/// Windows on ARM and Linux aarch64 have no TTS-free archive, so no library
/// and no recognition. Linux x86_64 has one.
#[test]
fn only_pinned_targets_have_a_library() {
    assert!(library_for("windows", "aarch64").is_none());
    assert!(library_for("linux", "aarch64").is_none());
    assert_eq!(
        library_for("linux", "x86_64").map(|library| library.file_name),
        Some("libonnxruntime.a")
    );
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        assert_eq!(library_for_this_build(), Some(&ONNXRUNTIME[0]));
    }
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(library_for_this_build(), Some(&ONNXRUNTIME[1]));
    }
}

#[test]
fn the_linux_library_is_extracted_where_ort_lib_path_points() {
    let library = library_for("linux", "x86_64").unwrap();
    let root = Path::new("repo");
    assert_eq!(
        library.lib_dir(root),
        root.join("third-party/sherpa-onnx/1.13.6")
            .join("sherpa-onnx-v1.13.6-linux-x64-static-no-tts-lib")
            .join("lib")
    );
    assert_eq!(
        library.member(),
        "sherpa-onnx-v1.13.6-linux-x64-static-no-tts-lib/lib/libonnxruntime.a"
    );
}

/// The provisioning scripts carry the same rows, the library's and a
/// reader-only archive's: each pin is declared in three places and a mismatch
/// fails here, not in a release.
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
    for archive in READER_ARCHIVES {
        for script in scripts {
            assert!(
                script.contains(archive.sha256),
                "a script lacks the archive pin"
            );
            assert!(script.contains(&archive.size.to_string()));
            assert!(script.contains(archive.stem));
        }
    }
}
