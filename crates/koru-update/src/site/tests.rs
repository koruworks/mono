// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    PackageFormat, ReleaseChannel, ReleaseNotes, UpdateManifest, UpdatePackage, build_info,
};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

fn target() -> UpdateTarget {
    let info = build_info();
    UpdateTarget {
        channel: ReleaseChannel::Nightly,
        platform: info.platform.to_string(),
        architecture: info.architecture.to_string(),
    }
}

fn manifest_json() -> Vec<u8> {
    let target = target();
    let manifest = UpdateManifest {
        schema_version: 1,
        channel: target.channel,
        platform: target.platform,
        architecture: target.architecture,
        version: "0.0.0-test.9".into(),
        build_number: 9,
        published_at: "2026-11-18T00:00:00Z".into(),
        commit: None,
        source: None,
        minimum_updater_version: None,
        minimum_supported_build: None,
        required: false,
        release_notes: ReleaseNotes {
            title: "Test".into(),
            summary: vec!["A test build".into()],
            url: None,
        },
        package: UpdatePackage {
            url: "https://example.invalid/package".into(),
            file_name: "package".into(),
            size: 1,
            sha256: "a".repeat(64),
            signature: "test signature".into(),
            format: if cfg!(target_os = "linux") {
                PackageFormat::TarGz
            } else {
                PackageFormat::Zip
            },
        },
    };
    serde_json::to_vec(&manifest).unwrap()
}

/// What one fake server saw: each request's first line and its headers,
/// lowercased.
type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// A loopback HTTP server answering every request with `respond(path)`.
/// Debug builds admit plain HTTP on loopback (`debug_override_allowed`).
type Reply = (u16, Vec<(&'static str, String)>, Vec<u8>);

fn serve(respond: fn(&str) -> Reply) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut headers = Vec::new();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                    break;
                }
                headers.push(header.trim().to_ascii_lowercase());
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            log.lock().unwrap().push((line.trim().to_owned(), headers));
            let (status, extra, body) = respond(&path);
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in extra {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (base, seen)
}

#[test]
fn only_the_production_feeds_route_through_the_site() {
    assert!(routes_through_site("https://updates.koru.do/"));
    assert!(routes_through_site("https://updates.koru.do"));
    assert!(routes_through_site(
        "https://github.com/koruworks/mono/releases/download/nightly/"
    ));
    assert!(routes_through_site(
        "https://github.com/koruworks/mono/releases/download/nightly"
    ));
    for local in [
        "http://127.0.0.1:8080/",
        "https://updates.invalid/",
        "https://github.com/someone/else/releases/download/nightly/",
        "https://updates.koru.do/staging/",
        "not a url",
    ] {
        assert!(!routes_through_site(local), "{local}");
    }
}

#[test]
fn the_site_url_is_the_channels_layout_with_first_today_when_set() {
    let target = UpdateTarget {
        channel: ReleaseChannel::Stable,
        platform: "windows".into(),
        architecture: "x86_64".into(),
    };
    assert_eq!(
        site_manifest_url(SITE_UPDATE_BASE, &target, true)
            .unwrap()
            .as_str(),
        "https://www.koru.do/v1/app/update/channels/stable/windows-x86_64.json?first_today=1"
    );
    assert_eq!(
        site_manifest_url(SITE_UPDATE_BASE, &target, false)
            .unwrap()
            .as_str(),
        "https://www.koru.do/v1/app/update/channels/stable/windows-x86_64.json"
    );
}

#[test]
fn first_today_is_once_per_utc_day() {
    let day = utc_day(UNIX_EPOCH + Duration::from_secs(20_000 * 86_400 + 5));
    assert_eq!(day, 20_000);
    assert_eq!(
        utc_day(UNIX_EPOCH + Duration::from_secs(20_001 * 86_400 - 1)),
        20_000
    );
    assert!(first_today(None, day));
    assert!(first_today(Some(day - 1), day));
    assert!(!first_today(Some(day), day));
}

#[test]
fn a_site_that_answers_serves_the_check_and_the_origin_is_never_asked() {
    let (site, site_seen) = serve(|_| (200, vec![("ETag", "\"v9\"".into())], manifest_json()));
    let (origin, origin_seen) = serve(|_| (500, vec![], Vec::new()));
    let check = check_via(
        Some(&site),
        &origin,
        "Koru/0.0.0 (build 1)",
        &target(),
        None,
        true,
    );
    assert!(check.site_answered);
    assert!(matches!(
        check.result,
        Ok(FetchResult::Manifest { ref etag, .. }) if etag.as_deref() == Some("\"v9\"")
    ));
    let seen = site_seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (line, headers) = &seen[0];
    assert!(
        line.starts_with("GET /channels/nightly/") && line.contains(".json?first_today=1 "),
        "{line}"
    );
    assert!(
        headers
            .iter()
            .any(|h| h == "user-agent: koru/0.0.0 (build 1)")
    );
    assert!(origin_seen.lock().unwrap().is_empty());
}

#[test]
fn a_not_modified_from_the_site_is_an_answer_and_carries_the_etag() {
    let (site, site_seen) = serve(|_| (304, vec![], Vec::new()));
    let (origin, _) = serve(|_| (500, vec![], Vec::new()));
    let check = check_via(
        Some(&site),
        &origin,
        "Koru",
        &target(),
        Some("\"v9\""),
        false,
    );
    assert!(check.site_answered);
    assert_eq!(check.result, Ok(FetchResult::NotModified));
    let seen = site_seen.lock().unwrap();
    assert!(!seen[0].0.contains("first_today"), "{}", seen[0].0);
    assert!(seen[0].1.iter().any(|h| h == "if-none-match: \"v9\""));
}

#[test]
fn a_failing_site_falls_back_to_the_origin_in_the_same_check() {
    for respond in [
        (|_: &str| (503, vec![], Vec::new())) as fn(&str) -> _,
        |_| (404, vec![], Vec::new()),
        |_| (200, vec![], b"<html>not a manifest</html>".to_vec()),
    ] {
        let (site, _) = serve(respond);
        let (origin, origin_seen) = serve(|_| (200, vec![], manifest_json()));
        let check = check_via(Some(&site), &origin, "Koru", &target(), None, true);
        assert!(!check.site_answered);
        assert!(matches!(check.result, Ok(FetchResult::Manifest { .. })));
        let seen = origin_seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(
            !seen[0].0.contains("first_today"),
            "the origin is never told"
        );
    }
}

#[test]
fn an_unreachable_site_falls_back_to_the_origin() {
    // A port nothing listens on: bind, read the port, drop the listener.
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/", listener.local_addr().unwrap())
    };
    let (origin, _) = serve(|_| (200, vec![], manifest_json()));
    let check = check_via(Some(&dead), &origin, "Koru", &target(), None, true);
    assert!(!check.site_answered);
    assert!(matches!(check.result, Ok(FetchResult::Manifest { .. })));
}

#[test]
fn without_a_site_the_origin_is_fetched_directly() {
    let (origin, origin_seen) = serve(|_| (200, vec![], manifest_json()));
    let check = check_via(None, &origin, "Koru", &target(), None, true);
    assert!(!check.site_answered);
    assert!(matches!(check.result, Ok(FetchResult::Manifest { .. })));
    assert_eq!(origin_seen.lock().unwrap().len(), 1);
}
