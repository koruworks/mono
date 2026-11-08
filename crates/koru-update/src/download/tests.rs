// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{PackageFormat, ReleaseChannel, ReleaseNotes, UpdatePackage};
use minisign::{KeyPair, sign};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Cursor};
use std::net::TcpListener;

/// One HTTP/1.1 answer on a loopback port, then the listener closes.
fn serve_once(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 2 {
            line.clear();
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    });
    format!("http://{address}/koru-test-android-aarch64.apk")
}

fn signed(bytes: &[u8]) -> (String, String) {
    let KeyPair { pk, sk } = KeyPair::generate_unencrypted_keypair().unwrap();
    let signature = sign(
        Some(&pk),
        &sk,
        Cursor::new(bytes),
        Some("Koru test"),
        Some("Koru update package"),
    )
    .unwrap()
    .to_string();
    (signature, pk.to_box().unwrap().to_string())
}

fn manifest(url: String, bytes: &[u8], signature: String) -> UpdateManifest {
    UpdateManifest {
        schema_version: 1,
        channel: ReleaseChannel::Nightly,
        platform: "android".into(),
        architecture: "aarch64".into(),
        version: "0.1.5-nightly.20261031+abc1234".into(),
        build_number: 202_610_311_530,
        published_at: "2026-10-31T15:30:00Z".into(),
        commit: Some("abc1234".into()),
        source: None,
        minimum_updater_version: Some(1),
        minimum_supported_build: None,
        required: false,
        release_notes: ReleaseNotes {
            title: "Nightly".into(),
            summary: vec!["One change".into()],
            url: None,
        },
        package: UpdatePackage {
            url,
            file_name: "koru-test-android-aarch64.apk".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            signature,
            format: PackageFormat::Apk,
        },
    }
}

#[test]
fn a_signed_package_is_downloaded_verified_and_renamed() {
    let bytes = b"an APK, as far as this test knows".to_vec();
    let (signature, public_key) = signed(&bytes);
    let manifest = manifest(serve_once(bytes.clone()), &bytes, signature);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("updates").join("koru.apk");

    let verified =
        download_verified_package(&manifest, &destination, &public_key, "koru-test").unwrap();

    assert_eq!(verified.size, bytes.len() as u64);
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(!part_path(&destination).exists());
}

#[test]
fn a_package_signed_by_another_key_is_refused_and_removed() {
    let bytes = b"bytes somebody else signed".to_vec();
    let (signature, _) = signed(&bytes);
    let (_, our_key) = signed(b"anything");
    let manifest = manifest(serve_once(bytes.clone()), &bytes, signature);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("koru.apk");

    let error =
        download_verified_package(&manifest, &destination, &our_key, "koru-test").unwrap_err();

    assert!(error.contains("signature"), "{error}");
    assert!(!destination.exists());
    assert!(!part_path(&destination).exists());
}

#[test]
fn bytes_that_do_not_match_the_manifest_are_refused() {
    let bytes = b"the published bytes".to_vec();
    let (signature, public_key) = signed(&bytes);
    let tampered = b"the published bytez".to_vec();
    let manifest = manifest(serve_once(tampered), &bytes, signature);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("koru.apk");

    let error =
        download_verified_package(&manifest, &destination, &public_key, "koru-test").unwrap_err();

    assert!(error.contains("SHA-256"), "{error}");
    assert!(!destination.exists());
}

#[test]
fn a_build_without_a_key_downloads_nothing() {
    let bytes = b"x".to_vec();
    let manifest = manifest("https://github.com/x".into(), &bytes, "sig".into());
    let dir = tempfile::tempdir().unwrap();
    let error =
        download_verified_package(&manifest, &dir.path().join("koru.apk"), " ", "koru-test")
            .unwrap_err();
    assert!(error.contains("public key"), "{error}");
}

#[test]
fn plain_http_off_loopback_is_refused() {
    let bytes = b"x".to_vec();
    let (signature, public_key) = signed(&bytes);
    let manifest = manifest("http://example.com/koru.apk".into(), &bytes, signature);
    let dir = tempfile::tempdir().unwrap();
    let error =
        download_verified_package(&manifest, &dir.path().join("koru.apk"), &public_key, "t")
            .unwrap_err();
    assert!(error.contains("HTTPS"), "{error}");
}
