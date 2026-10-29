// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::{Path, PathBuf};

/// The daemon's icon, and the version block Windows reads.
///
/// It had none until it grew a tray: `Icon::from_resource(1, ..)` reads the
/// executable's own icon rather than a file, so there is no path to resolve and
/// a bundle with no `assets/` still shows the right picture.
///
/// **`FileDescription` is the bare "Koru Store".** The notification area
/// remembers "always show this icon" against that string, so a description
/// carrying a version counts as a different application on every build and the
/// icon disappears. `koru-winres` takes the name and the version separately so
/// the two cannot be concatenated by accident; its documentation records the
/// bug in full.
fn main() {
    println!("cargo:rerun-if-env-changed=KORU_VERSION");
    println!("cargo:rerun-if-env-changed=KORU_BUILD_NUMBER");

    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets");

    // The same mark as the app, in this channel's tint. The store is Koru's,
    // and a second *shape* would be a second thing to recognise for no gain —
    // the color says which channel, and the tray tooltip stays the precise
    // answer.
    let resource = koru_winres::Resource::new(
        "Koru Store",
        "koru-store-server.exe",
        koru_winres::channel_icon(&assets),
    )
    .versioned(version(), build_number());
    koru_winres::embed("koru-store-server", &resource);
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
