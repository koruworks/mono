// SPDX-License-Identifier: MPL-2.0

//! Memory-only: the journal key and the record part three reads. The
//! detection itself is covered over real Loro documents in
//! `root_catalog::tests` (the `Q10 case 3` block), and end to end against a
//! real catalog in `koru-store-sqlite`'s `Root` fixture.

use super::*;

fn conflict(repaired: ProfileId, rival: Option<ProfileId>) -> BootstrapPlacementConflict {
    BootstrapPlacementConflict {
        repaired_profile: repaired,
        repair_move_id: Uuid::new_v4(),
        rivals: vec![RivalPlacement {
            profile: rival,
            move_id: Uuid::new_v4(),
            writer_peer: 9,
            counter: 3,
            at_ms: 1_700_000_000_000,
        }],
    }
}

#[test]
fn the_journal_key_is_one_row_per_workspace_under_the_root_catalog_prefix() {
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());

    assert!(bootstrap_conflict_key(a).starts_with(K_BOOTSTRAP_CONFLICT_PREFIX));
    assert!(K_BOOTSTRAP_CONFLICT_PREFIX.starts_with("root_catalog."));
    assert_ne!(bootstrap_conflict_key(a), bootstrap_conflict_key(b));
    assert_eq!(bootstrap_conflict_key(a), bootstrap_conflict_key(a));
    // The repair's record and the conflict's are two rows about one
    // Workspace, never one row overwriting the other.
    assert_ne!(
        bootstrap_conflict_key(a),
        super::super::bootstrap_repair_key(a)
    );
}

#[test]
fn the_record_round_trips_and_names_both_profiles_and_the_rival_operation() {
    let (repaired, rival) = (ProfileId::new(), ProfileId::new());
    let record = conflict(repaired, Some(rival));

    let json = serde_json::to_string(&record).unwrap();
    let back: BootstrapPlacementConflict = serde_json::from_str(&json).unwrap();

    assert_eq!(back, record);
    assert_eq!(back.repaired_profile, repaired);
    assert_eq!(back.rivals[0].profile, Some(rival));
    assert_eq!(back.rivals[0].operation(), loro::ID::new(9, 3));
    // Two Profiles and two move ids: enough to ask which of the two the
    // person meant, and to tell which one is in force by comparing the move
    // ids with the placement that currently stands.
    assert_ne!(back.repair_move_id, back.rivals[0].move_id);
    // No name is in it because no name is in the type.
    assert!(!json.contains("name"));
}

#[test]
fn an_explicitly_unassigned_rival_is_a_value_and_not_an_absence() {
    let record = conflict(ProfileId::new(), None);

    let back: BootstrapPlacementConflict =
        serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();

    assert_eq!(back, record);
    assert_eq!(
        back.rivals[0].profile, None,
        "somebody wrote *unassigned*, and the question offers that as the other side"
    );
}
