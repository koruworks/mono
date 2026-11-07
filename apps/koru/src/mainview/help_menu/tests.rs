// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::{PopoverKind, PopoverRow};

fn report(stage: UpdateStage) -> koru_agent_wire::UpdateReport {
    koru_agent_wire::UpdateReport {
        stage,
        version: "9.0".into(),
        build_number: 999,
        title: "Offer".into(),
        summary: vec![],
        notes_url: None,
        received: 0,
        total: 0,
        can_install: true,
        download_url: None,
        message: None,
        detail: None,
        channel: "test".into(),
        automatic_checks: false,
        active_session_count: 0,
        session_generation: 8,
    }
}

fn next_choice(sent: &std::sync::mpsc::Receiver<koru_agent_wire::FromShell>) -> UpdateChoice {
    let koru_agent_wire::FromShell::Request { body, .. } = sent.try_recv().expect("one consent");
    let koru_agent_wire::Request::UpdateChoice { choice } = body else {
        panic!("update choice")
    };
    choice
}

#[test]
fn help_press_reads_latest_report_and_shares_pending_guard() {
    let (mut view, _) = harness::container();
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report(UpdateStage::Available));
    view.updates = feed.clone();
    view.refresh_help();
    view.explore.open_popover(PopoverKind::Help);
    let mut ready = report(UpdateStage::Ready);
    ready.can_install = false;
    ready.download_url = Some("https://example.invalid/manual".into());
    ready.session_generation = 19;
    feed.replace_report(ready);
    assert!(view.help_action(Action::Offer));
    assert_eq!(
        next_choice(&sent),
        UpdateChoice::Install {
            session_generation: 19
        }
    );
    assert!(view.help_action(Action::Offer));
    assert!(
        sent.try_recv().is_err(),
        "same stage cannot queue a second install"
    );

    feed.replace_report(report(UpdateStage::Downloading));
    assert!(!view.help_action(Action::Offer));
    assert!(sent.try_recv().is_err(), "progress offers no action");
    assert!(
        !view
            .explore
            .popover_rects(view.explore_popover_area(), view.viewport)
            .iter()
            .any(|(row, _)| *row == PopoverRow::Help(Action::Offer))
    );
}

#[test]
fn deferred_download_failed_retry_and_timeout_use_existing_consent() {
    let (mut view, _) = harness::container();
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report(UpdateStage::Deferred));
    view.updates = feed.clone();
    assert!(view.help_action(Action::Offer));
    assert_eq!(next_choice(&sent), UpdateChoice::Download);
    view.update_action_pending = Some((
        UpdateStage::Deferred,
        std::time::Instant::now() - std::time::Duration::from_secs(11),
    ));
    assert!(view.help_action(Action::Offer));
    assert_eq!(next_choice(&sent), UpdateChoice::Download);
    let mut failed = report(UpdateStage::Failed);
    failed.version.clear();
    feed.replace_report(failed);
    assert!(view.help_action(Action::Offer));
    assert_eq!(next_choice(&sent), UpdateChoice::Retry);
}

#[test]
fn an_offer_superseded_before_layout_still_makes_failure_actionable() {
    let (mut view, _) = harness::container();
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report(UpdateStage::Idle));
    view.updates = feed.clone();
    feed.replace_report(report(UpdateStage::Available));
    let mut failed = report(UpdateStage::Failed);
    failed.version.clear();
    feed.replace_report(failed);
    assert!(
        !view.update_was_offered,
        "no paint or layout has adopted the offer"
    );
    assert!(view.help_action(Action::Offer));
    assert_eq!(next_choice(&sent), UpdateChoice::Retry);
}

#[test]
fn help_restart_preserves_test_upgrade_handoff_and_disconnected_failure() {
    let (mut view, _) = harness::container();
    view.updates = crate::agent::UpdateFeed::test_upgrade();
    assert!(view.help_action(Action::Offer));
    assert!(matches!(
        view.window_request,
        Some(koru_window::WindowRequest::TestUpgrade)
    ));
    assert!(view.update_action_pending.is_some());

    let (feed, sent) = crate::agent::UpdateFeed::for_test(report(UpdateStage::Available));
    drop(sent);
    view.updates = feed;
    view.update_action_pending = None;
    assert!(view.help_action(Action::Offer));
    assert!(view.update_action_pending.is_none());
    assert!(
        view.update_action_stalled.is_some(),
        "disconnected Agent is reported immediately"
    );
}

#[test]
fn manual_download_uses_latest_url_and_never_sends_install_consent() {
    let (mut view, _) = harness::container();
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report(UpdateStage::Available));
    view.updates = feed.clone();
    view.refresh_help();
    view.explore.open_popover(PopoverKind::Help);
    let mut latest = report(UpdateStage::Deferred);
    latest.can_install = false;
    latest.download_url = Some("https://example.invalid/current-package".into());
    feed.replace_report(latest.clone());
    koru_editor::browser::clear_last_launch();
    assert!(view.help_action(Action::Offer));
    assert_eq!(
        koru_editor::browser::last_launch(),
        Some(("https://example.invalid/current-package".into(), true))
    );
    assert!(sent.try_recv().is_err());
    latest.download_url = None;
    feed.replace_report(latest);
    koru_editor::browser::clear_last_launch();
    assert!(!view.help_action(Action::Offer));
    assert_eq!(koru_editor::browser::last_launch(), None);
    assert!(sent.try_recv().is_err());
}
