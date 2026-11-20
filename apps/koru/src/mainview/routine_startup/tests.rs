// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_api_client::ConfirmedFailure;
use koru_ui::confirm::Outcome;
use koru_workspace_host::View;
use winit::keyboard::{Key as KeyboardKey, NamedKey};

fn waiting() -> StartupOffer {
    let mut offer = StartupOffer::default();
    offer.first_enabled();
    offer
}

fn asking() -> StartupOffer {
    let mut offer = waiting();
    offer.observe(false);
    assert!(offer.offer(false));
    offer
}

fn facts(enabled: bool, explained: bool) -> koru_window::ReminderStartupFacts {
    koru_window::ReminderStartupFacts {
        start_on_login: enabled,
        explained,
    }
}

#[test]
fn a_settings_read_alone_never_offers_or_enables_startup() {
    let mut offer = StartupOffer::default();
    assert!(offer.observe(false));
    assert!(!offer.offer(false));
    assert!(offer.answer(true).is_none());
    assert_eq!(offer.stage, Stage::Idle);
}

#[test]
fn first_enable_waits_for_known_settings_and_a_free_modal_slot() {
    let mut offer = waiting();
    assert!(!offer.offer(false));
    assert!(!offer.offer(true));
    offer.first_enabled();
    assert_eq!(offer.stage, Stage::Waiting);
    assert!(offer.observe(false));
    assert!(!offer.offer(true));
    assert_eq!(offer.stage, Stage::Waiting);
    assert!(offer.offer(false));
    assert!(!offer.offer(false));
}

#[test]
fn already_enabled_startup_needs_no_offer_or_write() {
    let mut offer = waiting();
    offer.observe(true);
    assert!(!offer.offer(false));
    assert_eq!(offer.stage, Stage::Done);
    assert!(offer.answer(true).is_none());

    // A setting changed by another window while the question was open is
    // also sufficient; accepting does not issue a redundant write.
    let mut offer = asking();
    offer.observe(true);
    assert!(offer.answer(true).is_none());
    assert_eq!(offer.stage, Stage::Done);
}

#[test]
fn declining_writes_nothing_and_a_repeated_first_marker_does_not_reopen() {
    let mut offer = asking();
    assert!(offer.answer(false).is_none());
    offer.first_enabled();
    assert!(!offer.offer(false));
    assert_eq!(offer.stage, Stage::Done);
    assert!(offer.receipt.is_none());
}

#[test]
fn explicit_accept_changes_only_the_existing_startup_preference() {
    let mut offer = asking();
    let (generation, patch) = offer.answer(true).unwrap();
    assert_eq!(offer.stage, Stage::Saving(generation));
    assert_eq!(
        patch,
        GlobalPatch {
            start_on_login: Some(true),
            ..Default::default()
        }
    );
    for explained in [false, true] {
        let before = GlobalSettings {
            reminder_start_on_login_explained: explained,
            ..Default::default()
        };
        let mut after = before.clone();
        patch.clone().apply(&mut after);
        assert_eq!(
            after,
            GlobalSettings {
                start_on_login: true,
                ..before
            }
        );
    }
    // A repeated keyboard/pointer settlement cannot submit twice.
    assert!(offer.answer(true).is_none());
}

#[test]
fn settings_observations_and_old_replies_cannot_acknowledge_the_pending_write() {
    let mut offer = asking();
    let (generation, _) = offer.answer(true).unwrap();
    let enabled = GlobalSettings {
        start_on_login: true,
        ..Default::default()
    };
    offer.observe(true);
    assert_eq!(offer.stage, Stage::Saving(generation));
    assert!(!offer.finish(
        generation.wrapping_sub(1),
        ConfirmedOutcome::Accepted(enabled.clone())
    ));
    assert_eq!(offer.stage, Stage::Saving(generation));
    assert!(offer.finish(generation, ConfirmedOutcome::Accepted(enabled)));
    assert_eq!(offer.stage, Stage::Done);
    assert!(!offer.offer(false));
}

#[test]
fn failed_writes_require_another_explicit_answer_and_reject_old_results() {
    let failures = [
        ConfirmedOutcome::Refused {
            status: 503,
            code: "unavailable".into(),
        },
        ConfirmedOutcome::Disconnected,
        ConfirmedOutcome::Stale,
        ConfirmedOutcome::TimedOut,
        ConfirmedOutcome::ContractMismatch {
            authoritative: GlobalSettings::default(),
        },
        ConfirmedOutcome::Reconciled {
            authoritative: GlobalSettings::default(),
            failure: ConfirmedFailure::Disconnected,
        },
    ];
    for failure in failures {
        let mut offer = asking();
        let (first, _) = offer.answer(true).unwrap();
        assert!(offer.finish(first, failure));
        assert_eq!(offer.stage, Stage::Waiting);
        assert!(offer.retry);
        assert!(offer.answer(true).is_none(), "waiting is not consent");
        assert!(!offer.offer(true), "retry must wait behind another modal");
        assert!(offer.offer(false));
        let (retry, _) = offer.answer(true).unwrap();
        assert_ne!(first, retry);
        assert!(!offer.finish(
            first,
            ConfirmedOutcome::Accepted(GlobalSettings {
                start_on_login: true,
                ..Default::default()
            })
        ));
        assert_eq!(offer.stage, Stage::Saving(retry));
    }
}

#[test]
fn a_reconciled_enabled_preference_does_not_ask_for_another_write() {
    let mut offer = asking();
    let (generation, _) = offer.answer(true).unwrap();
    assert!(offer.finish(
        generation,
        ConfirmedOutcome::Reconciled {
            authoritative: GlobalSettings {
                start_on_login: true,
                ..Default::default()
            },
            failure: ConfirmedFailure::Disconnected,
        }
    ));
    assert!(!offer.offer(false));
    assert_eq!(offer.stage, Stage::Done);
}

#[test]
fn host_queues_the_offer_until_shell_facts_arrive_and_an_existing_modal_closes() {
    let (mut view, _) = crate::mainview::harness::container();
    view.routine_startup.first_enabled();
    // The disabled test link has default false settings, not a successful
    // read. It must not authorize showing a question about that preference.
    assert!(!view.maintain_routine_startup());
    assert!(view.confirm.is_none());
    assert_eq!(view.routine_startup.stage, Stage::Waiting);
    view.confirm = Some((
        koru_ui::Confirm::asking("Existing", "Question", "Yes", "No"),
        ConfirmSubject::EmptyTrash,
    ));
    assert!(View::set_reminder_startup(&mut view, facts(false, true)));
    assert!(!view.maintain_routine_startup());
    assert_eq!(view.confirm.as_ref().unwrap().1, ConfirmSubject::EmptyTrash);
    assert!(view.settle_confirm(false));
    assert!(view.maintain_routine_startup());
    assert_eq!(
        view.confirm.as_ref().unwrap().1,
        ConfirmSubject::RoutineStartup
    );
    assert!(view.routine_startup.receipt.is_none());
}

#[test]
fn host_default_enter_declines_without_writing_or_clearing_other_work() {
    let (mut view, _) = crate::mainview::harness::container();
    view.routine_startup.first_enabled();
    View::set_reminder_startup(&mut view, facts(false, false));
    assert!(view.maintain_routine_startup());
    view.pending_empty_trash = Some(false);
    let outcome = view
        .confirm
        .as_mut()
        .unwrap()
        .0
        .key(&KeyboardKey::Named(NamedKey::Enter));
    assert_eq!(outcome, Outcome::Cancel);
    assert!(view.settle_confirm_outcome(outcome));
    assert!(view.routine_startup.receipt.is_none());
    assert_eq!(view.pending_empty_trash, Some(false));
    assert_eq!(
        view.delegate_folder.authoritative(),
        &GlobalSettings::default()
    );
    assert!(!view.maintain_routine_startup());
}

#[test]
fn host_failed_settings_link_shows_retry_without_optimistically_enabling() {
    let (mut view, _) = crate::mainview::harness::container();
    view.routine_startup.first_enabled();
    View::set_reminder_startup(&mut view, facts(false, false));
    assert!(view.maintain_routine_startup());
    let dialog = &mut view.confirm.as_mut().unwrap().0;
    assert_eq!(
        dialog.key(&KeyboardKey::Named(NamedKey::Tab)),
        Outcome::Keep
    );
    let outcome = dialog.key(&KeyboardKey::Named(NamedKey::Enter));
    assert_eq!(outcome, Outcome::Confirm);
    assert!(view.settle_confirm_outcome(outcome));
    assert!(view.routine_startup.receipt.is_some());
    assert_eq!(
        view.delegate_folder.authoritative(),
        &GlobalSettings::default()
    );
    // The deliberately disabled link reports a synthetic disconnected receipt.
    assert!(view.maintain_routine_startup());
    assert!(view.routine_startup.receipt.is_none());
    assert_eq!(view.routine_startup.stage, Stage::Asking);
    assert!(
        view.confirm
            .as_ref()
            .unwrap()
            .0
            .body()
            .contains(t(crate::format::lang(), Key::BotsRoutineStartupFailed))
    );
    assert!(view.settle_confirm(false));
    assert_eq!(
        view.delegate_folder.authoritative(),
        &GlobalSettings::default()
    );
    assert!(!view.maintain_routine_startup());
}

#[test]
fn host_pointer_accept_uses_the_shared_dialog_answer_and_submits_once() {
    let (mut view, _) = crate::mainview::harness::container();
    view.routine_startup.first_enabled();
    View::set_reminder_startup(&mut view, facts(false, false));
    assert!(view.maintain_routine_startup());
    let dialog = &mut view.confirm.as_mut().unwrap().0;
    let (_, _, rect) = dialog
        .answers(&mut view.engine, view.viewport)
        .into_iter()
        .find(|(answer, _, _)| *answer == Outcome::Confirm)
        .unwrap();
    let outcome = dialog.mouse_down(
        &mut view.engine,
        view.viewport,
        rect.x + rect.w / 2.0,
        rect.y + rect.h / 2.0,
    );
    assert_eq!(outcome, Outcome::Confirm);
    assert!(view.settle_confirm_outcome(outcome));
    assert!(view.routine_startup.receipt.is_some());
    let stage = view.routine_startup.stage;
    assert!(!view.settle_confirm_outcome(outcome));
    assert_eq!(view.routine_startup.stage, stage);
    assert_eq!(
        view.delegate_folder.authoritative(),
        &GlobalSettings::default()
    );
}

#[test]
fn the_shared_dialog_has_localized_reason_and_defaults_to_decline() {
    for lang in Lang::ALL {
        for retry in [false, true] {
            let dialog = dialog(lang, retry);
            assert_eq!(dialog.title(), t(lang, Key::SettingsStartOnLogin));
            assert!(dialog.body().contains(t(lang, Key::BotsRoutineStartupBody)));
            assert_eq!(dialog.default_action(), Some(Outcome::Cancel));
            if retry {
                assert!(
                    dialog
                        .body()
                        .contains(t(lang, Key::BotsRoutineStartupFailed))
                );
            }
        }
    }
}
