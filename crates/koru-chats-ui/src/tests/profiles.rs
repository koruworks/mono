// SPDX-License-Identifier: MPL-2.0

//! One profile's chats (AI schema 13, koru-ai D120): the list is the
//! window's profile's, and switching profile switches it.

use koru_ai_core::ProfileId;

use super::*;
use crate::Load;

fn of(n: u32, title: &str, profile: ProfileId) -> Chat {
    let mut chat = chat(n, title, 1_000 * i64::from(n));
    chat.profile_id = Some(profile);
    chat
}

/// **Another profile's chat never shows**, whoever listed it: a listing is
/// kept to the window's profile, Pinned and the rest alike. A chat the
/// service has not placed yet still shows, so it is never unreachable.
#[test]
fn a_listing_shows_only_the_windows_profiles_chats() {
    let (private, work) = (ProfileId::new(), ProfileId::new());
    let mut surface = ChatsSurface::without_reader();
    surface.set_now(NOW);
    assert!(surface.set_profile(Some(private)));
    let mut pinned_elsewhere = of(3, "Pinned at work", work);
    pinned_elsewhere.pinned_at = Some(Timestamp::from_millis(NOW - 10));
    surface.take_listing(Ok(Listing {
        chats: vec![
            of(1, "Mine", private),
            of(2, "Theirs", work),
            pinned_elsewhere,
            chat(4, "Not placed yet", 4_000),
        ],
        agents: Vec::new(),
    }));
    assert_eq!(titles(&surface), ["Mine", "Not placed yet"]);
}

/// **Switching profile switches the list at once**: nothing of the old
/// profile stays on screen, picked or asked about while the new one is
/// listed; the same profile again changes nothing.
#[test]
fn switching_profile_empties_the_list_until_the_new_one_lands() {
    let (private, work) = (ProfileId::new(), ProfileId::new());
    let mut surface = showing(vec![of(1, "Mine", private), of(2, "Also mine", private)]);
    surface.set_profile(Some(private));
    surface.take_listing(Ok(Listing {
        chats: vec![of(1, "Mine", private), of(2, "Also mine", private)],
        agents: Vec::new(),
    }));
    surface.selection = vec![id(1)];
    surface.wants_open = Some(id(2));
    assert!(!surface.set_profile(Some(private)));
    assert_eq!(titles(&surface).len(), 2);

    assert!(surface.set_profile(Some(work)));
    assert!(surface.chats().is_empty());
    assert!(surface.selection.is_empty());
    assert_eq!(surface.take_wants_open(), None);
    assert_eq!(surface.load(), Load::Cold);
    assert_eq!(surface.profile(), Some(work));
    surface.take_listing(Ok(Listing {
        chats: vec![of(3, "Work", work)],
        agents: Vec::new(),
    }));
    assert_eq!(titles(&surface), ["Work"]);
}
