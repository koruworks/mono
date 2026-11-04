// SPDX-License-Identifier: MPL-2.0

//! **The Apple builds' notices**: the macOS package and the iOS app, from the
//! same generator as Windows' and Android's.
//!
//! - **macOS.** `package::build` writes the package's notices on the Mac that
//!   packages it, for that host's triple. `koru-release notices macos` runs
//!   the same walk for either Mac triple from any host, refusing exactly as a
//!   release does, so a crate without a license file is found on whichever
//!   machine bumps the lockfile, not at the first Mac release after it.
//! - **iOS.** The app links the `koru-mobile-ios` staticlib and the Swift
//!   packages Xcode resolves. `koru-release notices ios`, run by the app's
//!   Xcode build phase through `apps/ios/koru.sh notices`, writes the four
//!   files Settings → Open-source licenses shows into the bundle's
//!   `licenses/`: the staticlib's crates, the Swift packages (each a
//!   `licences/bundled/` file marked `Platforms: ios`), Koru's own `LICENSE`
//!   and `NOTICE`, and where this build's source is.

use super::{BUNDLED_DIR, Build, Bundled, FILE, Unrecorded, generate, read_bundled};
use std::fs;
use std::path::{Path, PathBuf};

/// The workspace packages a macOS package builds: the channel binaries
/// without Windows' COM notification server, the notification owner only a
/// Mac packages, and the updater (`package::build`).
pub(crate) const MACOS_PACKAGES: &[&str] = &[
    "koru",
    "koru-agent",
    "koru-mcp",
    "koru-voice-worker",
    "koru-notification-owner",
    "koru-updater",
];

/// A macOS package is built for one of these; each package's notices walk
/// only its own.
pub(crate) const MACOS_TRIPLES: &[&str] = &["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// The workspace package whose code is in the iOS app: the C-ABI staticlib,
/// and through it everything it links.
pub(crate) const IOS_PACKAGES: &[&str] = &["koru-mobile-ios"];

/// The triple a distributed iOS app holds code for. The simulator's
/// (`aarch64-apple-ios-sim`) is never distributed.
pub(crate) const IOS_TRIPLES: &[&str] = &["aarch64-apple-ios"];

/// Where Xcode pins the Swift packages the app links.
pub(crate) const PACKAGE_RESOLVED: &str =
    "apps/ios/KoruMobile.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved";

/// The files the iOS app's `licenses/` holds, in the names Android's
/// `assets/licenses/` uses and `OpenSourceLicenses.swift` reads.
pub(crate) const IOS_FILES: &[&str] = &["LICENSE.txt", "NOTICE.txt", "SOURCE.txt", FILE];

/// The build a macOS package's notices describe, for `triples`.
pub(crate) fn macos_build(triples: Vec<String>) -> Build<'static> {
    Build {
        packages: MACOS_PACKAGES,
        triples,
        os: "macos",
        unrecorded: Unrecorded::Refuse,
        source: None,
    }
}

/// The build the iOS app's notices describe.
pub(crate) fn ios_build(unrecorded: Unrecorded, source: Option<&str>) -> Build<'_> {
    Build {
        packages: IOS_PACKAGES,
        triples: IOS_TRIPLES
            .iter()
            .map(|triple| (*triple).to_owned())
            .collect(),
        os: "ios",
        unrecorded,
        source,
    }
}

/// Writes the iOS app's `licenses/` into `dir`: the notices, Koru's
/// `LICENSE` and `NOTICE`, and `SOURCE.txt` naming this build's source.
pub(crate) fn write_ios(
    root: &Path,
    dir: &Path,
    source: &str,
    unrecorded: Unrecorded,
) -> Result<(), String> {
    check_source(source)?;
    let path = root.join(PACKAGE_RESOLVED);
    let resolved =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let missing = unattributed(&pins(&resolved)?, &read_bundled(&root.join(BUNDLED_DIR))?);
    if !missing.is_empty() {
        return Err(format!(
            "these Swift packages the app links have no notice in {BUNDLED_DIR} \
             (Platforms: ios, Source at the pinned revision, Version as pinned): {}",
            missing.join(", ")
        ));
    }
    let notices = generate(root, &ios_build(unrecorded, Some(source)), &[])?;
    let files = ios_files(root, source, notices)?;
    fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for (name, contents) in files {
        let path = dir.join(name);
        fs::write(&path, contents).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

/// The source a build names must be a link a person can follow (MPL-2.0
/// §3.2); `koru.sh` passes `https://github.com/koruworks/mono/tree/<commit>`.
pub(crate) fn check_source(source: &str) -> Result<(), String> {
    if source.starts_with("https://") {
        Ok(())
    } else {
        Err(format!("--source must be an https:// URL, not `{source}`"))
    }
}

/// [`IOS_FILES`] in order, with their contents: Koru's `LICENSE` and
/// `NOTICE` from the repository, the source link, and `notices`.
pub(crate) fn ios_files(
    root: &Path,
    source: &str,
    notices: String,
) -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    let read = |from: &str| {
        let path = root.join(from);
        fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
    };
    Ok(vec![
        (IOS_FILES[0], read("LICENSE")?),
        (IOS_FILES[1], read("NOTICE")?),
        (IOS_FILES[2], format!("{source}\n").into_bytes()),
        (IOS_FILES[3], notices.into_bytes()),
    ])
}

/// One Swift package as `Package.resolved` pins it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pin {
    pub identity: String,
    pub location: String,
    pub revision: String,
    pub version: Option<String>,
}

/// The pins in a `Package.resolved` (format version 2 or 3).
pub(crate) fn pins(resolved: &str) -> Result<Vec<Pin>, String> {
    let value: serde_json::Value =
        serde_json::from_str(resolved).map_err(|error| format!("{PACKAGE_RESOLVED}: {error}"))?;
    let pins = value["pins"]
        .as_array()
        .ok_or_else(|| format!("{PACKAGE_RESOLVED} has no `pins`"))?;
    pins.iter()
        .map(|pin| {
            let field = |key: &str| {
                pin[key]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{PACKAGE_RESOLVED}: a pin has no `{key}`"))
            };
            Ok(Pin {
                identity: field("identity")?,
                location: field("location")?,
                revision: pin["state"]["revision"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{PACKAGE_RESOLVED}: a pin has no revision"))?,
                version: pin["state"]["version"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

/// **Every Swift package must be attributed at the revision it is pinned
/// to.** Xcode resolves them, so `cargo metadata` cannot; each is a
/// `licences/bundled/` file whose `Source` names the pinned revision, whose
/// `Version` is the pinned version, and which applies to iOS. Returns the pins
/// that have none, as `identity revision`.
pub(crate) fn unattributed(pins: &[Pin], bundled: &[Bundled]) -> Vec<String> {
    pins.iter()
        .filter(|pin| {
            !bundled.iter().any(|component| {
                component.applies_to("ios")
                    && component
                        .field("Source")
                        .is_some_and(|source| source.contains(&pin.revision))
                    && component.field("Version") == pin.version.as_deref()
            })
        })
        .map(|pin| format!("{} {}", pin.identity, pin.revision))
        .collect()
}

const USAGE: &str = "usage:\n  koru-release notices macos --out <file> [--triple <triple>]...\n  \
koru-release notices ios --dir <licenses dir> --source <url> [--debug]";

/// `koru-release notices macos …`: the macOS package's notices for each
/// `--triple` (both Mac triples when none is named), refused as a release is.
pub(crate) fn run_macos(root: &Path, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut out: Option<PathBuf> = None;
    let mut triples = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(args.next().ok_or(USAGE)?.into()),
            "--triple" => triples.push(args.next().ok_or(USAGE)?),
            _ => return Err(format!("unexpected argument: {arg}\n{USAGE}")),
        }
    }
    let out = out.ok_or(USAGE)?;
    if triples.is_empty() {
        triples = MACOS_TRIPLES.iter().map(|t| (*t).to_owned()).collect();
    }
    let text = generate(root, &macos_build(triples), &[])?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&out, text).map_err(|error| format!("{}: {error}", out.display()))
}

/// `koru-release notices ios …`, run by the app's Xcode build phase.
///
/// `--debug` labels a crate that has no record in `licences/crates/` instead
/// of refusing it, as Android's debug build does; a Release build never
/// passes it.
pub(crate) fn run_ios(root: &Path, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut dir: Option<PathBuf> = None;
    let mut source: Option<String> = None;
    let mut unrecorded = Unrecorded::Refuse;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(args.next().ok_or(USAGE)?.into()),
            "--source" => source = Some(args.next().ok_or(USAGE)?),
            "--debug" => unrecorded = Unrecorded::Label,
            _ => return Err(format!("unexpected argument: {arg}\n{USAGE}")),
        }
    }
    let (Some(dir), Some(source)) = (dir, source) else {
        return Err(USAGE.into());
    };
    check_source(&source)?;
    write_ios(root, &dir, &source, unrecorded)
}

#[cfg(test)]
mod tests;
