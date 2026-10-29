// SPDX-License-Identifier: MPL-2.0

//! What can be checked without a compiler: the version arithmetic and the
//! script the resource compiler is handed. Actually running `rc.exe` is the
//! build's job, and a test that shelled out to it would fail on a machine with
//! no SDK for a reason that has nothing to do with the code.

use super::*;

#[test]
fn package_identity_is_channel_isolated_and_stable() {
    let stable = notification_identity("stable", "CN=Koru");
    let beta = notification_identity("beta", "CN=Koru");
    assert_eq!(stable.name, "Koru.Desktop");
    assert_eq!(stable.application_id, "KoruAgent");
    assert_ne!(stable.name, beta.name);
    assert_ne!(stable.toast_activator_clsid, beta.toast_activator_clsid);
}

#[test]
fn executable_manifest_carries_sparse_package_identity() {
    let resource = Resource::new("Koru Agent", "koru-agent.exe", "koru.ico")
        .package_identity(notification_identity("nightly", "CN=Koru & Company"));
    let manifest = assembly_manifest(&resource);
    assert!(manifest.contains("urn:schemas-microsoft-com:msix.v1"));
    assert!(manifest.contains("packageName=\"Koru.Desktop.Nightly\""));
    assert!(manifest.contains("applicationId=\"KoruAgent\""));
    assert!(manifest.contains("publisher=\"CN=Koru &amp; Company\""));
}

#[test]
fn ordinary_build_has_no_package_identity() {
    let resource = Resource::new("Koru Agent", "koru-agent.exe", "koru.ico");
    assert!(!assembly_manifest(&resource).contains("schemas.microsoft.com:msix"));
}

#[test]
fn a_plain_version_splits_into_three_numbers() {
    assert_eq!(numeric_core("1.2.3"), (1, 2, 3));
    assert_eq!(numeric_core("v1.2.3"), (1, 2, 3));
}

#[test]
fn a_prerelease_suffix_is_dropped_rather_than_breaking_the_parse() {
    // `FILEVERSION` is four numbers and will take nothing else, so the suffix
    // has to go somewhere — it travels in the `FileVersion` *string*.
    assert_eq!(numeric_core("0.2.0-alpha"), (0, 2, 0));
    assert_eq!(numeric_core("0.0.3-nightly.20260805+4348140"), (0, 0, 3));
    assert_eq!(numeric_core("1.0.0+build.7"), (1, 0, 0));
}

#[test]
fn a_version_that_makes_no_sense_is_zeroes_rather_than_a_panic() {
    // A build must not fail because somebody set KORU_VERSION to a word.
    assert_eq!(numeric_core(""), (0, 0, 0));
    assert_eq!(numeric_core("nightly"), (0, 0, 0));
    assert_eq!(numeric_core("1.x.3"), (1, 0, 3));
    // Short versions fill from the left; missing fields are zero.
    assert_eq!(numeric_core("2"), (2, 0, 0));
    assert_eq!(numeric_core("2.5"), (2, 5, 0));
}

#[test]
fn the_description_never_carries_the_version() {
    // **The bug this crate exists to prevent.** The notification area
    // remembers "always show this icon" against `FileDescription`, so a
    // description carrying a version counts as a different application on
    // every build and the tray icon vanishes.
    let resource = Resource::new("Koru Store", "koru-store-server.exe", "koru.ico")
        .versioned("0.0.3-nightly.20260805+4348140".into(), 4348140);

    assert_eq!(resource.file_description, "Koru Store");
    assert_eq!(resource.product_name, "Koru Store");
    assert!(!resource.file_description.contains(char::is_numeric));
    // ...while the version itself is still carried, where nothing keys off it.
    assert!(resource.version.contains("nightly"));
}

#[test]
fn the_script_names_every_icon_at_its_ordinal() {
    let out = std::env::temp_dir();
    let resource = Resource::new("Koru", "koru.exe", "C:/x/assets/koru.ico")
        .icon(2, "C:/x/assets/document.ico");
    let path = write_script(&out, "koru-winres-test", &resource);
    let script = std::fs::read_to_string(&path).unwrap();

    assert!(
        script.contains(r#"1 ICON "C:/x/assets/koru.ico""#),
        "{script}"
    );
    assert!(
        script.contains(r#"2 ICON "C:/x/assets/document.ico""#),
        "{script}"
    );
    // Ordinal 1 is the application icon, so it has to come first in the file.
    let first = script.find("1 ICON").unwrap();
    let second = script.find("2 ICON").unwrap();
    assert!(first < second, "ordinal 1 must stay the application icon");

    let _ = std::fs::remove_file(path);
}

#[test]
fn backslashes_never_reach_the_resource_compiler() {
    // rc.exe reads a backslash inside a quoted path as a C escape, so
    // `C:\assets\koru.ico` becomes an unprintable mess and the icon is lost
    // with a warning nobody reads.
    let out = std::env::temp_dir();
    let resource = Resource::new("Koru", "koru.exe", r"C:\dev\koru\assets\koru.ico");
    let path = write_script(&out, "koru-winres-backslash", &resource);
    let script = std::fs::read_to_string(&path).unwrap();

    let icon_line = script.lines().find(|l| l.contains("ICON")).unwrap();
    assert!(!icon_line.contains('\\'), "{icon_line}");
    assert!(
        icon_line.contains("C:/dev/koru/assets/koru.ico"),
        "{icon_line}"
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn the_version_block_carries_the_full_version_and_a_numeric_fileversion() {
    let out = std::env::temp_dir();
    let resource = Resource::new("Koru Store", "koru-store-server.exe", "koru.ico")
        .versioned("0.2.0-alpha".into(), 77);
    let path = write_script(&out, "koru-winres-version", &resource);
    let script = std::fs::read_to_string(&path).unwrap();

    assert!(script.contains("FILEVERSION 0,2,0,77"), "{script}");
    assert!(
        script.contains(r#"VALUE "FileVersion", "0.2.0-alpha\0""#),
        "{script}"
    );
    assert!(
        script.contains(r#"VALUE "OriginalFilename", "koru-store-server.exe\0""#),
        "{script}"
    );
    assert!(
        script.contains(r#"VALUE "InternalName", "koru-store-server\0""#),
        "{script}"
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn an_allocated_build_number_is_its_own_field() {
    assert_eq!(build_field(0), Some(0));
    assert_eq!(build_field(42), Some(42));
    assert_eq!(build_field(u16::MAX as u64), Some(u16::MAX));
}

#[test]
fn a_nightly_build_time_is_the_hours_since_2026() {
    assert_eq!(build_field(202_601_010_000), Some(0));
    assert_eq!(build_field(202_601_010_059), Some(0));
    assert_eq!(build_field(202_601_010_100), Some(1));
    // 2026-10-29 is day 301 of the year.
    assert_eq!(build_field(202_610_291_530), Some(301 * 24 + 15));
    // Leap day 2028 (after 2026 and 2027), and the year after it.
    assert_eq!(
        build_field(202_802_290_000),
        Some((365 + 365 + 31 + 28) * 24)
    );
    assert_eq!(build_field(202_901_010_000), Some((365 + 365 + 366) * 24));
    // Later is always larger, across a month and a year boundary.
    assert!(build_field(202_611_010_000) > build_field(202_610_312_300));
    assert!(build_field(202_701_010_000) > build_field(202_612_312_300));
}

#[test]
fn a_number_with_no_field_is_refused_not_wrapped() {
    // Too large and not a time.
    assert_eq!(build_field(65_536), None);
    assert_eq!(build_field(999_999_999_999), None);
    // Times that are not real minutes.
    assert_eq!(build_field(202_613_010_000), None);
    assert_eq!(build_field(202_602_300_000), None);
    assert_eq!(build_field(202_610_292_400), None);
    assert_eq!(build_field(202_610_291_560), None);
    // Before 2026, and after the field runs out (mid-2033).
    assert_eq!(build_field(202_512_312_359), None);
    assert_eq!(build_field(203_401_010_000), None);
}

#[test]
fn the_package_version_is_four_sixteen_bit_fields() {
    assert_eq!(
        package_version("1.2.3-beta.4", 42).as_deref(),
        Ok("1.2.3.42")
    );
    assert_eq!(
        package_version("0.1.4-nightly.20261029+abc1234", 202_610_291_530).as_deref(),
        Ok("0.1.4.7239")
    );
    assert!(package_version("1.2.3", 65_536).is_err());
    assert!(package_version("70000.0.0", 1).is_err());
}
