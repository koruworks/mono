// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=KORU_VERSION");
    println!("cargo:rerun-if-env-changed=KORU_BUILD_NUMBER");

    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets");
    let resource = koru_winres::Resource::new(
        "Koru AI Chat",
        "koru-ai-chat.exe",
        koru_winres::channel_named_icon(&assets, "koru-ai-chat"),
    )
    .versioned(version(), build_number());
    koru_winres::embed("koru-ai-chat", &resource);
}

fn version() -> String {
    env::var("KORU_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").expect("package version"))
}

fn build_number() -> u64 {
    env::var("KORU_BUILD_NUMBER")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}
