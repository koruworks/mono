// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::{Path, PathBuf};

/// The updater's icon, and the version block Windows reads.
///
/// It shipped in every package with neither, alone among the four binaries, and
/// that is more visible than it sounds: `koru-updater.exe` is the process that
/// asks for elevation, so what Windows draws in the UAC prompt — and what
/// Task Manager shows while an update runs — was a generic executable icon with
/// no publisher, no product and no version. A person is being asked to trust
/// that dialog. It should say whose program it is.
///
/// The same mark as the other three. The updater is Koru's, and a second icon
/// would be a second thing to recognise for no gain.
///
/// **`FileDescription` is the bare "Koru Updater", with no version in it.** The
/// notification area remembers "always show this icon" against that string, so a
/// description carrying a version counts as a different application on every
/// build and the icon disappears. This binary has no tray icon of its own, but
/// the rule is the rule everywhere: `koru-winres::Resource::new` takes the name
/// and `versioned()` takes the version precisely so the two cannot be
/// concatenated by accident, and a build script that broke the pattern here is
/// the one somebody copies next. Its documentation records the bug in full.
fn main() {
    println!("cargo:rerun-if-env-changed=KORU_VERSION");
    println!("cargo:rerun-if-env-changed=KORU_BUILD_NUMBER");

    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets");

    let resource =
        koru_winres::Resource::new("Koru Updater", "koru-updater.exe", assets.join("koru.ico"))
            .versioned(version(), build_number());
    koru_winres::embed("koru-updater", &resource);
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
