// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn the_version_code_is_the_minutes_since_2026() {
    assert_eq!(android_version_code("202601010001").unwrap(), 1);
    assert_eq!(android_version_code("202601020000").unwrap(), 24 * 60);
    // The 2026-10-30 nightly: 302 days and 22:35 into the year.
    assert_eq!(
        android_version_code("202610302235").unwrap(),
        302 * 24 * 60 + 22 * 60 + 35
    );
}

#[test]
fn every_later_build_has_a_larger_version_code() {
    let builds = [
        "202610302235",
        "202610302236",
        "202610310000",
        "202712312359",
    ];
    let codes: Vec<u32> = builds
        .iter()
        .map(|build| android_version_code(build).unwrap())
        .collect();
    assert!(codes.windows(2).all(|pair| pair[0] < pair[1]), "{codes:?}");
}

#[test]
fn a_build_before_2026_or_not_a_build_time_has_no_version_code() {
    for build in [
        "202601010000",
        "202512312359",
        "2026103122",
        "20261031223a",
        "209913319999",
    ] {
        assert!(android_version_code(build).is_err(), "{build}");
    }
}

#[test]
fn the_version_code_fits_play_for_three_thousand_years() {
    // Play's ceiling is about 3,990 years of minutes: 2999 fits, under
    // i32::MAX too, and a build number past the ceiling is refused, never
    // wrapped.
    let code = android_version_code("299912312359").unwrap();
    assert!(i64::from(code) <= MAX_VERSION_CODE);
    assert!(android_version_code("999912312359").is_err());
}

fn parse(values: &[&str]) -> Result<PackageOptions, String> {
    PackageOptions::parse(values.iter().map(|value| value.to_string()))
}

const FULL: &[&str] = &[
    "--target",
    "android-aarch64",
    "--package",
    "apps/android/app/build/outputs/apk/release/app-release.apk",
    "--version",
    "0.1.5-nightly.20261031+abc1234",
    "--build",
    "202610311530",
    "--notes",
    "releases/nightly/2026-10-31.toml",
    "--key",
    "/run/secrets/minisign.key",
    "--download-base",
    "https://github.com/koruworks/mono/releases/download/nightly",
    "--out",
    "dist/nightly",
];

#[test]
fn dist_github_package_parses_every_flag() {
    let options = parse(FULL).unwrap();
    assert_eq!(options.target, "android-aarch64");
    assert_eq!(options.build_number, 202_610_311_530);
    assert_eq!(options.version, "0.1.5-nightly.20261031+abc1234");
    assert_eq!(
        options.download_base,
        "https://github.com/koruworks/mono/releases/download/nightly/"
    );
    assert!(!options.key_password_stdin);

    let mut with_password = FULL.to_vec();
    with_password.push("--key-password-stdin");
    assert!(parse(&with_password).unwrap().key_password_stdin);
}

#[test]
fn dist_github_package_refuses_what_it_does_not_understand() {
    // Every flag but --key-password-stdin is required, the version included:
    // a phone never infers its own.
    for index in (0..FULL.len()).step_by(2) {
        let mut missing = FULL.to_vec();
        missing.drain(index..index + 2);
        assert!(parse(&missing).is_err(), "without {}", FULL[index]);
    }
    let mut twice = FULL.to_vec();
    twice.extend(["--target", "android-aarch64"]);
    assert!(parse(&twice).is_err());
    let mut unknown = FULL.to_vec();
    unknown.push("--with-cloud");
    assert!(parse(&unknown).is_err());
    let mut ios = FULL.to_vec();
    ios[1] = "ios";
    assert!(parse(&ios).is_err(), "iOS is not published yet");
}

#[test]
fn the_apk_is_named_like_every_other_package() {
    // GitHub turns the `+` into a `.`, and the publish job's cleanup of the
    // previous nightly matches `koru-*-android-aarch64.*`.
    assert_eq!(
        package_asset_name("0.1.5-nightly.20261031+abc1234", "android-aarch64").as_deref(),
        Some("koru-0.1.5-nightly.20261031.abc1234-android-aarch64.apk")
    );
    assert_eq!(package_asset_name("0.1.5", "linux-x86_64"), None);
}
