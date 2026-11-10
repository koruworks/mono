// SPDX-License-Identifier: MPL-2.0

use super::*;

fn profile(n: u128) -> ProfileId {
    ProfileId::from_uuid(uuid::Uuid::from_u128(n))
}

fn summary(id: ProfileId, name: &str) -> ProfileSummary {
    ProfileSummary {
        id,
        name: name.to_owned(),
        ..Default::default()
    }
}

/// The catalog is authoritative while it has the row — a rename elsewhere
/// replaces the remembered name rather than being shadowed by it.
#[test]
fn a_catalog_listing_replaces_the_name_it_carries() {
    let work = profile(1);
    let known = vec![(work, "Work".to_owned())];
    assert_eq!(
        merged(&known, &[summary(work, "Day job")]),
        vec![(work, "Day job".to_owned())]
    );
}

/// **The record outlives the row.** This is the whole point: the Profile is
/// gone from the catalog, and its name is still here to word a notice with.
#[test]
fn a_profile_the_catalog_no_longer_lists_keeps_its_name_in_front_of_older_absences() {
    let gone_long_ago = profile(1);
    let just_gone = profile(2);
    let live = profile(3);
    let known = vec![
        (just_gone, "Work".to_owned()),
        (live, "Home".to_owned()),
        (gone_long_ago, "Old".to_owned()),
    ];
    // `just_gone` was in the listing last time and is not now; it keeps its
    // name, behind the live row and ahead of the absence that is already old.
    assert_eq!(
        merged(&known, &[summary(live, "Home")]),
        vec![
            (live, "Home".to_owned()),
            (just_gone, "Work".to_owned()),
            (gone_long_ago, "Old".to_owned()),
        ]
    );
}

/// A blank name is no name: the notice has its own sentence for an unknown
/// one and must never be handed empty quotes.
#[test]
fn a_blank_name_is_never_remembered() {
    let blank = profile(1);
    assert!(merged(&[], &[summary(blank, "   ")]).is_empty());
    assert!(merged(&[], &[summary(blank, "")]).is_empty());
    assert_eq!(remembered_name("  Work  ").as_deref(), Some("Work"));
    // A line break would read back as two records, so it is not one.
    assert_eq!(remembered_name("Wo\nrk").as_deref(), Some("Wo rk"));
}

/// The store is bounded, and the cap takes the stalest absence first. A live
/// Profile is re-seen on every listing, so it cannot be evicted while it
/// exists.
#[test]
fn the_store_is_capped_and_evicts_the_stalest_absence_first() {
    let gone = profile(0xdead);
    let mut known = vec![(gone, "Work".to_owned())];
    let catalog: Vec<ProfileSummary> = (1..=KEPT as u128)
        .map(|n| summary(profile(n), &format!("Profile {n}")))
        .collect();

    known = merged(&known, &catalog);
    assert_eq!(known.len(), KEPT);
    // Every live Profile survived; the one absence was what the cap took.
    for row in &catalog {
        assert!(known.iter().any(|(id, _)| *id == row.id));
    }
    assert!(!known.iter().any(|(id, _)| *id == gone));
}

#[test]
fn a_record_round_trips_including_a_name_with_the_separator_in_it() {
    let names = vec![
        (profile(1), "Work | Home".to_owned()),
        (profile(2), "Études".to_owned()),
    ];
    assert_eq!(decode(&encode(&names)), names);
    // A line that is not a record is skipped rather than fatal.
    assert_eq!(
        decode("not-a-uuid|Work\nno separator here\n\n"),
        Vec::<(ProfileId, String)>::new()
    );
}

/// **The name survives the restart**, because the deletion notice is replayed
/// to a window on connect — which can be a launch after the one that heard
/// the deletion.
#[test]
fn a_remembered_name_is_read_back_by_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let work = profile(1);

    let mut names = ProfileNames::open(dir.path().to_path_buf());
    assert_eq!(names.get(work), None);
    names.observe(&[summary(work, "Work")]);
    assert_eq!(names.get(work).as_deref(), Some("Work"));
    names.flush();
    assert!(path(dir.path()).is_file());

    // The Profile is deleted on another device while this one is shut down:
    // the next launch sees a catalog without it, and still has the name.
    let mut relaunched = ProfileNames::open(dir.path().to_path_buf());
    assert_eq!(relaunched.get(work).as_deref(), Some("Work"));
    relaunched.observe(&[]);
    assert_eq!(relaunched.get(work).as_deref(), Some("Work"));
}

/// The privacy half: a Profile removed on this device leaves no name behind,
/// on disk or in memory.
#[test]
fn forgetting_a_profile_removes_its_name_from_the_disk_too() {
    let dir = tempfile::tempdir().unwrap();
    let work = profile(1);
    let home = profile(2);

    let mut names = ProfileNames::open(dir.path().to_path_buf());
    names.observe(&[summary(work, "Work"), summary(home, "Home")]);
    names.flush();

    names.forget(work);
    assert_eq!(names.get(work), None);
    assert_eq!(names.get(home).as_deref(), Some("Home"));

    let body = std::fs::read_to_string(path(dir.path())).unwrap();
    assert!(!body.contains("Work"), "{body}");
    assert!(body.contains("Home"), "{body}");

    let relaunched = ProfileNames::open(dir.path().to_path_buf());
    assert_eq!(relaunched.get(work), None);
    assert_eq!(relaunched.get(home).as_deref(), Some("Home"));
}

#[test]
fn a_missing_profile_directory_is_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let names = ProfileNames::open(dir.path().join("never-created"));
    assert_eq!(names.get(profile(1)), None);
}
