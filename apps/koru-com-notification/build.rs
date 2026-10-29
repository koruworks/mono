// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=KORU_VERSION");
    println!("cargo:rerun-if-env-changed=KORU_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=KORU_WINDOWS_PACKAGE_PUBLISHER");
    println!("cargo:rerun-if-env-changed=KORU_RELEASE_CHANNEL");

    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets");
    let mut resource = koru_winres::Resource::new(
        "Koru Notification Activation",
        "koru-com-notification.exe",
        koru_winres::channel_icon(&assets),
    )
    .versioned(version(), build_number());
    if let Some(publisher) = env::var("KORU_WINDOWS_PACKAGE_PUBLISHER")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        let channel = env::var("KORU_RELEASE_CHANNEL").unwrap_or_else(|_| "internal".into());
        let channel = koru_channel::ReleaseChannel::from_build_channel(&channel);
        let debug = env::var("PROFILE").as_deref() == Ok("debug");
        let mut identity =
            koru_winres::notification_identity(koru_channel::segment_of(channel, debug), publisher);
        identity.application_id = koru_native_notifications::WINDOWS_NOTIFICATION_APPLICATION_ID;
        resource = resource.package_identity(identity);
    }
    koru_winres::embed("koru-com-notification", &resource);
}

fn version() -> String {
    env::var("KORU_VERSION")
        .ok()
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").expect("package version"))
}

fn build_number() -> u64 {
    env::var("KORU_BUILD_NUMBER")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}
