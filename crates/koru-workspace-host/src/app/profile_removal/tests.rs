// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn lost_completion_reply_can_finish_cleanup_only_after_authoritative_absence() {
    assert_eq!(removal_absence(false, false), Ok(()));
    // A retained workspace contradicts removal even if the Profile listing
    // omitted its owner; do not clean up its local state.
    assert_eq!(
        removal_absence(false, true),
        Err(CascadeFailure::Incomplete)
    );
}

#[test]
fn keep_or_an_unfinished_empty_profile_cannot_be_mistaken_for_completed_removal() {
    // Keep clears the attempt too. The remaining Profile distinguishes it
    // from completion, even after the cascade trashed its last workspace.
    assert_eq!(
        removal_absence(true, false),
        Err(CascadeFailure::Incomplete)
    );
    assert_eq!(removal_absence(true, true), Err(CascadeFailure::Incomplete));
}

/// The Store's stale-preview refusal has a code of its own, so the page can
/// refresh the preview and ask again rather than say "try again" for a
/// confirmation that was about a different list (root-catalog §6.1).
#[test]
fn a_stale_preview_is_its_own_failure_for_the_page_to_re_ask_on() {
    assert_eq!(
        CascadeFailure::StalePreview.code(),
        "profile_deletion_preview_stale"
    );
    for other in [
        CascadeFailure::NotPermitted,
        CascadeFailure::Unsupported,
        CascadeFailure::Incomplete,
    ] {
        assert_ne!(CascadeFailure::StalePreview.code(), other.code());
    }
}

// ------------------------------- the Profile a deletion elsewhere was for

fn summary(id: koru_core::ProfileId, name: &str) -> koru_core::ProfileSummary {
    koru_core::ProfileSummary {
        id,
        name: name.to_owned(),
        ..Default::default()
    }
}

fn a_profile() -> koru_core::ProfileId {
    koru_core::ProfileId::from_uuid(uuid::Uuid::from_u128(0x5eed))
}

fn another_profile() -> koru_core::ProfileId {
    koru_core::ProfileId::from_uuid(uuid::Uuid::from_u128(0xfeed))
}

/// **The name has to be read while the row is still there.** A removal is
/// what takes the catalog row away, so the name is captured when the removal
/// is first observed; the open Profile's own document is what outlives the
/// row, and is the only *live* source left on the path whose trigger *is* the
/// row's absence.
#[test]
fn an_observed_name_comes_from_the_catalog_row_or_the_open_profile_document() {
    let profile = a_profile();
    let catalog = vec![summary(profile, "Work"), summary(another_profile(), "Home")];
    assert_eq!(
        observed_profile_name_in(&catalog, profile, None, None).as_deref(),
        Some("Work")
    );
    // The row is gone: the open Profile's document still names it.
    assert_eq!(
        observed_profile_name_in(&[], profile, Some("Work".to_owned()), None).as_deref(),
        Some("Work")
    );
    // No source has it, and nothing is invented in its place.
    assert_eq!(observed_profile_name_in(&[], profile, None, None), None);
    // A blank name is no name, from any source.
    assert_eq!(
        observed_profile_name_in(&[summary(profile, "  ")], profile, None, None),
        None
    );
    assert_eq!(
        observed_profile_name_in(
            &[summary(profile, "")],
            profile,
            Some("Work".to_owned()),
            None
        )
        .as_deref(),
        Some("Work")
    );
}

/// **A deletion from another device, for a Profile that was not open here.**
/// The catalog row went before this device heard of the deletion and the
/// Profile was never the open one, so neither live source has a name — and
/// the notice is still written with one, from what this device remembered
/// while the Profile existed.
#[test]
fn a_profile_deleted_elsewhere_is_named_from_what_this_device_remembered() {
    let profile = a_profile();
    assert_eq!(
        observed_profile_name_in(&[], profile, None, Some("Work".to_owned())).as_deref(),
        Some("Work")
    );
    let notice = profile_deleted_elsewhere_notice(
        koru_i18n::Lang::En,
        DeletedProfile::new(
            observed_profile_name_in(&[], profile, None, Some("Work".to_owned())).as_deref(),
            false,
        ),
    );
    assert!(
        notice.starts_with("\u{201c}Work\u{201d} was deleted from Koru"),
        "{notice}"
    );
}

/// The remembered name is a cache, so a live source outranks it: a rename
/// that arrived after the last listing must not be overruled by the stale
/// copy, and a blank remembered name is still no name.
#[test]
fn a_remembered_name_is_the_last_source_consulted() {
    let profile = a_profile();
    assert_eq!(
        observed_profile_name_in(
            &[summary(profile, "Day job")],
            profile,
            None,
            Some("Work".to_owned())
        )
        .as_deref(),
        Some("Day job")
    );
    assert_eq!(
        observed_profile_name_in(
            &[],
            profile,
            Some("Day job".to_owned()),
            Some("Work".to_owned())
        )
        .as_deref(),
        Some("Day job")
    );
    assert_eq!(
        observed_profile_name_in(&[], profile, None, Some("   ".to_owned())),
        None
    );
}

/// **The honest unknown case stays reachable.** A Profile this device never
/// saw has no name in the catalog, no open document and nothing remembered,
/// and the notice must not start claiming one.
#[test]
fn a_profile_this_device_never_saw_is_still_the_unnamed_notice() {
    let never_seen = another_profile();
    let remembered_someone_else =
        observed_profile_name_in(&[], never_seen, None, None).or_else(|| {
            // Even with a store that remembers other Profiles, this one is
            // not in it.
            observed_profile_name_in(&[summary(a_profile(), "Work")], never_seen, None, None)
        });
    assert_eq!(remembered_someone_else, None);
    let notice = profile_deleted_elsewhere_notice(
        koru_i18n::Lang::En,
        DeletedProfile::new(remembered_someone_else.as_deref(), false),
    );
    assert!(
        notice.starts_with("A Profile on this device was deleted from Koru"),
        "{notice}"
    );
    assert!(!notice.contains('\u{201c}'), "{notice}");
}

/// The name that was captured is the name the person reads, in quotes.
#[test]
fn a_known_name_is_the_notice_the_person_reads() {
    let notice = profile_deleted_elsewhere_notice(
        koru_i18n::Lang::En,
        DeletedProfile::new(Some("Work"), false),
    );
    assert!(
        notice.starts_with("\u{201c}Work\u{201d} was deleted from Koru"),
        "{notice}"
    );
    assert!(
        notice.contains("Another trusted device deleted this Profile."),
        "{notice}"
    );
    assert!(notice.contains("It no longer syncs."), "{notice}");
    assert!(
        notice.contains("remain recoverable for 30 days"),
        "{notice}"
    );
    assert!(!notice.contains("{}"), "{notice}");
}

/// **An unknown name is a different sentence, not a stand-in name.** The
/// defect this replaced showed `\u{201c}This Profile\u{201d} was deleted from
/// Koru`, which reads as the name of a Profile nobody has.
#[test]
fn an_unknown_name_is_written_without_a_name_rather_than_given_a_fake_one() {
    for (profile, expected_opening) in [
        (
            DeletedProfile::new(None, true),
            "The Profile you were using was deleted from Koru",
        ),
        (
            DeletedProfile::new(None, false),
            "A Profile on this device was deleted from Koru",
        ),
        // An empty or blank captured name is an unknown name, not a Profile
        // called nothing in quotes.
        (
            DeletedProfile::new(Some("   "), true),
            "The Profile you were using was deleted from Koru",
        ),
    ] {
        let notice = profile_deleted_elsewhere_notice(koru_i18n::Lang::En, profile);
        assert!(notice.starts_with(expected_opening), "{notice}");
        assert!(!notice.contains("This Profile\u{201d}"), "{notice}");
        assert!(!notice.contains('\u{201c}'), "{notice}");
        assert!(!notice.contains('\u{201d}'), "{notice}");
        assert!(!notice.contains("{}"), "{notice}");
        // The substance is kept whether or not the name is known.
        assert!(notice.contains("It no longer syncs."), "{notice}");
        assert!(
            notice.contains("remain recoverable for 30 days"),
            "{notice}"
        );
    }
}

/// The unknown-name notice still says which Profile is meant as far as this
/// device can: the one that was open, or else the Workspaces now in the Trash.
#[test]
fn an_unknown_name_still_points_at_the_profile_it_can() {
    let in_use =
        profile_deleted_elsewhere_notice(koru_i18n::Lang::En, DeletedProfile::new(None, true));
    assert!(in_use.contains("you were using"), "{in_use}");
    let other =
        profile_deleted_elsewhere_notice(koru_i18n::Lang::En, DeletedProfile::new(None, false));
    assert!(
        other.contains("those Workspaces are the ones that belonged to it"),
        "{other}"
    );
}

/// **Both branches are translated.** The peer branch wrote English with
/// `format!`; it now reads the same table its sibling does, so every language
/// gets its own sentence and none of the three shapes is another's.
#[test]
fn the_notice_is_translated_in_every_language_for_every_shape() {
    for lang in koru_i18n::Lang::ALL {
        let named = profile_deleted_elsewhere_notice(lang, DeletedProfile::new(Some("Work"), true));
        assert_eq!(
            named,
            koru_i18n::fill(
                koru_i18n::t(lang, koru_i18n::Key::ProfileDeletedOnAnotherDevice),
                &["Work"]
            )
        );
        assert!(named.contains("Work"), "{named}");
        assert!(!named.contains("{}"), "{named}");
        let in_use = profile_deleted_elsewhere_notice(lang, DeletedProfile::new(None, true));
        let other = profile_deleted_elsewhere_notice(lang, DeletedProfile::new(None, false));
        assert_eq!(
            in_use,
            koru_i18n::t(lang, koru_i18n::Key::ProfileInUseDeletedOnAnotherDevice)
        );
        assert_eq!(
            other,
            koru_i18n::t(
                lang,
                koru_i18n::Key::ProfileWithoutNameDeletedOnAnotherDevice
            )
        );
        for notice in [&named, &in_use, &other] {
            // A title line and a body, as the hardcoded message had.
            assert!(notice.contains("\n\n"), "{notice}");
        }
        assert_ne!(in_use, other);
        if lang != koru_i18n::Lang::En {
            // Not the English string under another tag.
            assert_ne!(
                in_use,
                koru_i18n::t(
                    koru_i18n::Lang::En,
                    koru_i18n::Key::ProfileInUseDeletedOnAnotherDevice
                ),
                "{}",
                lang.tag()
            );
            assert_ne!(
                other,
                koru_i18n::t(
                    koru_i18n::Lang::En,
                    koru_i18n::Key::ProfileWithoutNameDeletedOnAnotherDevice
                ),
                "{}",
                lang.tag()
            );
        }
    }
}

/// A cleanup that stops has to say so: what happened, why it stopped here,
/// and what finishes it. It used to be a line in the log.
#[test]
fn a_stalled_cleanup_says_why_it_stopped_and_what_finishes_it() {
    let refused = peer_cleanup_stalled_notice(
        koru_i18n::Lang::En,
        Some("Work"),
        CascadeFailure::NotPermitted,
    );
    assert!(
        refused.starts_with(
            "Koru could not finish removing \u{201c}Work\u{201d} from this device\n\n"
        ),
        "{refused}"
    );
    assert!(
        refused.contains("Another trusted device deleted this Profile."),
        "{refused}"
    );
    assert!(refused.contains("nothing was removed here"), "{refused}");
    assert!(refused.contains("Quit Koru and open it again"), "{refused}");
    assert!(!refused.contains("{}"), "{refused}");

    // The other failure is retried, and says so instead.
    for failure in [CascadeFailure::Incomplete, CascadeFailure::Unsupported] {
        let incomplete = peer_cleanup_stalled_notice(koru_i18n::Lang::En, Some("Work"), failure);
        assert!(incomplete.contains("Koru tries again"), "{incomplete}");
        assert!(
            !incomplete.contains("nothing was removed here"),
            "{incomplete}"
        );
    }
}

/// An unknown or blank name is a different headline, never an empty pair of
/// quotes.
#[test]
fn a_stalled_cleanup_of_an_unnamed_profile_has_no_quotes() {
    for name in [None, Some("  ")] {
        let notice =
            peer_cleanup_stalled_notice(koru_i18n::Lang::En, name, CascadeFailure::Incomplete);
        assert!(
            notice.starts_with("Koru could not finish removing a deleted Profile from this device"),
            "{notice}"
        );
        assert!(!notice.contains('\u{201c}'), "{notice}");
        assert!(!notice.contains("{}"), "{notice}");
    }
}
