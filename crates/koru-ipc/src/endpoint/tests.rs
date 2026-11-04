// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{AGENT, API_GATEWAY, SESSION_HOST, STORE};

#[test]
fn every_channel_gets_a_distinct_endpoint() {
    // Two channels sharing an endpoint is two builds sharing a database, which
    // is the failure `koru-channel` exists to make impossible.
    let mut names: Vec<String> = koru_channel::SEGMENTS
        .iter()
        .map(|s| Endpoint::for_service(STORE, s).unwrap().name().to_string())
        .collect();
    let before = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), before, "{names:?}");
}

#[test]
fn every_service_gets_a_distinct_endpoint_on_one_channel() {
    // **The reason this crate is parameterized at all.** The agent and the
    // store run side by side on one machine, one user and one channel; if the
    // name did not carry the service, whichever bound first would be dialled by
    // clients of the other — and the handshake, not the transport, would be the
    // only thing that noticed.
    for segment in koru_channel::SEGMENTS {
        let store = Endpoint::for_service(STORE, segment).unwrap();
        let agent = Endpoint::for_service(AGENT, segment).unwrap();
        assert_ne!(store.name(), agent.name());
        assert_ne!(store, agent);
    }
}

#[test]
fn the_name_carries_the_service_so_a_wrong_one_is_visible() {
    for service in [STORE, AGENT, SESSION_HOST] {
        let endpoint = Endpoint::for_service(service, "nightly").unwrap();
        assert!(
            endpoint.name().contains(service),
            "{service} missing from {}",
            endpoint.name()
        );
        assert_eq!(endpoint.service(), service);
    }
}

#[test]
fn a_segment_that_is_not_a_channel_is_refused() {
    // The segment reaches a pipe name and a file name, so it is checked here
    // where it is cheap rather than by the filesystem three frames down.
    let e = Endpoint::for_service(STORE, "../../etc").unwrap_err();
    assert_eq!(e.code(), "ipc_unknown_segment");
    assert!(e.to_string().contains("not a release channel"), "{e}");
}

#[test]
fn the_endpoint_is_stable_across_calls() {
    // It is derived from the OS user, the channel and the service and nothing
    // else — no clock, no pid, no port — or two processes would compute
    // different names in the same second.
    let a = Endpoint::for_service(STORE, "nightly").unwrap();
    let b = Endpoint::for_service(STORE, "nightly").unwrap();
    assert_eq!(a, b);
}

#[test]
fn the_name_carries_the_channel_so_a_wrong_one_is_visible() {
    for segment in koru_channel::SEGMENTS {
        let endpoint = Endpoint::for_service(STORE, segment).unwrap();
        assert!(
            endpoint.name().contains(segment),
            "{segment} missing from {}",
            endpoint.name()
        );
        assert_eq!(endpoint.segment(), *segment);
    }
}

#[cfg(windows)]
#[test]
fn the_windows_name_carries_the_users_sid() {
    // Named pipes live in one machine-wide namespace. Without the SID the
    // second user to start a service collides with the first, and — far worse —
    // a client dials it.
    let sid = owner::current_user_sid().unwrap();
    assert!(sid.starts_with("S-1-"), "{sid}");
    let endpoint = Endpoint::for_service(STORE, "nightly").unwrap();
    assert!(endpoint.name().contains(&sid), "{}", endpoint.name());
    // Namespaced, so interprocess prepends the prefix itself.
    assert!(!endpoint.name().starts_with(r"\\"), "{}", endpoint.name());
    assert!(endpoint.parent().is_none());
}

#[cfg(windows)]
#[test]
fn the_pipe_name_fits_within_the_windows_limit() {
    // `\.\pipe\` plus the name must stay under 256 characters. A SID on a
    // domain-joined machine is longer than one on a workgroup machine, and a
    // root digest adds nine more, so this is worth asserting rather than
    // eyeballing on a developer's laptop.
    let root = std::path::Path::new(r"C:\some\explicit\root\for\a\test");
    for segment in koru_channel::SEGMENTS {
        for service in [STORE, AGENT] {
            for endpoint in [
                Endpoint::for_service(service, segment).unwrap(),
                Endpoint::for_service_root(service, segment, root).unwrap(),
            ] {
                let full = format!(r"\.\pipe\{}", endpoint.name());
                assert!(full.len() < 256, "{} chars: {full}", full.len());
            }
        }
        let claim = Endpoint::for_service_claim(
            crate::REOPEN_CLAIM,
            segment,
            root,
            "5ee8ffdf-00c8-47ba-b294-054820454a0d",
        )
        .unwrap();
        let full = format!(r"\.\pipe\{}", claim.name());
        assert!(full.len() < 256, "{} chars: {full}", full.len());
    }
}

#[cfg(unix)]
#[test]
fn the_unix_socket_sits_under_a_directory_of_its_own() {
    // The directory is the barrier — `mode()` on the socket itself is
    // documented as unsupported on several platforms, macOS among them. It is
    // named for the *service*, so the store's and the agent's do not share one.
    let endpoint = Endpoint::for_service(STORE, "nightly").unwrap();
    let parent = endpoint.parent().expect("a unix endpoint has a directory");
    assert!(parent.ends_with(STORE), "{}", parent.display());
    assert!(
        endpoint.name().ends_with("nightly.sock"),
        "{}",
        endpoint.name()
    );
}

#[test]
fn the_channels_own_root_produces_the_endpoint_a_plain_client_dials() {
    // **The bug this exists to prevent.** A daemon resolves a root before it
    // binds, so a plain launch with no `--root` goes through `for_service_root`;
    // an ordinary client goes through `for_service`. If those disagree the
    // service binds a name nobody looks at, and every test still passes —
    // because a test passes `--root` on both sides, so the two agree with each
    // other perfectly while agreeing with nothing else.
    let Some(own) = koru_channel::store_root() else {
        return; // no local data directory on this machine; nothing to compare
    };
    for service in [STORE, AGENT] {
        let plain = Endpoint::for_service(service, koru_channel::segment()).unwrap();
        let rooted = Endpoint::for_service_root(service, koru_channel::segment(), &own).unwrap();
        assert_eq!(plain, rooted);
    }
}

#[test]
fn a_product_service_at_the_channel_root_meets_a_plain_client() {
    let Some(own) = koru_channel::channel_root() else {
        return;
    };
    let plain = Endpoint::for_service(API_GATEWAY, koru_channel::segment()).unwrap();
    let rooted =
        Endpoint::for_service_channel_root(API_GATEWAY, koru_channel::segment(), &own).unwrap();
    assert_eq!(plain, rooted);
    let isolated = Endpoint::for_service_channel_root(
        API_GATEWAY,
        koru_channel::segment(),
        std::path::Path::new("/tmp/koru-explicit-channel-root"),
    )
    .unwrap();
    assert_ne!(plain, isolated);
}

#[test]
fn any_other_root_gets_an_endpoint_of_its_own() {
    // The other half: a service on an explicit root must be findable, and must
    // not collide with the channel's own or with another root's.
    let plain = Endpoint::for_service(STORE, "nightly").unwrap();
    let a =
        Endpoint::for_service_root(STORE, "nightly", std::path::Path::new("/tmp/koru-a")).unwrap();
    let b =
        Endpoint::for_service_root(STORE, "nightly", std::path::Path::new("/tmp/koru-b")).unwrap();
    assert_ne!(a, plain);
    assert_ne!(b, plain);
    assert_ne!(a, b);
    // Still legible as that channel's, so a wrong one is diagnosable.
    assert!(a.name().contains("nightly"), "{}", a.name());
}

#[test]
fn one_root_still_keeps_every_service_apart() {
    // A gate runs an agent and a store on the *same* temp root. If the digest
    // were the whole discriminator they would collide there and nowhere else,
    // which is the worst possible place for it to happen.
    let root = std::path::Path::new("/tmp/koru-one-root");
    let store = Endpoint::for_service_root(STORE, "nightly", root).unwrap();
    let agent = Endpoint::for_service_root(AGENT, "nightly", root).unwrap();
    let sessions = Endpoint::for_service_root(SESSION_HOST, "nightly", root).unwrap();
    assert_ne!(store.name(), agent.name());
    assert_ne!(store.name(), sessions.name());
    assert_ne!(agent.name(), sessions.name());
}

#[test]
fn the_same_root_always_gets_the_same_endpoint() {
    // The digest is FNV-1a rather than `DefaultHasher` precisely so this holds
    // across builds and toolchains, not merely within one process.
    let root = std::path::Path::new("/tmp/koru-stable");
    assert_eq!(
        Endpoint::for_service_root(STORE, "beta", root).unwrap(),
        Endpoint::for_service_root(STORE, "beta", root).unwrap()
    );
}

#[cfg(windows)]
#[test]
fn canonical_and_native_spellings_of_one_root_share_an_endpoint() {
    // Windows canonicalization adds the verbatim `\\?\` prefix. This is the
    // packaged rehearsal failure this test pins: `run --rehearse-update`
    // validated and retained that spelling, while `status` and `stop` received
    // the ordinary drive-letter path and could not find the Agent it had just
    // started.
    let native = std::env::current_dir().unwrap();
    let canonical = native.canonicalize().unwrap();
    assert_eq!(
        Endpoint::for_service_root(AGENT, "internal-debug", &native).unwrap(),
        Endpoint::for_service_root(AGENT, "internal-debug", &canonical).unwrap()
    );
}

#[cfg(windows)]
#[test]
fn creating_a_child_root_does_not_change_its_endpoint_identity() {
    // The Agent can derive the gateway endpoint before `<root>/store` exists;
    // the gateway creates that directory before binding. Root identity must be
    // lexical, or existence itself moves the pipe between those two moments.
    let unique = format!(
        "koru-ipc-endpoint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let parent = std::env::temp_dir().join(unique);
    let child = parent.join("store");
    std::fs::create_dir_all(&parent).unwrap();
    let before = Endpoint::for_service_root(STORE, "internal-debug", &child).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    let after = Endpoint::for_service_root(STORE, "internal-debug", &child).unwrap();
    assert_eq!(before, after);
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn reopen_claims_are_stable_per_root_and_distinct_per_token() {
    let root = std::env::temp_dir().join(format!("koru-ipc-claim-root-{}", std::process::id()));
    let first = Endpoint::for_service_claim(
        crate::REOPEN_CLAIM,
        "internal-debug",
        &root,
        "5ee8ffdf-00c8-47ba-b294-054820454a0d",
    )
    .unwrap();
    let replay = Endpoint::for_service_claim(
        crate::REOPEN_CLAIM,
        "internal-debug",
        &root,
        "5ee8ffdf-00c8-47ba-b294-054820454a0d",
    )
    .unwrap();
    let other = Endpoint::for_service_claim(
        crate::REOPEN_CLAIM,
        "internal-debug",
        &root,
        "f9781e80-f257-42f5-8e70-df6eb1c1bf37",
    )
    .unwrap();

    assert_eq!(first, replay);
    assert_ne!(first.name(), other.name());
}

#[cfg(windows)]
#[test]
fn windows_root_casing_does_not_change_the_endpoint() {
    let lower = std::path::Path::new(r"c:\dev\koru\store");
    let upper = std::path::Path::new(r"C:\DEV\KORU\STORE");
    assert_eq!(
        Endpoint::for_service_root(STORE, "internal-debug", lower).unwrap(),
        Endpoint::for_service_root(STORE, "internal-debug", upper).unwrap()
    );
}

/// **The length budget, as a test rather than a comment.**
///
/// `sockaddr_un.sun_path` holds **104 bytes on macOS** against Linux's 108, and
/// this crate composes a path into it. Nothing checked the total, so the trap
/// was rediscovered twice: once when `koru-agent-gateway-bridge` pushed a test
/// endpoint to 112 bytes and every bind failed — which is why
/// [`AGENT_GATEWAY_BRIDGE`] is spelled `koru-bridge` — and again when
/// `koru-session-host` on the `internal-release` channel reached **114** and
/// `koru-reopen`'s claim endpoint 106. Both times the lesson was applied to one
/// constant and never made a rule, and every service added in between
/// re-broke it in silence.
///
/// This is that rule. It walks **every** service against **every** channel
/// segment in both qualified forms, so a new service name, a new channel name,
/// or a wider digest fails here — on every platform, in a second — instead of
/// at `bind` on somebody's Mac.
///
/// The cap is 103 rather than 104 because `sun_path` must hold the terminator
/// too. A path of exactly 104 does bind on macOS today; relying on that is
/// relying on an implementation detail, and after [ADR-417] moved the base to
/// `/tmp` the worst case is 70, so strictness costs nothing.
#[cfg(unix)]
#[test]
fn every_endpoint_this_crate_can_name_fits_in_sun_path() {
    const SUN_PATH_USABLE: usize = 103;

    let mut worst = (0usize, String::new());
    for service in crate::SERVICES {
        for segment in koru_channel::SEGMENTS {
            let plain = Endpoint::for_service(service, segment).expect("known segment");

            // The narrow qualifier is what a rooted service gets; the wide one
            // is `for_service_claim`'s. Both are real, so both are budgeted.
            let mut narrow = plain.clone();
            narrow.qualify(&digest("root"));
            let mut wide = plain.clone();
            wide.qualify(&digest64("root"));

            for endpoint in [&plain, &narrow, &wide] {
                let length = endpoint.name().len();
                if length > worst.0 {
                    worst = (length, endpoint.name().to_owned());
                }
                assert!(
                    length <= SUN_PATH_USABLE,
                    "{} is {length} bytes and sun_path holds {SUN_PATH_USABLE}",
                    endpoint.name()
                );
            }
        }
    }
    // Printed rather than asserted: the margin is the useful number when this
    // test starts getting close, and pinning it would fail on every machine
    // whose uid has a different number of digits.
    println!("worst endpoint: {} bytes — {}", worst.0, worst.1);
}

/// The table the budget above walks has to be the whole set, or the budget is
/// only checking the services somebody remembered.
#[test]
fn the_service_table_names_every_service_constant() {
    for service in [
        crate::STORE,
        crate::AGENT,
        crate::SESSION_HOST,
        crate::EVENTS,
        crate::API_GATEWAY,
        crate::REOPEN_CLAIM,
        crate::AGENT_GATEWAY_BRIDGE,
        crate::VOICE,
        crate::DELEGATE,
    ] {
        assert!(
            crate::SERVICES.contains(&service),
            "{service} is not in SERVICES, so no length budget covers it"
        );
    }
    assert_eq!(crate::SERVICES.len(), 9);
}
