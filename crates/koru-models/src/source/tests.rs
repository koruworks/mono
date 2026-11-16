// SPDX-License-Identifier: MPL-2.0

//! The sources' own suite. `koru-voice-models`' source tests, which predate the
//! extraction, run the same two sources through voice's typed descriptor.

use super::*;
use crate::fixtures;

fn url() -> Url {
    Url::parse("https://models.koru.do/v1/parakeet-1.2.0.zip").unwrap()
}

#[test]
fn a_build_with_no_downloader_refuses_with_a_code() {
    let error = NoNetworkSource
        .fetch(
            &url(),
            &fixtures::descriptor(),
            Path::new("/nowhere/out.bin"),
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.code, ModelErrorCode::Offline);
}

#[test]
fn a_directory_source_copies_the_file_the_url_names() {
    let from = tempfile::tempdir().unwrap();
    let into = tempfile::tempdir().unwrap();
    std::fs::write(
        from.path().join("parakeet-1.2.0.zip"),
        fixtures::archive_bytes(),
    )
    .unwrap();
    let destination = into.path().join("archive");
    let mut last = None;
    DirectorySource::new(from.path())
        .fetch(
            &url(),
            &fixtures::descriptor(),
            &destination,
            &mut |p| last = Some(p),
            &AtomicBool::new(false),
        )
        .unwrap();
    fixtures::descriptor().verify_archive(&destination).unwrap();
    assert_eq!(
        last.map(|p| (p.stage, p.received)),
        Some((InstallStage::Downloading, 3))
    );
}
