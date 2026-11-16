// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::path::PathBuf;

#[test]
fn every_channel_has_a_distinct_segment() {
    // If two channels shared a segment they would share a database, which is
    // the one thing this module exists to prevent.
    let mut seen: Vec<&str> = SEGMENTS.to_vec();
    seen.sort_unstable();
    let before = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), before, "two channels share a directory");
}

#[test]
fn a_local_debug_and_release_build_do_not_share_a_store() {
    // `ReleaseChannel` calls both of these `Internal`. They are different
    // binaries with different behaviour, and one store between them is exactly
    // the confusion the split removes.
    let debug = segment_of(ReleaseChannel::Internal, true);
    let release = segment_of(ReleaseChannel::Internal, false);
    assert_ne!(debug, release);
    assert_eq!(debug, "internal-debug");
    assert_eq!(release, "internal-release");
}

#[test]
fn published_channels_ignore_the_debug_flag() {
    // A stable build is a stable build however it was compiled; only the
    // unpublished channel splits.
    for channel in [
        ReleaseChannel::Stable,
        ReleaseChannel::Beta,
        ReleaseChannel::Nightly,
    ] {
        assert_eq!(
            segment_of(channel, true),
            segment_of(channel, false),
            "{channel:?} split on a build flag"
        );
    }
}

#[test]
fn segments_are_stable_ascii_tokens() {
    // They are path components and store instance keys, so they can never be
    // translated or prettified.
    for s in SEGMENTS {
        assert!(
            s.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "{s} is not a stable token"
        );
    }
}

#[test]
fn every_segment_is_reachable_from_some_channel() {
    let produced: Vec<&str> = ReleaseChannel::ALL
        .iter()
        .flat_map(|c| [segment_of(*c, true), segment_of(*c, false)])
        .collect();
    for s in SEGMENTS {
        assert!(produced.contains(s), "{s} is listed but unreachable");
    }
}

#[test]
fn this_builds_segment_is_one_of_the_five() {
    assert!(SEGMENTS.contains(&segment()));
}

#[test]
fn no_two_channels_resolve_to_one_directory() {
    // **The claim three documents make, asserted at last.** Stable, beta,
    // nightly and the two local builds never share a database, because they do
    // not share a schema: a nightly that has moved `workspace.db` forward would
    // be opened by stable, refused, and — if the refusal ever leaked —
    // half-written. Separate directories make that impossible rather than
    // merely unlikely, and this is what checks that they are separate.
    //
    // It could not be written before `store_root_of`: `store_root()` answers
    // only for the running binary, so a test could compare one path with
    // itself and nothing else.
    let Some(first) = store_root_of(SEGMENTS[0]) else {
        return; // no local-data directory on this machine; nothing to compare
    };
    let _ = first;

    let mut roots: Vec<PathBuf> = SEGMENTS
        .iter()
        .filter_map(|segment| store_root_of(segment))
        .collect();
    assert_eq!(roots.len(), SEGMENTS.len());

    let before = roots.len();
    roots.sort();
    roots.dedup();
    assert_eq!(
        roots.len(),
        before,
        "two channels share a directory: {roots:?}"
    );

    // And each one is under the channel it belongs to, so a person looking at
    // the path can tell which build wrote it.
    for segment in SEGMENTS {
        let root = store_root_of(segment).unwrap();
        assert!(
            root.to_string_lossy().to_lowercase().contains(segment),
            "{segment} is not in {}",
            root.display()
        );
    }
}

#[test]
fn this_builds_root_is_the_one_its_segment_names() {
    // The join between the two functions. If these ever disagreed, the daemon
    // would write to one directory and a chooser would list another.
    assert_eq!(store_root(), store_root_of(segment()));
}

#[test]
fn the_store_root_is_channel_scoped() {
    // Two channels must not resolve to one directory.
    if let Some(root) = store_root() {
        let shown = root.to_string_lossy().to_lowercase();
        assert!(shown.contains(&segment().to_lowercase()), "{shown}");
        assert!(shown.ends_with("store"), "{shown}");
    }
}

#[test]
fn an_existing_legacy_root_sticks_and_nothing_else_does() {
    // The whole macOS migration rule in four rows: legacy wins only when it is
    // the only root that exists. Both existing means a build already answered
    // *preferred* and wrote there — flipping back would split the data.
    let preferred = std::path::Path::new("/Users/ada/Library/Application Support/Koru/stable");
    let legacy = std::path::Path::new("/Users/ada/.local/share/Koru/stable");
    assert_eq!(pick_root(preferred, legacy, false, false), preferred);
    assert_eq!(pick_root(preferred, legacy, true, false), preferred);
    assert_eq!(pick_root(preferred, legacy, false, true), legacy);
    assert_eq!(pick_root(preferred, legacy, true, true), preferred);
}

#[test]
fn a_synced_folder_is_recognised_by_name() {
    for (path, expected) in [
        (r"C:\Users\sam\OneDrive\Koru", "OneDrive"),
        (r"C:\Users\sam\Dropbox\koru\store", "Dropbox"),
        ("/home/sam/Nextcloud/koru", "Nextcloud"),
        ("/home/sam/Sync/syncthing/koru", "Syncthing"),
        (
            "/Users/sam/Library/Mobile Documents/iCloud~x",
            "iCloud Drive",
        ),
    ] {
        assert_eq!(
            looks_synced(std::path::Path::new(path)),
            Some(expected),
            "{path}"
        );
    }
}

#[test]
fn an_ordinary_path_is_not_flagged() {
    for path in [
        r"C:\Users\sam\AppData\Local\Koru\stable\store",
        "/home/sam/.local/share/koru/stable/store",
    ] {
        assert_eq!(looks_synced(std::path::Path::new(path)), None, "{path}");
    }
}

#[test]
fn the_synced_check_is_advisory_and_case_insensitive() {
    // It must never refuse to start — a false positive would lock someone out
    // of their own data — so it only ever produces a name to put in a warning.
    assert_eq!(
        looks_synced(std::path::Path::new(r"D:\ONEDRIVE\koru")),
        Some("OneDrive")
    );
}

/// The sibling directories beside a conventional `store/` are one rule each,
/// and a bare root used as a store root grows nothing outside itself.
#[test]
fn the_delegate_root_sits_beside_the_store_and_never_outside_a_bare_root() {
    let channel = std::path::Path::new("R");
    let store = store_root_under(channel);
    assert_eq!(delegate_root_beside(&store), channel.join("delegate"));
    assert_eq!(events_root_beside(&store), channel.join("events"));
    assert_ne!(
        delegate_root_beside(&store),
        events_root_beside(&store),
        "two device-local databases never share a directory"
    );
    let bare = std::path::Path::new("some-temp-dir");
    assert_eq!(delegate_root_beside(bare), bare.join("delegate"));
    assert!(
        delegate_root_beside(bare).starts_with(bare),
        "a harness root never grows a sibling outside its own tree"
    );
}

/// The artifact store's directory follows the events journal's rule: a
/// sibling of a conventional `store/`, and inside a bare test root.
#[test]
fn the_artifacts_root_is_a_sibling_of_a_conventional_store_and_inside_a_bare_root() {
    let channel = std::path::Path::new("R");
    assert_eq!(
        artifacts_root_beside(&channel.join("store")),
        channel.join("artifacts")
    );
    assert_eq!(
        artifacts_root_beside(&channel.join("store")).parent(),
        events_root_beside(&channel.join("store")).parent(),
        "artifacts/, events/ and store/ are siblings"
    );
    let bare = std::path::Path::new("bare-temp-root");
    assert_eq!(artifacts_root_beside(bare), bare.join("artifacts"));
}

#[test]
fn the_ai_root_sits_under_the_channel_root_beside_the_store() {
    let channel = std::path::Path::new("R");
    assert_eq!(ai_root_under(channel), channel.join("ai"));
    assert_ne!(ai_root_under(channel), store_root_under(channel));
    assert!(
        !ai_root_under(channel).starts_with(store_root_under(channel)),
        "clearing the store must not clear a Bot's working folder"
    );
}
