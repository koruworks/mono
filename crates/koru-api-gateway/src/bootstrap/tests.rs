// SPDX-License-Identifier: MPL-2.0

use super::*;

fn bootstrap(
    session_lifecycle: Option<koru_network_bridge::SessionLifecycleAuthority>,
) -> koru_network_bridge::LaunchBootstrap {
    koru_network_bridge::LaunchBootstrap {
        device_id: None,
        run_authority: None,
        bots_engine: None,
        launch_id: "launch".into(),
        bridge_version: koru_network_bridge::BRIDGE_VERSION,
        channel: koru_channel::segment().into(),
        root_fingerprint: "root".into(),
        installation_fingerprint: "installation".into(),
        capability: koru_network_bridge::BridgeCapability::generate(),
        session_lifecycle,
    }
}

#[test]
fn a_supervised_gateway_without_a_session_host_skips_only_that_adapter() {
    assert!(session_bridge_authority(None).is_none());
    let without_host = bootstrap(None);
    assert!(session_bridge_authority(Some(&without_host)).is_none());

    let with_host = bootstrap(Some(koru_network_bridge::SessionLifecycleAuthority::new(
        "authority".into(),
    )));
    assert!(session_bridge_authority(Some(&with_host)).is_some());
}

#[test]
fn gateway_resolving_the_ordinary_channel_root_dials_the_unrooted_session_host() {
    let Some(channel_root) = koru_channel::channel_root() else {
        return;
    };
    let gateway = session_bridge_endpoint(&channel_root, koru_channel::segment()).unwrap();
    let host =
        koru_ipc::Endpoint::for_service(koru_ipc::SESSION_HOST, koru_channel::segment()).unwrap();
    assert_eq!(gateway, host);
}

#[test]
fn gateway_and_host_agree_on_an_isolated_channel_root() {
    let isolated = tempfile::tempdir().unwrap();
    let gateway = session_bridge_endpoint(isolated.path(), koru_channel::segment()).unwrap();
    let host = koru_ipc::Endpoint::for_service_channel_root(
        koru_ipc::SESSION_HOST,
        koru_channel::segment(),
        isolated.path(),
    )
    .unwrap();
    assert_eq!(gateway, host);
    assert_ne!(
        gateway,
        koru_ipc::Endpoint::for_service(koru_ipc::SESSION_HOST, koru_channel::segment(),).unwrap()
    );
}

/// **An unsupervised gateway validates nothing**, which is what lets a
/// developer run one in the foreground on a temporary root.
#[test]
fn no_bootstrap_is_no_refusal() {
    assert!(validate(std::path::Path::new("C:/x"), None, koru_channel::segment()).is_ok());
}

/// The channel, the bridge version and the root are checked *before* the image
/// is read, so a bootstrap for another channel is refused without touching the
/// disk. Each of the three is checked, not just the first one.
#[test]
fn a_bootstrap_for_another_channel_root_or_bridge_is_refused() {
    let store_root = std::path::Path::new("C:/rehearsal/root/store");
    let fingerprint = koru_network_bridge::fingerprint(store_root.to_string_lossy().as_bytes());

    let mut wrong_channel = bootstrap(None);
    wrong_channel.channel = "not-this-channel".into();
    wrong_channel.root_fingerprint = fingerprint.clone();
    assert!(validate(store_root, Some(&wrong_channel), koru_channel::segment()).is_err());

    let mut wrong_bridge = bootstrap(None);
    wrong_bridge.bridge_version = koru_network_bridge::BRIDGE_VERSION.wrapping_add(1);
    wrong_bridge.root_fingerprint = fingerprint.clone();
    assert!(validate(store_root, Some(&wrong_bridge), koru_channel::segment()).is_err());

    // The default fixture's `root_fingerprint` is the literal "root", which is
    // not the digest of any path.
    let wrong_root = bootstrap(None);
    assert!(validate(store_root, Some(&wrong_root), koru_channel::segment()).is_err());
}

/// **The root that is fingerprinted is the store root.** The Agent digests the
/// path it hands the Store, so validating against the channel root above it
/// would refuse every real supervised launch — a mistake that is invisible
/// until a supervised process exists to make it.
#[test]
fn the_fingerprinted_root_is_the_store_root_and_not_the_channel_root() {
    let channel_root = std::path::Path::new("C:/rehearsal/root");
    let store_root = koru_channel::store_root_under(channel_root);
    assert_ne!(
        koru_network_bridge::fingerprint(store_root.to_string_lossy().as_bytes()),
        koru_network_bridge::fingerprint(channel_root.to_string_lossy().as_bytes())
    );
}
