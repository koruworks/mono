// SPDX-License-Identifier: MPL-2.0

//! The ladder, the compare-and-set, the ceiling and the group rules, with no
//! database anywhere near them. Every case here is one A1 §6 asks for; the
//! storage half is pinned in `koru-store-sqlite`.

use super::*;

fn owner() -> ProfileId {
    ProfileId::parse("11111111-1111-4111-8111-111111111111").unwrap()
}

fn ws(n: u8) -> WorkspaceId {
    WorkspaceId::parse(&format!("2222222{n}-2222-4222-8222-222222222222")).unwrap()
}

fn profile_row(mode: TransportMode, explicit: bool, revision: i64) -> TransportModeState {
    TransportModeState {
        subject: TransportSubject::Profile(owner()),
        owner: owner(),
        mode,
        explicit_choice: explicit,
        revision,
    }
}

fn child_row(n: u8, mode: TransportMode, explicit: bool, revision: i64) -> TransportModeState {
    TransportModeState {
        subject: TransportSubject::Workspace(ws(n)),
        owner: owner(),
        mode,
        explicit_choice: explicit,
        revision,
    }
}

fn group(profile: TransportModeState, children: Vec<TransportModeState>) -> TransportModeGroup {
    TransportModeGroup { profile, children }
}

fn set(subject: TransportSubject, expected: i64, mode: TransportMode) -> TransportModeRequest {
    TransportModeRequest::Set(SetTransportMode {
        subject,
        expected_revision: expected,
        mode,
    })
}

fn init(subject: TransportSubject, expected: i64, mode: TransportMode) -> TransportModeRequest {
    TransportModeRequest::Initialize(InitializeTransportMode {
        subject,
        expected_revision: expected,
        mode,
    })
}

// ------------------------------------------------------------------- ladder

#[test]
fn the_ladder_is_local_then_peer_then_cloud() {
    assert!(TransportMode::LocalOnly < TransportMode::P2p);
    assert!(TransportMode::P2p < TransportMode::Cloud);
    assert!(!TransportMode::LocalOnly.allows_peer());
    assert!(TransportMode::P2p.allows_peer());
    assert!(!TransportMode::P2p.allows_cloud());
    assert!(TransportMode::Cloud.allows_peer());
    assert!(TransportMode::Cloud.allows_cloud());
}

#[test]
fn stored_tokens_round_trip_and_an_unknown_one_is_corruption() {
    for mode in TransportMode::ALL {
        assert_eq!(TransportMode::parse(mode.as_storage()).unwrap(), *mode);
    }
    // Never a default: a mode this build cannot decode is reported, and the
    // affected transport stops. Silently reading it as Local-only would lose a
    // person's explicit Cloud choice on the next write.
    let error = TransportMode::parse("satellite").unwrap_err();
    assert_eq!(error.code(), Code::CorruptState);
}

// ----------------------------------------------------- defaults and pinning

#[test]
fn a_matching_revision_sets_an_explicit_choice_and_advances_once() {
    let g = group(profile_row(TransportMode::LocalOnly, false, 1), vec![]);
    let plan = plan(&g, &[set(g.profile.subject, 1, TransportMode::LocalOnly)]).unwrap();

    // Choosing the already-effective default is still a choice: it must pin
    // the row against a later verified promotion.
    let state = plan.outcomes[0].state();
    assert!(plan.outcomes[0].wrote());
    assert!(state.explicit_choice);
    assert_eq!(state.mode, TransportMode::LocalOnly);
    assert_eq!(state.revision, 2);
    assert_eq!(plan.writes.len(), 1);
}

#[test]
fn the_exact_duplicate_of_a_committed_choice_writes_nothing() {
    let g = group(profile_row(TransportMode::P2p, true, 7), vec![]);
    let plan = plan(&g, &[set(g.profile.subject, 7, TransportMode::P2p)]).unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::AlreadyCurrent(_)
    ));
    assert_eq!(plan.outcomes[0].state().revision, 7, "revision advanced");
    assert!(plan.writes.is_empty());
}

#[test]
fn verified_evidence_promotes_a_still_default_row() {
    let g = group(profile_row(TransportMode::LocalOnly, false, 1), vec![]);
    let plan = plan(&g, &[init(g.profile.subject, 1, TransportMode::Cloud)]).unwrap();

    let state = plan.outcomes[0].state();
    assert_eq!(state.mode, TransportMode::Cloud);
    assert!(
        !state.explicit_choice,
        "evidence is a default, not a choice"
    );
    assert_eq!(state.revision, 2);
}

#[test]
fn an_initializer_never_modifies_an_explicit_row() {
    // Even with the revision it expects: an explicit choice always wins, and
    // the initializer is told the current value so it stops rather than
    // replaying.
    let g = group(profile_row(TransportMode::LocalOnly, true, 4), vec![]);
    let plan = plan(&g, &[init(g.profile.subject, 4, TransportMode::Cloud)]).unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::Superseded(_)
    ));
    assert_eq!(plan.outcomes[0].state().mode, TransportMode::LocalOnly);
    assert!(plan.writes.is_empty());
}

#[test]
fn a_delayed_initializer_loses_the_race_to_an_explicit_choice() {
    // The default-versus-selection race A1 §6 asks for: the initializer read
    // revision 1, a person chose Local-only meanwhile (revision 2, explicit),
    // and the late promotion must not land.
    let g = group(profile_row(TransportMode::LocalOnly, true, 2), vec![]);
    let plan = plan(&g, &[init(g.profile.subject, 1, TransportMode::Cloud)]).unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::Superseded(_)
    ));
    assert_eq!(plan.outcomes[0].state().revision, 2);
    assert!(plan.writes.is_empty());
}

#[test]
fn initializing_twice_from_the_same_evidence_is_idempotent() {
    let g = group(profile_row(TransportMode::P2p, false, 3), vec![]);
    let plan = plan(&g, &[init(g.profile.subject, 3, TransportMode::P2p)]).unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::AlreadyCurrent(_)
    ));
    assert!(plan.writes.is_empty());
}

// ----------------------------------------------------- stale and unknown

#[test]
fn a_mismatched_revision_with_a_different_value_returns_the_current_state() {
    let g = group(profile_row(TransportMode::Cloud, true, 9), vec![]);
    let plan = plan(&g, &[set(g.profile.subject, 3, TransportMode::P2p)]).unwrap();

    match plan.outcomes[0] {
        TransportModeCommit::Superseded(state) => {
            assert_eq!(state.mode, TransportMode::Cloud);
            assert_eq!(state.revision, 9);
        }
        other => panic!("{other:?}"),
    }
    assert!(plan.writes.is_empty());
}

#[test]
fn a_stale_retry_of_an_already_committed_choice_succeeds_without_writing() {
    // The unknown-commit case: the first attempt committed but its answer was
    // lost, so the caller retries with the revision it last saw. The requested
    // explicit state already equals the current one, so this is success —
    // and the revision does not move a second time.
    let g = group(profile_row(TransportMode::P2p, true, 6), vec![]);
    let plan = plan(&g, &[set(g.profile.subject, 5, TransportMode::P2p)]).unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::AlreadyCurrent(_)
    ));
    assert_eq!(plan.outcomes[0].state().revision, 6);
    assert!(plan.writes.is_empty());
}

#[test]
fn a_stale_retry_whose_value_only_matches_a_default_is_not_success() {
    // Same mode, but the row is still a default — so the caller's explicit
    // choice did *not* land, and telling it otherwise would leave the row open
    // to a later promotion the person thought they had prevented.
    let g = group(profile_row(TransportMode::P2p, false, 6), vec![]);
    let plan = plan(&g, &[set(g.profile.subject, 5, TransportMode::P2p)]).unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::Superseded(_)
    ));
}

#[test]
fn an_exhausted_revision_refuses_the_mutation() {
    let g = group(
        profile_row(TransportMode::LocalOnly, false, i64::MAX),
        vec![],
    );
    let error = plan(
        &g,
        &[set(g.profile.subject, i64::MAX, TransportMode::Cloud)],
    )
    .unwrap_err();

    assert_eq!(error.code(), Code::CorruptState);
}

// ------------------------------------------------------------- the ceiling

#[test]
fn a_default_child_is_capped_at_the_profile_ceiling() {
    let g = group(
        profile_row(TransportMode::P2p, true, 2),
        vec![child_row(1, TransportMode::LocalOnly, false, 1)],
    );
    let plan = plan(
        &g,
        &[init(
            TransportSubject::Workspace(ws(1)),
            1,
            TransportMode::Cloud,
        )],
    )
    .unwrap();

    let state = plan.outcomes[0].state();
    assert_eq!(state.mode, TransportMode::P2p, "capped, not refused");
    assert!(!state.explicit_choice);
    // And the Profile was not touched to make room for it.
    assert_eq!(plan.writes.len(), 1);
    assert_eq!(plan.writes[0].subject, TransportSubject::Workspace(ws(1)));
}

#[test]
fn an_explicit_child_above_the_profile_is_refused_rather_than_capped() {
    // Capping a *deliberate* choice would be the same silent rewrite as
    // raising the Profile. The caller is told to raise the Profile first.
    let g = group(
        profile_row(TransportMode::P2p, true, 2),
        vec![child_row(1, TransportMode::P2p, false, 1)],
    );
    let error = plan(
        &g,
        &[set(
            TransportSubject::Workspace(ws(1)),
            1,
            TransportMode::Cloud,
        )],
    )
    .unwrap_err();

    assert_eq!(error.code(), Code::TransportModeCeiling);
}

#[test]
fn raising_the_profile_and_the_child_together_commits_as_one_group() {
    let g = group(
        profile_row(TransportMode::P2p, false, 1),
        vec![child_row(1, TransportMode::P2p, false, 1)],
    );
    let plan = plan(
        &g,
        &[
            set(TransportSubject::Workspace(ws(1)), 1, TransportMode::Cloud),
            set(g.profile.subject, 1, TransportMode::Cloud),
        ],
    )
    .unwrap();

    assert_eq!(plan.writes.len(), 2);
    for outcome in &plan.outcomes {
        assert_eq!(outcome.state().mode, TransportMode::Cloud);
        assert!(outcome.state().explicit_choice);
    }
}

#[test]
fn the_profile_is_decided_before_its_children_whatever_order_the_caller_used() {
    // The child is listed first, and would be refused if the ceiling were read
    // from the Profile's *previous* value. A caller's list order must not
    // change what a group means.
    let g = group(
        profile_row(TransportMode::LocalOnly, false, 1),
        vec![child_row(1, TransportMode::LocalOnly, false, 1)],
    );
    let plan = plan(
        &g,
        &[
            init(TransportSubject::Workspace(ws(1)), 1, TransportMode::Cloud),
            init(g.profile.subject, 1, TransportMode::Cloud),
        ],
    )
    .unwrap();

    assert_eq!(plan.outcomes[0].state().mode, TransportMode::Cloud);
    assert_eq!(plan.outcomes[1].state().mode, TransportMode::Cloud);
}

// ------------------------------------------------- downgrade and atomicity

#[test]
fn a_profile_downgrade_refuses_while_a_higher_child_is_unresolved() {
    let g = group(
        profile_row(TransportMode::Cloud, true, 3),
        vec![
            child_row(1, TransportMode::Cloud, true, 2),
            child_row(2, TransportMode::LocalOnly, false, 1),
        ],
    );
    let error = plan(&g, &[set(g.profile.subject, 3, TransportMode::LocalOnly)]).unwrap_err();

    assert_eq!(error.code(), Code::TransportModeChildren);
}

#[test]
fn a_resolved_downgrade_group_commits_atomically() {
    let g = group(
        profile_row(TransportMode::Cloud, true, 3),
        vec![
            child_row(1, TransportMode::Cloud, true, 2),
            child_row(2, TransportMode::P2p, false, 1),
        ],
    );
    let plan = plan(
        &g,
        &[
            set(g.profile.subject, 3, TransportMode::LocalOnly),
            set(
                TransportSubject::Workspace(ws(1)),
                2,
                TransportMode::LocalOnly,
            ),
            set(
                TransportSubject::Workspace(ws(2)),
                1,
                TransportMode::LocalOnly,
            ),
        ],
    )
    .unwrap();

    assert_eq!(plan.writes.len(), 3);
    for write in &plan.writes {
        assert_eq!(write.mode, TransportMode::LocalOnly);
    }
}

#[test]
fn one_invalid_member_refuses_the_whole_group() {
    // The Profile drops to P2P and one child follows, but the other is named
    // with a mode above the new ceiling. Nothing may half-change.
    let g = group(
        profile_row(TransportMode::Cloud, true, 3),
        vec![
            child_row(1, TransportMode::Cloud, true, 2),
            child_row(2, TransportMode::Cloud, true, 2),
        ],
    );
    let error = plan(
        &g,
        &[
            set(g.profile.subject, 3, TransportMode::P2p),
            set(TransportSubject::Workspace(ws(1)), 2, TransportMode::P2p),
            set(TransportSubject::Workspace(ws(2)), 2, TransportMode::Cloud),
        ],
    )
    .unwrap_err();

    assert_eq!(error.code(), Code::TransportModeCeiling);
}

#[test]
fn a_stale_child_in_a_downgrade_group_does_not_count_as_resolved() {
    // The resolution has to actually land. A child whose Set was superseded is
    // still above the new ceiling, so the downgrade is refused rather than
    // committing a Profile that contradicts its children.
    let g = group(
        profile_row(TransportMode::Cloud, true, 3),
        vec![child_row(1, TransportMode::Cloud, true, 9)],
    );
    let error = plan(
        &g,
        &[
            set(g.profile.subject, 3, TransportMode::LocalOnly),
            set(
                TransportSubject::Workspace(ws(1)),
                2,
                TransportMode::LocalOnly,
            ),
        ],
    )
    .unwrap_err();

    assert_eq!(error.code(), Code::TransportModeChildren);
}

#[test]
fn a_group_naming_one_subject_twice_is_refused() {
    let g = group(profile_row(TransportMode::LocalOnly, false, 1), vec![]);
    let error = plan(
        &g,
        &[
            set(g.profile.subject, 1, TransportMode::P2p),
            set(g.profile.subject, 2, TransportMode::Cloud),
        ],
    )
    .unwrap_err();

    assert_eq!(error.code(), Code::InvalidRequest);
}

#[test]
fn an_empty_group_is_refused() {
    let g = group(profile_row(TransportMode::LocalOnly, false, 1), vec![]);
    assert_eq!(plan(&g, &[]).unwrap_err().code(), Code::InvalidRequest);
}

// ------------------------------------------------------------------- fence

#[test]
fn closing_the_fence_invalidates_the_generation_and_reopening_does_too() {
    let mut fence = Fence::new();
    let generation = fence.generation();
    let subject = TransportSubject::Workspace(ws(1));

    fence.close([subject]);
    assert!(fence.is_closed(subject, owner()));
    assert_ne!(
        fence.generation(),
        generation,
        "old answers must not be reused"
    );

    let closed_generation = fence.generation();
    fence.reopen([subject]);
    assert!(!fence.is_closed(subject, owner()));
    assert_ne!(fence.generation(), closed_generation);
}

#[test]
fn a_closed_profile_closes_its_children_too() {
    let mut fence = Fence::new();
    fence.close([TransportSubject::Profile(owner())]);

    assert!(fence.is_closed(TransportSubject::Workspace(ws(1)), owner()));
}

#[test]
fn closing_a_subject_that_is_already_closed_does_not_reopen_it() {
    let mut fence = Fence::new();
    let subject = TransportSubject::Profile(owner());
    fence.close([subject]);
    fence.close([subject]);

    assert!(fence.is_closed(subject, owner()));
    assert_eq!(fence.closed().count(), 1);
}

#[test]
fn outcomes_come_back_in_the_order_the_caller_sent_them() {
    // The decision is made Profile-first, whatever order the list is in. The
    // *answer* must still line up with the list, or a caller matching outcome
    // to request by index reads somebody else's result.
    let g = group(
        profile_row(TransportMode::LocalOnly, false, 1),
        vec![
            child_row(1, TransportMode::LocalOnly, false, 5),
            child_row(2, TransportMode::LocalOnly, false, 1),
        ],
    );
    let plan = plan(
        &g,
        &[
            // stale: revision 5, not 1
            set(TransportSubject::Workspace(ws(1)), 1, TransportMode::P2p),
            set(g.profile.subject, 1, TransportMode::P2p),
            set(TransportSubject::Workspace(ws(2)), 1, TransportMode::P2p),
        ],
    )
    .unwrap();

    assert!(matches!(
        plan.outcomes[0],
        TransportModeCommit::Superseded(_)
    ));
    assert_eq!(
        plan.outcomes[1].state().subject,
        g.profile.subject,
        "the Profile's outcome is where the caller put the Profile"
    );
    assert!(plan.outcomes[2].wrote());
    assert_eq!(
        plan.outcomes[2].state().subject,
        TransportSubject::Workspace(ws(2))
    );
}

// ---------------------------------------------- the evidence ladder (B8 §3)

#[test]
fn the_verified_default_follows_cloud_then_peer_then_local() {
    assert_eq!(
        SharingEvidence {
            hosted_enabled_with_account: true,
            peer_selected: false,
        }
        .default_mode(),
        TransportMode::Cloud
    );
    // Cloud wins over P2P when both are verified: the ladder is ordered, not
    // a set of independent switches.
    assert_eq!(
        SharingEvidence {
            hosted_enabled_with_account: true,
            peer_selected: true,
        }
        .default_mode(),
        TransportMode::Cloud
    );
    assert_eq!(
        SharingEvidence {
            hosted_enabled_with_account: false,
            peer_selected: true,
        }
        .default_mode(),
        TransportMode::P2p
    );
    assert_eq!(
        SharingEvidence::default().default_mode(),
        TransportMode::LocalOnly,
        "no evidence is Local-only, which is what migration 12 already seeded"
    );
}

#[test]
fn evidence_promotes_a_default_row_and_never_touches_a_choice() {
    let cloud = SharingEvidence {
        hosted_enabled_with_account: true,
        peer_selected: true,
    };
    assert!(cloud.promotes(&child_row(1, TransportMode::LocalOnly, false, 1)));
    assert!(cloud.promotes(&child_row(1, TransportMode::P2p, false, 3)));
    // Already there: nothing to write, and a write would only spend a revision.
    assert!(!cloud.promotes(&child_row(1, TransportMode::Cloud, false, 3)));
    // **An explicit row always wins**, however strong the evidence.
    assert!(!cloud.promotes(&child_row(1, TransportMode::LocalOnly, true, 2)));
}

#[test]
fn absent_evidence_never_demotes_a_verified_default() {
    // A Workspace promoted to Cloud yesterday, and an Agent that cannot see
    // the account today. §3: absence of evidence is not a command.
    let row = child_row(1, TransportMode::Cloud, false, 2);
    assert!(!SharingEvidence::default().promotes(&row));
    assert!(
        !SharingEvidence {
            hosted_enabled_with_account: false,
            peer_selected: true,
        }
        .promotes(&row)
    );
}

// ------------------------------------------------- linking is the choice

#[test]
fn a_standalone_device_seeds_local_only_and_a_linked_one_admits_peers() {
    assert_eq!(
        DeviceLinkage::Standalone.seeded_mode(),
        TransportMode::LocalOnly
    );
    assert_eq!(DeviceLinkage::Linked.seeded_mode(), TransportMode::P2p);
    assert!(DeviceLinkage::Linked.seeded_mode().allows_peer());
    // **Linking is not consent to Cloud.** Two of the person's own devices
    // talking to each other is what they asked for; a hosted copy is still the
    // Profile prompt's business and nothing else's (D258).
    assert!(!DeviceLinkage::Linked.seeded_mode().allows_cloud());
    assert_eq!(DeviceLinkage::default(), DeviceLinkage::Standalone);
    assert_eq!(DeviceLinkage::from_linked(true), DeviceLinkage::Linked);
    assert_eq!(DeviceLinkage::from_linked(false), DeviceLinkage::Standalone);
}

#[test]
fn a_childs_seed_takes_an_explicit_profile_choice_whichever_way_it_points() {
    for linkage in [DeviceLinkage::Standalone, DeviceLinkage::Linked] {
        // The person answered the prompt: that answer is what a child created
        // under it starts at, above *or* below the linkage default.
        assert_eq!(
            seeded_child_mode(&profile_row(TransportMode::Cloud, true, 4), linkage),
            TransportMode::Cloud,
        );
        assert_eq!(
            seeded_child_mode(&profile_row(TransportMode::LocalOnly, true, 4), linkage),
            TransportMode::LocalOnly,
            "a Profile somebody turned off does not get re-opened by linkage",
        );
    }
}

#[test]
fn an_unspoken_profile_hands_down_the_linkage_default_capped_at_its_own_row() {
    // Profile seeded before linking, Workspace created after. The Profile row
    // is **not** promoted, so the cap is what the child gets: seeding it P2P
    // here would be a `ceiling_mismatch`, which stops the transport instead of
    // opening it.
    assert_eq!(
        seeded_child_mode(
            &profile_row(TransportMode::LocalOnly, false, 1),
            DeviceLinkage::Linked
        ),
        TransportMode::LocalOnly,
    );
    // Profile seeded after linking: both rows admit the peer path, and no
    // mismatch exists to report.
    assert_eq!(
        seeded_child_mode(
            &profile_row(TransportMode::P2p, false, 1),
            DeviceLinkage::Linked
        ),
        TransportMode::P2p,
    );
    // Standalone is exactly what it was before this existed.
    assert_eq!(
        seeded_child_mode(
            &profile_row(TransportMode::LocalOnly, false, 1),
            DeviceLinkage::Standalone
        ),
        TransportMode::LocalOnly,
    );
    // A default the withdrawn evidence path had raised is still only a
    // default, so the linkage seed caps the child rather than copying it.
    assert_eq!(
        seeded_child_mode(
            &profile_row(TransportMode::Cloud, false, 2),
            DeviceLinkage::Linked
        ),
        TransportMode::P2p,
    );
}
