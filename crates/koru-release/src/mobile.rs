// SPDX-License-Identifier: MPL-2.0

//! **The phones on the GitHub release** (docs/features/github-release/
//! TECHNICAL.final.md §5.4, rung 2): the Android APK beside the desktop
//! packages on the same page, built by Gradle rather than by `package::build`.
//!
//! Two verbs, and neither builds anything nor talks to GitHub:
//!
//! - `android-version-code --build <yyyymmddHHMM>` prints the APK's
//!   `versionCode` for a build number. The build number itself does not fit:
//!   Android holds the code in a signed 32-bit integer, and Play refuses
//!   anything above 2100000000.
//! - `dist-github-package <nightly|stable> --target android --package <apk> …`
//!   takes the APK Gradle built and signed and writes what the publish job
//!   uploads: the package under its release name, its `.minisig`, and the
//!   release body every build writes. **No feed manifest**: nothing on a
//!   phone reads one yet (the update notice is rung 3), and the publish job
//!   knows a build without one is a phone's.

use crate::nightly::{
    self, DIST_GITHUB_PACKAGE_USAGE, GithubBuild, download_base_url, github_asset_name,
    parse_build_time, read_notes, release_body,
};
use crate::{require_clean_tree, sign_archive, source, validate_release_version};
use chrono::{DateTime, TimeZone, Utc};
use koru_update::ReleaseChannel;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// The targets this verb publishes, and the extension each package carries.
/// **`android-aarch64` names the one universal APK** (D12, D25): it carries
/// both ABIs, and the architecture is in the name only so it has the
/// `<platform>-<arch>` shape every reader of a target already parses — the
/// publish job's cleanup (`koru-*-android-aarch64.*`) and the update feed's
/// `nightly-android-aarch64.json` among them.
const MOBILE_TARGETS: &[(&str, &str)] = &[("android-aarch64", "apk")];

/// The instant versionCode 0 would name: 2026-01-01 00:00 UTC, the year the
/// nightly's `yyyymmddHHMM` build numbers start (N11).
fn version_code_epoch() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("a fixed, valid instant")
}

/// Play's ceiling for `versionCode`.
const MAX_VERSION_CODE: i64 = 2_100_000_000;

/// **The APK's `versionCode`: the minutes since 2026-01-01 00:00 UTC** of the
/// build number, so it grows with every build, as the number it comes from
/// does, and a later nightly always installs over an earlier one. It stays
/// under Play's ceiling for some four thousand years. The Windows version
/// field makes the same move for its 16 bits (NIGHTLY.rev2 §7), with hours.
///
/// A build number from before 2026 has no code and is refused, never wrapped.
pub(crate) fn android_version_code(build: &str) -> Result<u32, String> {
    let (_, time) = parse_build_time(build)?;
    let minutes = (time - version_code_epoch()).num_minutes();
    if !(1..=MAX_VERSION_CODE).contains(&minutes) {
        return Err(format!(
            "build {build} gives Android no versionCode: it must fall after 2026-01-01 00:00 UTC, and within {MAX_VERSION_CODE} minutes of it"
        ));
    }
    u32::try_from(minutes).map_err(|error| error.to_string())
}

pub(crate) const ANDROID_VERSION_CODE_USAGE: &str =
    "usage: koru-release android-version-code --build <yyyymmddHHMM>";

pub(crate) fn version_code_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let build = match (args.next().as_deref(), args.next(), args.next()) {
        (Some("--build"), Some(build), None) => build,
        _ => return Err(ANDROID_VERSION_CODE_USAGE.into()),
    };
    println!("{}", android_version_code(&build)?);
    Ok(())
}

/// What `dist-github-package` was asked to write.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PackageOptions {
    pub target: String,
    pub package: PathBuf,
    pub build_number: u64,
    pub released: DateTime<Utc>,
    pub notes: PathBuf,
    pub key: PathBuf,
    pub key_password_stdin: bool,
    pub download_base: String,
    pub out: PathBuf,
    pub version: String,
}

impl PackageOptions {
    /// Strict, as `dist-github`'s parser is.
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut target = None;
        let mut package = None;
        let mut build = None;
        let mut notes = None;
        let mut key = None;
        let mut key_password_stdin = false;
        let mut download_base = None;
        let mut out = None;
        let mut version = None;
        while let Some(arg) = args.next() {
            let slot = match arg.as_str() {
                "--key-password-stdin" => {
                    key_password_stdin = true;
                    continue;
                }
                "--target" => &mut target,
                "--package" => &mut package,
                "--build" => &mut build,
                "--notes" => &mut notes,
                "--key" => &mut key,
                "--download-base" => &mut download_base,
                "--out" => &mut out,
                "--version" => &mut version,
                _ => {
                    return Err(format!(
                        "unexpected argument: {arg}\n{DIST_GITHUB_PACKAGE_USAGE}"
                    ));
                }
            };
            let value = args
                .next()
                .ok_or_else(|| format!("{arg} needs a value\n{DIST_GITHUB_PACKAGE_USAGE}"))?;
            if slot.replace(value).is_some() {
                return Err(format!("{arg} given twice\n{DIST_GITHUB_PACKAGE_USAGE}"));
            }
        }
        let required = |value: Option<String>, flag: &str| {
            value.ok_or_else(|| format!("{flag} is required\n{DIST_GITHUB_PACKAGE_USAGE}"))
        };
        let target = required(target, "--target")?;
        if package_extension(&target).is_none() {
            return Err(format!(
                "--target must be one of {}, not {target:?}",
                MOBILE_TARGETS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let (build_number, released) = parse_build_time(&required(build, "--build")?)?;
        Ok(Self {
            target,
            package: required(package, "--package")?.into(),
            build_number,
            released,
            notes: required(notes, "--notes")?.into(),
            key: required(key, "--key")?.into(),
            key_password_stdin,
            download_base: download_base_url(&required(download_base, "--download-base")?)?,
            out: required(out, "--out")?.into(),
            // **Named, never inferred**: the plan job computed it once for
            // every platform of the release, and a phone that guessed its own
            // would be drawn in amber beside the desktop it was built with.
            version: required(version, "--version")?,
        })
    }
}

fn package_extension(target: &str) -> Option<&'static str> {
    MOBILE_TARGETS
        .iter()
        .find(|(name, _)| *name == target)
        .map(|(_, extension)| *extension)
}

/// The package's name on the release: `koru-<version>-<target>.<ext>`, made
/// safe by GitHub's own renaming rule, as every other package is.
pub(crate) fn package_asset_name(version: &str, target: &str) -> Option<String> {
    package_extension(target)
        .map(|extension| github_asset_name(&format!("koru-{version}-{target}.{extension}")))
}

pub(crate) fn dist_github_package(
    root: &Path,
    channel: ReleaseChannel,
    options: PackageOptions,
) -> Result<(), String> {
    if !matches!(channel, ReleaseChannel::Nightly | ReleaseChannel::Stable) {
        return Err(format!(
            "only the nightly and stable are published to GitHub so far, not {channel}"
        ));
    }
    validate_release_version(channel, &options.version)?;
    let asset = package_asset_name(&options.version, &options.target)
        .ok_or_else(|| format!("no package name for target {}", options.target))?;
    let extension = package_extension(&options.target).unwrap_or_default();
    if options.package.extension().and_then(|value| value.to_str()) != Some(extension) {
        return Err(format!(
            "--package {} is not a .{extension}",
            options.package.display()
        ));
    }
    let descriptor = read_notes(&options.notes)?;
    let (secret, public_key) = nightly::open_key_file(&options.key, options.key_password_stdin)?;
    // The package names the commit it was built from (its notices do), and
    // the body says the same; a tree that moved since is refused, as
    // `dist-github` refuses one.
    let commit = require_clean_tree(root, None)?;
    if source::url(&commit).is_none() {
        return Err(format!(
            "a published package must name its source commit, and {commit:?} is not a full commit id"
        ));
    }

    let bytes = fs::read(&options.package)
        .map_err(|error| format!("--package {}: {error}", options.package.display()))?;
    if bytes.is_empty() {
        return Err(format!("--package {} is empty", options.package.display()));
    }
    let sha256 = hex(&Sha256::digest(&bytes));
    let signature = sign_archive(&secret, &options.package, &public_key, &options.version)?;
    nightly::verify_signature(&options.package, &signature, &public_key)?;

    let released_at = options
        .released
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let build = GithubBuild {
        channel,
        target: &options.target,
        version: &options.version,
        build_number: options.build_number,
        released_at: &released_at,
        commit: &commit,
        notes: &descriptor,
        download_base: &options.download_base,
        asset: &asset,
        size: bytes.len() as u64,
        sha256: &sha256,
        signature: &signature,
    };

    fs::create_dir_all(&options.out)
        .map_err(|error| format!("{}: {error}", options.out.display()))?;
    let signature_name = format!("{asset}.minisig");
    let body_name = "release-body.md";
    fs::write(options.out.join(&asset), &bytes)
        .map_err(|error| format!("{}: {error}", options.out.join(&asset).display()))?;
    nightly::write(&options.out.join(&signature_name), signature.as_bytes())?;
    nightly::write(
        &options.out.join(body_name),
        release_body(&build).as_bytes(),
    )?;
    require_clean_tree(root, Some(&commit))?;

    println!(
        "[Koru] {channel} {}, build {}, for {}",
        options.version, options.build_number, options.target
    );
    println!("[Koru] sha256: {sha256}");
    println!("[Koru] wrote into {}:", options.out.display());
    for name in [asset.as_str(), signature_name.as_str(), body_name] {
        println!("{name}");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;
