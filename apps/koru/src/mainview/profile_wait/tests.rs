// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::harness::*;
use koru_workspace_host::accessibility::{ViewSlot, WindowSlot};
use koru_workspace_host::profile_wait::WaitAttempt;

fn waiting(reading: WaitReading, sequence: u64) -> ProfileWait {
    ProfileWait {
        attempt: WaitAttempt {
            window: winit::window::WindowId::from(1),
            profile: ProfileId::new(),
            sequence,
        },
        reading,
    }
}

#[test]
fn lease_expiry_updates_the_menu_sentence_without_a_catalog_push() {
    use koru_store_core::{
        Timestamp,
        profile_removal::{RemovalKind, RemovalMark},
    };
    let (mut view, _) = container();
    let mark = RemovalMark {
        attempt: koru_store_core::ids::RemovalAttemptId::new(),
        kind: RemovalKind::Local,
        started_at: Timestamp::now(),
        last_step_at: Timestamp::now(),
        stopped: false,
    };
    View::set_profile_wait(&mut view, Some(waiting(WaitReading::Removing { mark }, 1)));
    let due = view.profile_wait_deadline().unwrap();
    let WaitReading::Removing { mark } = &mut view.profile_wait.waiting.as_mut().unwrap().reading
    else {
        unreachable!()
    };
    mark.last_step_at = Timestamp::from_millis(0);
    assert!(view.maintain_profile_wait(due));
    let expected = koru_i18n::t(crate::format::lang(), Words::ProfileRemovalInterrupted);
    assert!(
        !view.explore.set_workspace_waiting(Some(expected.into())),
        "menu already shows the expired lease reading"
    );
    assert!(view.wait_actions().iter().any(|(key, _)| *key == KEEP));
    assert!(view.profile_wait_deadline().is_none());
}

#[test]
fn interrupted_removal_has_exact_recovery_controls_and_live_lease_has_only_cancel_and_manage() {
    use koru_store_core::profile_removal::{RemovalKind, RemovalMark};
    for kind in [RemovalKind::Local, RemovalKind::HostedCleanup] {
        for stopped in [false, true] {
            let (mut view, _) = container();
            let mark = RemovalMark {
                attempt: koru_store_core::ids::RemovalAttemptId::new(),
                kind,
                started_at: koru_store_core::Timestamp::now(),
                last_step_at: koru_store_core::Timestamp::now(),
                stopped,
            };
            let wait = waiting(WaitReading::Removing { mark }, 5);
            View::set_profile_wait(&mut view, Some(wait.clone()));
            let keys: Vec<_> = view
                .wait_actions()
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            assert!(keys.contains(&CANCEL));
            assert_eq!(keys.contains(&RESUME), stopped);
            assert_eq!(keys.contains(&KEEP), stopped && kind == RemovalKind::Local);
            assert_eq!(view.profile_wait_deadline().is_some(), !stopped);
            assert!(
                !view.profile_wait_animating(),
                "lease expiry is a single wake, not an animation"
            );
            if stopped {
                assert!(view.activate_wait(RESUME));
                let request = View::take_pane_request(&mut view).unwrap();
                assert_eq!(
                    request,
                    if kind == RemovalKind::Local {
                        PaneRequest::RemoveLocalProfile {
                            profile: wait.attempt.profile,
                            delete_workspaces: true,
                        }
                    } else {
                        PaneRequest::FinishHostedProfileDeletion {
                            profile: wait.attempt.profile,
                            discovered_after_offline: true,
                        }
                    }
                );
            } else {
                assert!(!view.activate_wait(KEEP));
                assert!(!view.activate_wait(RESUME));
                let due = view.profile_wait_deadline().unwrap();
                assert!(view.maintain_profile_wait(due));
                assert!(view.profile_wait_deadline().is_none());
                assert!(!view.maintain_profile_wait(due));
            }
        }
    }
}

#[test]
fn every_wait_reading_has_cancel_from_the_element_frame_and_no_hidden_document() {
    for reading in [
        WaitReading::Unknown,
        WaitReading::Preparing,
        WaitReading::Cloud {
            enrollment: false,
            counts: Some((1, 3)),
        },
        WaitReading::Pending { workspaces: vec![] },
        WaitReading::Uncertain { issues: vec![] },
        WaitReading::ListingPending { issues: vec![] },
        WaitReading::Unavailable {
            cause: "storage_failed".into(),
        },
        WaitReading::EnrollmentFailed {
            code: None,
            retry_at_millis: None,
        },
        WaitReading::ContentFailed {
            code: Some("session_revoked".into()),
            automatic: false,
            retry_at_millis: None,
        },
    ] {
        let (mut view, _) = container();
        let wait = waiting(reading, 1);
        View::set_profile_wait(&mut view, Some(wait.clone()));
        assert!(view.profile_wait_visible());
        let rect = view.with_wait_frame(|frame| {
            let rect = frame.node(CANCEL).unwrap().rect;
            assert_eq!(
                frame.hit_at(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
                Some(CANCEL)
            );
            rect
        });
        assert!(view.profile_wait_mouse_down(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0));
        assert_eq!(
            View::take_pane_request(&mut view),
            Some(PaneRequest::CancelProfileWait(wait.attempt))
        );
    }
}

#[test]
fn retry_and_sign_in_are_reading_specific_and_old_accessibility_actions_are_refused() {
    let (mut view, _) = container();
    let wait = waiting(
        WaitReading::EnrollmentFailed {
            code: None,
            retry_at_millis: None,
        },
        1,
    );
    View::set_profile_wait(&mut view, Some(wait.clone()));
    assert!(view.activate_wait(RETRY));
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::RetryProfileWait(wait.attempt))
    );
    let old = Occurrence::instance(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        SURFACE,
        1,
        ControlPart::new("cancel"),
    );
    View::set_profile_wait(&mut view, Some(waiting(WaitReading::Preparing, 2)));
    assert!(!view.profile_wait_action(old, accesskit::Action::Click));
    assert!(!view.activate_wait(RETRY));
    assert!(!view.activate_wait(CLOUD));
    assert!(View::take_pane_request(&mut view).is_none());
    View::set_profile_wait(&mut view, None);
    assert!(!view.profile_wait_visible());
    assert!(!view.profile_wait_animating());
}

#[test]
fn waiting_keystrokes_cannot_promote_a_hidden_draft_and_escape_cancels() {
    let (mut view, _) = container();
    let wait = waiting(WaitReading::Unknown, 1);
    View::set_profile_wait(&mut view, Some(wait.clone()));
    assert!(view.profile_wait_key(&ch("x"), ModifiersState::empty()));
    assert!(view.profile_wait_key(&ch("v"), ModifiersState::CONTROL));
    assert!(View::take_pane_request(&mut view).is_none());
    assert!(view.profile_wait_key(&Key::Named(NamedKey::Escape), ModifiersState::empty()));
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::CancelProfileWait(wait.attempt))
    );
}

#[test]
fn waiting_withholds_document_semantics_and_dictation_and_uses_painted_bounds() {
    let (mut view, _) = on_document();
    View::set_profile_wait(&mut view, Some(waiting(WaitReading::Unknown, 3)));
    assert!(matches!(
        view.editable_target(),
        koru_workspace_host::routing::EditableTarget::None
    ));
    let cx = accessibility::Shell {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(1),
        viewport: view.viewport,
        lang: koru_i18n::Lang::En,
    };
    let semantics = view.profile_wait_semantics(&cx).unwrap();
    fn find(node: &SemanticNode) -> Option<&SemanticNode> {
        if node.occurrence.part == ControlPart::new("cancel") {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    let cancel = find(&semantics).unwrap();
    let rect = view.with_wait_frame(|frame| frame.node(CANCEL).unwrap().rect);
    assert_eq!(cancel.bounds, Some(rect.into()));
    assert_eq!(view.profile_wait_focus(&cx), Some(cancel.occurrence));
    assert!(view.profile_wait_action(cancel.occurrence, accesskit::Action::Focus));
    assert!(view.profile_wait_action(cancel.occurrence, accesskit::Action::Click));
}

#[test]
fn only_active_work_animates_and_failure_never_claims_admission_counts() {
    let (mut view, _) = container();
    for (reading, animated) in [
        (WaitReading::Unknown, false),
        (WaitReading::Preparing, true),
        (
            WaitReading::Cloud {
                enrollment: true,
                counts: None,
            },
            true,
        ),
        (
            WaitReading::ContentFailed {
                code: Some("session_revoked".into()),
                automatic: false,
                retry_at_millis: None,
            },
            false,
        ),
    ] {
        View::set_profile_wait(&mut view, Some(waiting(reading, 1)));
        assert_eq!(view.profile_wait_animating(), animated);
        view.with_wait_frame(|frame| {
            assert!(frame.node(el::Key::surface(SURFACE, "counts")).is_none())
        });
    }
}

#[test]
fn auxiliary_windows_never_enter_availability_waits() {
    for flags in [
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        let (mut view, _) = container();
        (
            view.search_only,
            view.feedback_only,
            view.support_only,
            view.fab_only,
        ) = flags;
        assert!(!View::set_profile_wait(
            &mut view,
            Some(waiting(WaitReading::Preparing, 1))
        ));
        assert!(view.profile_wait.waiting.is_none());
        assert!(!view.profile_wait_animating());
    }
}

#[test]
fn a_reading_that_removes_retry_returns_focus_to_cancel() {
    let (mut view, _) = container();
    let mut wait = waiting(
        WaitReading::Unavailable {
            cause: "storage_failed".into(),
        },
        1,
    );
    View::set_profile_wait(&mut view, Some(wait.clone()));
    view.profile_wait.focus = RETRY;
    wait.reading = WaitReading::Preparing;
    View::set_profile_wait(&mut view, Some(wait));
    assert_eq!(view.profile_wait.focus, CANCEL);
}

#[test]
fn delivery_failures_have_localized_cause_specific_messages() {
    use koru_i18n::{Key as Words, Lang, t};
    for lang in Lang::ALL {
        for (code, expected) in [
            ("profile_access_revoked", Words::ProfileWaitAccess),
            ("cloud_selection_denied", Words::ProfileWaitAccess),
            ("attestation_invalid", Words::ProfileWaitVerify),
            ("older_peer", Words::ProfileWaitPeerUpdate),
            ("cloud_offline", Words::ProfileWaitOffline),
            ("cloud_dataset_mismatch", Words::ProfileWaitDataset),
            ("cloud_bootstrap_failed", Words::ProfileWaitProvisioning),
            ("cloud_descriptor_not_ready", Words::ProfileWaitProvisioning),
            ("enrollment_rate_limited", Words::ProfileWaitRateLimited),
            ("credential_expired", Words::ProfileWaitCredential),
            ("trust_persistence_failed", Words::ProfileWaitStorage),
            ("cloud_access_revoked", Words::ProfileWaitSignIn),
            ("unknown_future_code", Words::CloudUnavailable),
        ] {
            assert_eq!(
                message(
                    &WaitReading::ContentFailed {
                        code: Some(code.into()),
                        automatic: true,
                        retry_at_millis: None,
                    },
                    lang
                ),
                t(lang, expected),
                "{lang:?} {code}"
            );
        }
    }
}
