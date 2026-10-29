// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The agent's icon, and the version block Windows reads.
///
/// **The product icon, tinted for the channel.** The Agent owns the only
/// notification-area icon now, so a role badge no longer distinguishes it from
/// another Koru tray icon; it only adds noise at 16 px. What *does* need
/// telling apart is one channel's agent from another's — several install side
/// by side — so `channel_icon` picks the tinted mark and the tooltip stays the
/// precise answer.
///
/// It needs one because it lives in the notification area:
/// `Icon::from_resource(1, ..)` reads the executable's own icon rather than a
/// file, so there is no path to resolve and a bundle with no `assets/` still
/// shows the right picture.
///
/// **`FileDescription` is the bare "Koru Agent", with no version in it.** The
/// notification area remembers "always show this icon" against that string, so
/// a description carrying a version counts as a different application on every
/// build and the icon disappears. `koru-winres` takes the name and the version
/// separately so the two cannot be concatenated by accident; its documentation
/// records the bug in full.
fn main() {
    stamp_branch();

    println!("cargo:rerun-if-env-changed=KORU_VERSION");
    println!("cargo:rerun-if-env-changed=KORU_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=KORU_WINDOWS_PACKAGE_PUBLISHER");

    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets");

    let mut resource = koru_winres::Resource::new(
        "Koru Agent",
        "koru-agent.exe",
        koru_winres::channel_icon(&assets),
    )
    .versioned(version(), build_number());
    if let Some(publisher) = env::var("KORU_WINDOWS_PACKAGE_PUBLISHER")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        // The packager writes the MSIX's version with this same function, and
        // registration compares the two.
        let package_version = koru_winres::package_version(&version(), build_number())
            .unwrap_or_else(|error| panic!("{error}"));
        println!("cargo:rustc-env=KORU_WINDOWS_PACKAGE_VERSION={package_version}");
        let channel = env::var("KORU_RELEASE_CHANNEL").unwrap_or_else(|_| "internal".into());
        let channel = koru_channel::ReleaseChannel::from_build_channel(&channel);
        let debug = env::var("PROFILE").as_deref() == Ok("debug");
        resource = resource.package_identity(koru_winres::notification_identity(
            koru_channel::segment_of(channel, debug),
            publisher,
        ));
    }
    koru_winres::embed("koru-agent", &resource);
}

/// Stamp the working-tree branch into internal builds so two locally built
/// Agents can identify themselves in the notification area. Published builds
/// already carry a channel, version and commit and deliberately omit it.
///
/// Keep this policy aligned with `apps/koru/build.rs`: build-script variables
/// apply only to their own package, so the Agent must stamp its own binary.
fn stamp_branch() {
    println!("cargo:rerun-if-env-changed=KORU_RELEASE_CHANNEL");
    if env::var_os("KORU_RELEASE_CHANNEL").is_some() {
        return;
    }
    let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let Some(git_dir) = git(&manifest, &["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    println!(
        "cargo:rerun-if-changed={}",
        Path::new(&git_dir).join("HEAD").display()
    );
    match git(&manifest, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(branch) if !branch.is_empty() && branch != "HEAD" => {
            println!("cargo:rustc-env=KORU_BRANCH={branch}");
        }
        _ => {}
    }
    // The commit too, so two internal builds of one branch can tell whether
    // they are the same code — the build a device advertises compares on it.
    // A published build gets its KORU_COMMIT from the packager instead.
    // HEAD names the branch; the branch's ref file (or packed-refs, once git
    // packs it) is what moves on a commit, so both are watched. Refs live in
    // the *common* dir, which differs from the git dir inside a worktree.
    if let Some(reference) = git(&manifest, &["rev-parse", "--symbolic-full-name", "HEAD"])
        && reference.starts_with("refs/")
        && let Some(common) = git(&manifest, &["rev-parse", "--git-common-dir"])
    {
        let common = manifest.join(common);
        println!(
            "cargo:rerun-if-changed={}",
            common.join(&reference).display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            common.join("packed-refs").display()
        );
    }
    if let Some(commit) = git(&manifest, &["rev-parse", "--short=12", "HEAD"])
        && !commit.is_empty()
    {
        println!("cargo:rustc-env=KORU_COMMIT={commit}");
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn version() -> String {
    env::var("KORU_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").expect("package version"))
}

fn build_number() -> u64 {
    env::var("KORU_BUILD_NUMBER")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}
