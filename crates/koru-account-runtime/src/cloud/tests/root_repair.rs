// SPDX-License-Identifier: MPL-2.0

//! **When the withheld unit is published** (root-catalog QUESTIONS.md Q10,
//! case 1). The repair itself is the Store's; what is tested here is the one
//! thing the Store cannot know and must not guess — that this pass ran under
//! the v2 contract, read the service's *whole* catalog and finished its
//! pulls, so a Workspace still holding no unit proves no unit exists anywhere.
//!
//! The v1 window's own behaviour is asserted where the window is tested
//! (`a_404_in_the_services_own_words_engages_the_v1_window`, which now also
//! requires that no repair is asked there).

use super::*;
use crate::cloud::coordinator::Coordinator;

fn coordinator(world: Arc<Mutex<World>>, cancel: Cancel) -> Coordinator<FakeApi, FakeHost> {
    Coordinator::new(
        FakeApi(world.clone()),
        FakeHost(world),
        Arc::new(Steps::default()),
        cancel,
    )
}

fn run(world: Arc<Mutex<World>>, cancel: Cancel) {
    let clock = PauseClock::new();
    let mut c = coordinator(world, cancel);
    c.apply(
        Command::Activate(Some(authority("1")), Some(7)),
        clock.as_ref(),
    );
    c.dispatch(clock.as_ref(), true);
}

/// The unit for Workspace 1, as the Store answers it once the repair has
/// published it.
fn withheld_unit() -> CloudRootUnitState {
    CloudRootUnitState {
        unit: JoinUnit::WorkspaceCatalog {
            workspace: workspace(1),
        },
        frontier: vec![5],
    }
}

/// One Workspace whose unit the v1 window withheld, a Profile unit on the
/// service to pull so the pull phase is observable, and an export ready for
/// the repaired unit so the push phase is too.
fn window_leftover() -> Arc<Mutex<World>> {
    let world = one_hosted_pair();
    {
        let mut state = lock(&world);
        let profile_unit = CloudUnitRefV2::ProfileCatalog {
            subject_id: profile(1),
        };
        state.service_units.insert(profile_unit, vec![7]);
        state.service_frames.insert(profile_unit, vec![vec![1]]);
        state.local_frontiers.insert("profile:1".into(), vec![3]);
        state.withheld.push(withheld_unit());
        state.exports.insert(
            "workspace:1".into(),
            CloudRootExport::Published {
                frames: vec![vec![9]],
                redacted_ops: 0,
            },
        );
    }
    world
}

fn position(log: &[String], entry: &str) -> usize {
    log.iter()
        .position(|e| e == entry)
        .unwrap_or_else(|| panic!("{entry} in {log:?}"))
}

#[test]
fn a_complete_v2_inventory_read_publishes_the_withheld_unit_after_the_pulls_and_pushes_it() {
    let world = window_leftover();

    run(world.clone(), Cancel::default());

    let state = lock(&world);
    let log = &state.log;
    assert_eq!(
        log.iter().filter(|entry| *entry == "repair").count(),
        1,
        "once per pass, not once per unit: {log:?}"
    );
    // The order is the whole precondition: the catalog read, then every pull,
    // then the repair, then the push that carries what it created.
    assert!(position(log, "catalog") < position(log, "repair"));
    assert!(
        position(log, "pull:profile:1") < position(log, "repair"),
        "any unit Cloud holds is joined before a row is called unit-less: {log:?}"
    );
    assert!(position(log, "repair") < position(log, "export:workspace:1"));
    assert!(position(log, "export:workspace:1") < position(log, "push:workspace:1"));
    assert!(
        state.withheld.is_empty(),
        "the Store published it, so it is no longer withheld"
    );
    assert_eq!(
        state.pushed,
        vec![CloudUnitRefV2::WorkspaceCatalog {
            subject_id: workspace(1)
        }],
        "the repaired unit reaches Cloud in the same pass, not the next one"
    );
}

#[test]
fn a_catalog_read_cancelled_part_way_never_repairs() {
    let world = window_leftover();
    let cancel = Cancel::default();
    {
        let mut state = lock(&world);
        // The first page names a cursor and the pass is cancelled while it is
        // in flight, so the loop ends without the absent cursor that is the
        // only honest proof the service holds no unit.
        state.catalog_next = Some(CloudUnitRefV2::ProfileCatalog {
            subject_id: profile(1),
        });
        state.cancel_catalog_page = Some(cancel.clone());
    }

    run(world.clone(), cancel.clone());

    assert!(cancel.cancelled(), "the cancellation happened in the loop");
    let state = lock(&world);
    let log = &state.log;
    assert!(
        !log.contains(&"repair".to_owned()),
        "a partial read cannot establish that no unit exists: {log:?}"
    );
    assert_eq!(
        state.withheld,
        vec![withheld_unit()],
        "the unit is still withheld, for the next complete pass"
    );
    assert!(state.pushed.is_empty());
}

#[test]
fn a_paged_catalog_read_that_reaches_the_last_page_still_repairs() {
    let world = window_leftover();
    {
        // Two pages, neither cancelled: the loop ends on the absent cursor of
        // the second. The gate is the end of the pagination, not the first
        // page.
        lock(&world).catalog_next = Some(CloudUnitRefV2::ProfileCatalog {
            subject_id: profile(1),
        });
    }

    run(world.clone(), Cancel::default());

    let state = lock(&world);
    let log = &state.log;
    assert_eq!(
        log.iter().filter(|entry| *entry == "catalog").count(),
        2,
        "both pages were read: {log:?}"
    );
    assert!(log.contains(&"repair".to_owned()), "{log:?}");
    assert!(state.withheld.is_empty());
}

#[test]
fn a_device_with_nothing_withheld_asks_once_and_changes_nothing() {
    let world = one_hosted_pair();
    {
        let mut state = lock(&world);
        // This device holds Workspace 1's unit already, and Cloud has it at
        // the same frontier, so there is nothing to pull, nothing to push and
        // nothing to repair.
        let unit = CloudUnitRefV2::WorkspaceCatalog {
            subject_id: workspace(1),
        };
        state.service_units.insert(unit, vec![5]);
        state.local_units.push(withheld_unit());
        state.local_frontiers.insert("workspace:1".into(), vec![5]);
    }

    run(world.clone(), Cancel::default());

    let state = lock(&world);
    let log = &state.log;
    assert_eq!(
        log.iter().filter(|entry| *entry == "repair").count(),
        1,
        "the pass asks; whether anything is unit-less is the Store's answer"
    );
    assert_eq!(
        state.local_units,
        vec![withheld_unit()],
        "a Workspace that already has a unit is left exactly as it was"
    );
    assert!(state.pushed.is_empty(), "nothing was republished: {log:?}");
}
