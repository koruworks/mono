// SPDX-License-Identifier: MPL-2.0

use koru_update::{ReleaseChannel, UpdateManifest};
use semver::{Prerelease, Version};

pub fn suggest_version(
    channel: ReleaseChannel,
    stable: Option<&UpdateManifest>,
    beta: Option<&UpdateManifest>,
    nightly: Option<&UpdateManifest>,
    date: &str,
    short_commit: &str,
) -> Result<String, String> {
    match channel {
        ReleaseChannel::Stable => {
            let mut version = manifest_version(stable).unwrap_or_else(|| Version::new(0, 1, 0));
            if stable.is_some() {
                version.patch += 1;
                version.pre = Prerelease::EMPTY;
                version.build = semver::BuildMetadata::EMPTY;
            }
            Ok(version.to_string())
        }
        ReleaseChannel::Beta => suggest_beta(stable, beta),
        ReleaseChannel::Nightly => suggest_nightly(stable, beta, nightly, date, short_commit),
        ReleaseChannel::Internal => {
            Err("internal is a developer flavor only and cannot be published".into())
        }
    }
}

fn suggest_beta(
    stable: Option<&UpdateManifest>,
    beta: Option<&UpdateManifest>,
) -> Result<String, String> {
    let stable = manifest_version(stable).unwrap_or_else(|| Version::new(0, 0, 0));
    if let Some(current) = manifest_version(beta) {
        if core(&current) > core(&stable) {
            if let Some(number) = current.pre.as_str().strip_prefix("beta.") {
                let number: u64 = number
                    .parse()
                    .map_err(|_| format!("current beta version {} does not use beta.N", current))?;
                let mut next = current;
                next.pre = Prerelease::new(&format!("beta.{}", number + 1))
                    .map_err(|error| error.to_string())?;
                next.build = semver::BuildMetadata::EMPTY;
                return Ok(next.to_string());
            }
        }
    }
    let mut next = stable;
    next.patch += 1;
    next.pre = Prerelease::new("beta.0").expect("fixed prerelease is valid");
    next.build = semver::BuildMetadata::EMPTY;
    Ok(next.to_string())
}

fn suggest_nightly(
    stable: Option<&UpdateManifest>,
    beta: Option<&UpdateManifest>,
    nightly: Option<&UpdateManifest>,
    date: &str,
    short_commit: &str,
) -> Result<String, String> {
    // The higher of the two lines, not beta whenever there is one: a beta
    // channel left behind (0.1.3 on the update origin while stable reached
    // 0.1.8 on GitHub) would otherwise name nightlies below the stable they
    // follow.
    let mut base = manifest_version(beta)
        .into_iter()
        .chain(manifest_version(stable))
        .max()
        .unwrap_or_else(|| Version::new(0, 0, 0));
    base.patch += 1;
    base.pre = Prerelease::EMPTY;
    base.build = semver::BuildMetadata::EMPTY;

    // Several commits on one day keep the same semantic line; the commit makes
    // every immutable release name distinct. A prior nightly never advances
    // the base by itself.
    if let Some(current) = manifest_version(nightly) {
        let current_core = core(&current);
        if current_core > core(&base) {
            base.major = current_core.0;
            base.minor = current_core.1;
            base.patch = current_core.2;
        }
    }
    let candidate = format!("{base}-nightly.{date}+{short_commit}");
    Version::parse(&candidate).map_err(|error| format!("invalid inferred nightly: {error}"))?;
    Ok(candidate)
}

fn manifest_version(manifest: Option<&UpdateManifest>) -> Option<Version> {
    manifest.and_then(|manifest| Version::parse(&manifest.version).ok())
}

fn core(version: &Version) -> (u64, u64, u64) {
    (version.major, version.minor, version.patch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_update::{PackageFormat, ReleaseNotes, UpdatePackage};

    fn manifest(channel: ReleaseChannel, version: &str) -> UpdateManifest {
        UpdateManifest {
            schema_version: 1,
            channel,
            platform: "windows".into(),
            architecture: "x86_64".into(),
            version: version.into(),
            build_number: 1,
            published_at: "now".into(),
            commit: None,
            source: None,
            minimum_updater_version: Some(1),
            minimum_supported_build: None,
            required: false,
            release_notes: ReleaseNotes {
                title: "notes".into(),
                summary: vec!["one".into()],
                url: None,
            },
            package: UpdatePackage {
                url: "https://updates.koru.do/package.zip".into(),
                file_name: "package.zip".into(),
                size: 1,
                sha256: "a".repeat(64),
                signature: "signature".into(),
                format: PackageFormat::Zip,
            },
        }
    }

    #[test]
    fn beta_starts_after_stable_and_then_increments_counter() {
        let stable = manifest(ReleaseChannel::Stable, "0.9.0");
        assert_eq!(
            suggest_version(ReleaseChannel::Beta, Some(&stable), None, None, "", "").unwrap(),
            "0.9.1-beta.0"
        );
        let beta = manifest(ReleaseChannel::Beta, "0.9.1-beta.0");
        assert_eq!(
            suggest_version(
                ReleaseChannel::Beta,
                Some(&stable),
                Some(&beta),
                None,
                "",
                ""
            )
            .unwrap(),
            "0.9.1-beta.1"
        );
    }

    #[test]
    fn stable_catching_beta_starts_a_new_beta_line() {
        let stable = manifest(ReleaseChannel::Stable, "0.9.1");
        let beta = manifest(ReleaseChannel::Beta, "0.9.1-beta.8");
        assert_eq!(
            suggest_version(
                ReleaseChannel::Beta,
                Some(&stable),
                Some(&beta),
                None,
                "",
                ""
            )
            .unwrap(),
            "0.9.2-beta.0"
        );
    }

    #[test]
    fn nightly_is_the_patch_after_beta() {
        let stable = manifest(ReleaseChannel::Stable, "0.9.0");
        let beta = manifest(ReleaseChannel::Beta, "0.9.1-beta.2");
        assert_eq!(
            suggest_version(
                ReleaseChannel::Nightly,
                Some(&stable),
                Some(&beta),
                None,
                "20260804",
                "abc1234"
            )
            .unwrap(),
            "0.9.2-nightly.20260804+abc1234"
        );
    }

    /// A beta line left behind does not hold nightlies below stable.
    #[test]
    fn nightly_follows_stable_when_stable_has_passed_beta() {
        let stable = manifest(ReleaseChannel::Stable, "0.1.9");
        let beta = manifest(ReleaseChannel::Beta, "0.1.3");
        let nightly = manifest(ReleaseChannel::Nightly, "0.1.4-nightly.20261119+c96d2f7");
        assert_eq!(
            suggest_version(
                ReleaseChannel::Nightly,
                Some(&stable),
                Some(&beta),
                Some(&nightly),
                "20261120",
                "abc1234"
            )
            .unwrap(),
            "0.1.10-nightly.20261120+abc1234"
        );
    }

    /// **One version, declared once.** The root Cargo.toml's workspace version
    /// is what a dev desktop build reports, what a published one is suffixed
    /// from, and — through `apps/ios/koru.sh` and Android's
    /// `build.gradle.kts`, which read it at build time — what the phones say.
    /// The iOS project file keeps a copy for builds made straight from Xcode,
    /// and this is what keeps that copy honest. A phone a minor apart from a
    /// desktop is drawn in amber on every device list, which is exactly what
    /// three independent numbers (0.1.0, 0.1.0, 0.0.3) produced.
    #[test]
    fn every_platform_carries_the_workspace_version() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let section = cargo
            .split("[workspace.package]")
            .nth(1)
            .expect("a [workspace.package] table");
        let version = section
            .lines()
            .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
            .expect("a workspace version");
        assert!(
            version.split('.').count() == 3 && version.split('.').all(|p| p.parse::<u32>().is_ok()),
            "the workspace version is a bare MAJOR.MINOR.PATCH, never a channel suffix: {version}"
        );

        let pbxproj =
            std::fs::read_to_string(root.join("apps/ios/KoruMobile.xcodeproj/project.pbxproj"))
                .unwrap();
        let expected = format!("MARKETING_VERSION = {version};");
        let marketing = pbxproj
            .lines()
            .filter(|line| line.trim_start().starts_with("MARKETING_VERSION = "))
            .collect::<Vec<_>>();
        assert!(!marketing.is_empty());
        for line in marketing {
            assert_eq!(
                line.trim(),
                expected,
                "the iOS project's copy is out of step"
            );
        }

        let gradle =
            std::fs::read_to_string(root.join("apps/android/app/build.gradle.kts")).unwrap();
        assert!(
            gradle.contains("versionName = koruWorkspaceVersion()"),
            "Android must read versionName from the workspace, not declare one"
        );
        let sh = std::fs::read_to_string(root.join("apps/ios/koru.sh")).unwrap();
        assert!(
            sh.contains("MARKETING_VERSION=$(workspace_version)"),
            "the iOS build script must pass the workspace version to xcodebuild"
        );
    }
}
