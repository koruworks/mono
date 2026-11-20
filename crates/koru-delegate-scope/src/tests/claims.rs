// SPDX-License-Identifier: MPL-2.0

//! D34 in memory: a manual start onto a busy scope warns and proceeds, a
//! scheduled firing onto one is skipped with the holder recorded, and
//! nothing is ever claimed for `WorkspaceScope::None`.

use std::path::Path;

use super::*;

fn spelled(path: &str) -> ScopeIdentity {
    ScopeIdentity::from_resolved(Path::new(path))
}

#[test]
fn retired_bot_folder_is_refused_before_io() {
    struct NoIo;
    impl ScopeRoots for NoIo {
        fn project_root(&self, _: ProjectId) -> RootLookup {
            panic!("must refuse before lookup")
        }
        fn temporary_directory(&self, _: RunId) -> Option<std::path::PathBuf> {
            panic!("must refuse before lookup")
        }
    }
    let mut claims = ScopeClaims::new();
    let scope = WorkspaceScope::BotFolder {
        folder: koru_delegate_core::BotWorkingFolder::new("/bot".into()).unwrap(),
    };
    assert_eq!(
        claims.admit(run_id(1), attempt_id(2), &scope, &NoIo, StartOrigin::Manual),
        Admission::Refused(ScopeRefusal::NotAdmissible)
    );
    assert_eq!(
        ResolvedScope::resolve(&scope, run_id(1), &NoIo).unwrap_err(),
        ScopeRefusal::NotAdmissible
    );
}

#[cfg(windows)]
const REPO: &str = r"C:\Work\repo";
#[cfg(windows)]
const CRATE: &str = r"C:\Work\repo\crates\one";
#[cfg(windows)]
const OTHER: &str = r"C:\Work\other";
#[cfg(not(windows))]
const REPO: &str = "/work/repo";
#[cfg(not(windows))]
const CRATE: &str = "/work/repo/crates/one";
#[cfg(not(windows))]
const OTHER: &str = "/work/other";

fn admit(
    claims: &mut ScopeClaims,
    run: u8,
    attempt: u8,
    path: &str,
    origin: StartOrigin,
) -> Admission {
    claims.admit_resolved(run_id(run), attempt_id(attempt), spelled(path), origin)
}

#[test]
fn a_free_scope_is_admitted_without_a_warning_and_is_then_held() {
    let mut claims = ScopeClaims::new();
    assert_eq!(
        admit(&mut claims, 1, 11, REPO, StartOrigin::Manual),
        Admission::Admitted { warning: None }
    );
    assert_eq!(claims.len(), 1);
    let held = claims.claims().next().unwrap();
    assert_eq!((held.run, held.attempt), (run_id(1), attempt_id(11)));
    assert_eq!(held.scope, spelled(REPO));
}

#[test]
fn a_manual_start_onto_a_held_scope_proceeds_with_a_warning_that_names_the_holder() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);

    let admission = admit(&mut claims, 2, 22, REPO, StartOrigin::Manual);
    assert_eq!(
        admission,
        Admission::Admitted {
            warning: Some(BusyWarning {
                holder: Holder {
                    run: run_id(1),
                    attempt: attempt_id(11),
                    overlap: Overlap::Same,
                },
                others: vec![],
            }),
        }
    );
    assert_eq!(claims.len(), 2, "the person proceeded, so both now hold it");
}

#[test]
fn a_manual_start_inside_or_around_a_held_scope_warns_with_the_direction() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);
    let Admission::Admitted {
        warning: Some(inside),
    } = admit(&mut claims, 2, 22, CRATE, StartOrigin::Manual)
    else {
        panic!("a manual start is admitted");
    };
    assert_eq!(
        inside.holder.overlap,
        Overlap::Nested {
            inner: Party::Candidate,
            outer: Party::Holder,
        }
    );

    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, CRATE, StartOrigin::Manual);
    let Admission::Admitted {
        warning: Some(around),
    } = admit(&mut claims, 2, 22, REPO, StartOrigin::Manual)
    else {
        panic!("a manual start is admitted");
    };
    assert_eq!(
        around.holder.overlap,
        Overlap::Nested {
            inner: Party::Holder,
            outer: Party::Candidate,
        }
    );
}

#[test]
fn a_sibling_scope_is_free() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);
    assert_eq!(
        admit(&mut claims, 2, 22, OTHER, scheduled(5)),
        Admission::Admitted { warning: None }
    );
}

#[test]
fn a_scheduled_firing_onto_a_held_scope_is_skipped_with_the_holder_recorded() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);

    let admission = admit(&mut claims, 2, 22, CRATE, scheduled(5));
    assert_eq!(
        admission,
        Admission::Skipped {
            schedule: schedule(5),
            busy: BusyWarning {
                holder: Holder {
                    run: run_id(1),
                    attempt: attempt_id(11),
                    overlap: Overlap::Nested {
                        inner: Party::Candidate,
                        outer: Party::Holder,
                    },
                },
                others: vec![],
            },
        }
    );
    assert_eq!(
        claims.len(),
        1,
        "nothing was claimed for the skipped firing"
    );
    assert!(claims.release(attempt_id(22)).is_none());
}

#[test]
fn a_scheduled_firing_onto_a_free_scope_is_admitted_and_holds_it() {
    let mut claims = ScopeClaims::new();
    assert_eq!(
        admit(&mut claims, 1, 11, REPO, scheduled(5)),
        Admission::Admitted { warning: None }
    );
    assert_eq!(claims.len(), 1);
}

#[test]
fn releasing_the_holder_frees_the_scope() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);
    let released = claims.release(attempt_id(11)).unwrap();
    assert_eq!(
        (released.run, released.attempt),
        (run_id(1), attempt_id(11))
    );
    assert!(claims.is_empty());
    assert_eq!(
        admit(&mut claims, 2, 22, REPO, scheduled(5)),
        Admission::Admitted { warning: None }
    );
    assert!(claims.release(attempt_id(11)).is_none(), "released once");
}

#[test]
fn every_holder_is_named_earliest_first() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);
    admit(&mut claims, 2, 22, CRATE, StartOrigin::Manual);
    admit(&mut claims, 3, 33, OTHER, StartOrigin::Manual);

    let Admission::Skipped { busy, .. } = admit(&mut claims, 4, 44, REPO, scheduled(5)) else {
        panic!("skipped");
    };
    let named: Vec<_> = busy.holders().map(|holder| holder.run).collect();
    assert_eq!(named, vec![run_id(1), run_id(2)]);
    assert_eq!(busy.holder.overlap, Overlap::Same);
    assert_eq!(
        busy.others[0].overlap,
        Overlap::Nested {
            inner: Party::Holder,
            outer: Party::Candidate,
        }
    );
}

#[test]
fn an_attempt_re_admitting_its_scope_is_not_its_own_holder() {
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);
    assert_eq!(
        admit(&mut claims, 1, 11, CRATE, scheduled(5)),
        Admission::Admitted { warning: None }
    );
    assert_eq!(claims.len(), 1, "replaced, not doubled");
    assert_eq!(claims.claims().next().unwrap().scope, spelled(CRATE));
}

#[test]
fn a_runs_fresh_attempt_is_warned_about_its_own_prior_attempt() {
    // §7.6: a resume is a fresh attempt, and an unknown prior attempt may
    // still be running there. The warning names the same run, honestly.
    let mut claims = ScopeClaims::new();
    admit(&mut claims, 1, 11, REPO, StartOrigin::Manual);
    let Admission::Admitted {
        warning: Some(warning),
    } = admit(&mut claims, 1, 12, REPO, StartOrigin::Manual)
    else {
        panic!("admitted");
    };
    assert_eq!(warning.holder.run, run_id(1));
    assert_eq!(warning.holder.attempt, attempt_id(11));
}

#[test]
fn no_scope_is_refused_and_claims_nothing() {
    let mut claims = ScopeClaims::new();
    let roots = Roots::default();
    assert_eq!(
        claims.admit(
            run_id(1),
            attempt_id(11),
            &WorkspaceScope::None,
            &roots,
            StartOrigin::Manual
        ),
        Admission::Refused(ScopeRefusal::NotAdmissible)
    );
    assert_eq!(
        claims.admit(
            run_id(1),
            attempt_id(11),
            &WorkspaceScope::None,
            &roots,
            scheduled(5)
        ),
        Admission::Refused(ScopeRefusal::NotAdmissible)
    );
    assert!(claims.is_empty());
}

#[test]
fn an_unresolvable_scope_is_refused_and_claims_nothing() {
    let mut claims = ScopeClaims::new();
    let roots = Roots::default();
    assert_eq!(
        claims.admit(
            run_id(1),
            attempt_id(11),
            &root_scope(7),
            &roots,
            StartOrigin::Manual
        ),
        Admission::Refused(ScopeRefusal::UnknownProject)
    );
    assert!(claims.is_empty());
}

#[test]
fn a_schedule_reference_round_trips_its_uuid() {
    let reference = schedule(5);
    assert_eq!(ScheduleRef::from_uuid(reference.as_uuid()), reference);
    assert_eq!(format!("{reference:?}"), "schedule(00000000)");
}
