// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_i18n::{Lang, with_current};

#[test]
fn help_focus_follows_action_when_an_offer_changes_or_disappears() {
    use winit::keyboard::{Key as InputKey, NamedKey};
    let mut view = ExploreView::new();
    view.set_help(model(UpdateStage::Available));
    view.open_popover(PopoverKind::Help);
    assert_eq!(
        view.popover_rows()[view.live().unwrap().hover.unwrap()],
        PopoverRow::Help(Action::Offer)
    );
    view.set_help(model(UpdateStage::Ready));
    assert_eq!(
        view.popover_rows()[view.live().unwrap().hover.unwrap()],
        PopoverRow::Help(Action::Offer)
    );
    view.set_help(model(UpdateStage::Downloading));
    assert_eq!(
        view.popover_rows()[view.live().unwrap().hover.unwrap()],
        PopoverRow::Help(Action::Current)
    );
    view.set_help(model(UpdateStage::Available));
    assert_eq!(
        view.popover_rows()[view.live().unwrap().hover.unwrap()],
        PopoverRow::Help(Action::Current)
    );
    assert_eq!(
        view.popover_key(&InputKey::Named(NamedKey::Space)),
        Some(crate::panes::Outcome::Help(Action::Current))
    );
    assert!(!view.popover_open());
}

#[test]
fn help_elements_keep_every_action_reachable_in_all_languages_and_themes() {
    use koru_draw::el::{FrameCx, TextCache};
    use koru_draw::{
        geometry::Rect,
        theme::{Contrast, themes},
    };
    for lang in Lang::ALL {
        with_current(lang, || {
            for theme in [&themes::LIGHT_THEME, &themes::DARK_THEME] {
                let mut view = ExploreView::new();
                let mut data = model(UpdateStage::Ready);
                data.report.as_mut().unwrap().active_session_count = 1234;
                data.headline =
                    Some("Release notes with a long headline that must remain readable".repeat(3));
                view.set_help(data);
                view.open_popover(PopoverKind::Help);
                let area = Rect::new(0.0, 0.0, 280.0, 600.0);
                let viewport = (720.0, 600.0);
                let panel = view.settled_panel(area, viewport).unwrap();
                assert!(panel.y >= 0.0 && panel.y + panel.h <= viewport.1);
                assert!(panel.x + panel.w <= viewport.0);
                let tree = view.help_page(0.0);
                let frame = crate::input_engine::with(|engine| {
                    tree.layout(
                        engine,
                        &mut TextCache::default(),
                        panel,
                        FrameCx::new(theme, Contrast::Standard),
                    )
                });
                for action in Action::ALL {
                    let node = frame.node(action.key()).expect("action is published");
                    assert!(
                        node.rect.w > 0.0 && node.rect.h > 0.0,
                        "{lang:?} {action:?}"
                    );
                    assert!(frame.semantics().iter().any(|menu| {
                        menu.children
                            .iter()
                            .any(|item| item.key == Some(action.key()))
                    }));
                }
                view.popover.as_mut().unwrap().hover = Some(6);
                view.reveal_popover_hover(area, viewport);
                view.with_page(area, viewport, |frame| {
                    let node = frame.node(Action::X.key()).unwrap();
                    assert!(
                        node.rect.y >= panel.y && node.rect.y + node.rect.h <= panel.y + panel.h
                    );
                    let (x, y) = node.rect.centre();
                    assert_eq!(frame.hit_at(x, y), Some(Action::X.key()));
                })
                .unwrap();
            }
        });
    }
}

fn report(stage: UpdateStage) -> UpdateReport {
    UpdateReport {
        stage,
        version: "0.9.1".into(),
        build_number: 412,
        title: "A fixture release".into(),
        summary: vec!["A fixture change".into()],
        notes_url: Some("https://example.invalid/notes/412".into()),
        received: 0,
        total: 0,
        can_install: true,
        download_url: Some("https://example.invalid/download".into()),
        message: None,
        detail: None,
        channel: "nightly".into(),
        automatic_checks: true,
        active_session_count: 0,
        session_generation: 1,
    }
}

fn model(stage: UpdateStage) -> Model {
    Model {
        report: Some(report(stage)),
        ..Model::default()
    }
}

#[derive(Clone, Copy)]
enum Visibility {
    Never,
    NamedVersion,
    PriorOffer,
}

#[derive(Clone, Copy)]
enum Button {
    None,
    DownloadOrManual,
    Restart,
    Retry,
}

/// The approved stage table, independent of the projection's branching.
/// Each row says which fact admits an offer, which button it permits, and
/// whether the offer itself contributes a dot after notes have been read.
const STAGES: [(UpdateStage, Visibility, Button, bool); 11] = [
    (UpdateStage::Idle, Visibility::Never, Button::None, false),
    (
        UpdateStage::Checking,
        Visibility::Never,
        Button::None,
        false,
    ),
    (
        UpdateStage::UpToDate,
        Visibility::Never,
        Button::None,
        false,
    ),
    (
        UpdateStage::InstalledNewer,
        Visibility::Never,
        Button::None,
        false,
    ),
    (
        UpdateStage::Available,
        Visibility::NamedVersion,
        Button::DownloadOrManual,
        true,
    ),
    (
        UpdateStage::Downloading,
        Visibility::NamedVersion,
        Button::None,
        true,
    ),
    (
        UpdateStage::Verifying,
        Visibility::NamedVersion,
        Button::None,
        true,
    ),
    (
        UpdateStage::Ready,
        Visibility::NamedVersion,
        Button::Restart,
        true,
    ),
    (
        UpdateStage::Installing,
        Visibility::NamedVersion,
        Button::None,
        true,
    ),
    (
        UpdateStage::Deferred,
        Visibility::NamedVersion,
        Button::DownloadOrManual,
        false,
    ),
    (
        UpdateStage::Failed,
        Visibility::PriorOffer,
        Button::Retry,
        true,
    ),
];

#[test]
fn every_stage_obeys_offer_action_and_dot_contract_for_all_report_inputs() {
    for (stage, visibility, button, offer_dot) in STAGES {
        for (version, named) in [("0.9.1", true), ("", false), (" \t ", false)] {
            for was_offered in [false, true] {
                for can_install in [false, true] {
                    for (url, manual) in [
                        (None, false),
                        (Some(""), false),
                        (Some(" \t "), false),
                        (Some("https://example.invalid/download"), true),
                    ] {
                        for notes_unread in [false, true] {
                            let mut report = report(stage);
                            report.version = version.into();
                            report.can_install = can_install;
                            report.download_url = url.map(str::to_owned);
                            let offer = match visibility {
                                Visibility::Never => false,
                                Visibility::NamedVersion => named,
                                Visibility::PriorOffer => was_offered,
                            };
                            let action = if offer {
                                match button {
                                    Button::None => None,
                                    Button::Restart => Some(OfferAction::Restart),
                                    Button::Retry => Some(OfferAction::Retry),
                                    Button::DownloadOrManual => match (can_install, manual) {
                                        (true, _) => Some(OfferAction::Download),
                                        (false, true) => Some(OfferAction::Manual),
                                        (false, false) => None,
                                    },
                                }
                            } else {
                                None
                            };
                            assert_eq!(
                                project(Some(&report), was_offered, notes_unread),
                                Projection {
                                    offer,
                                    action,
                                    dot: notes_unread || (offer && offer_dot)
                                },
                                "{stage:?}, version={version:?}, prior={was_offered}, install={can_install}, url={url:?}, unread={notes_unread}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn a_failed_prior_offer_keeps_retry_even_when_the_report_has_lost_its_version() {
    let mut report = report(UpdateStage::Failed);
    report.version.clear();
    report.can_install = false;
    report.download_url = None;
    assert_eq!(
        project(Some(&report), true, false),
        Projection {
            offer: true,
            action: Some(OfferAction::Retry),
            dot: true,
        }
    );
    assert_eq!(project(Some(&report), false, false), Projection::default());
}

#[test]
fn later_clears_only_the_offer_dot_and_new_offers_and_ready_restore_it() {
    let mut model = model(UpdateStage::Ready);
    assert!(
        model.projection().dot,
        "Ready keeps its dot after notes are read"
    );
    model.report.as_mut().unwrap().stage = UpdateStage::Deferred;
    assert_eq!(
        model.projection(),
        Projection {
            offer: true,
            action: Some(OfferAction::Download),
            dot: false,
        }
    );
    model.notes_unread = true;
    assert!(
        model.projection().dot,
        "Later cannot dismiss unread release notes"
    );
    model.notes_unread = false;
    assert!(!model.projection().dot);
    let report = model.report.as_mut().unwrap();
    report.stage = UpdateStage::Available;
    report.version = "0.9.2".into();
    report.build_number += 1;
    assert!(model.projection().dot, "a newer offer lights the dot again");
    model.report.as_mut().unwrap().stage = UpdateStage::Ready;
    model.notes_unread = true;
    assert!(model.projection().dot);
    model.notes_unread = false;
    assert!(model.projection().dot, "reading notes cannot dismiss Ready");
}

#[test]
fn no_report_has_only_the_independent_unread_notes_dot() {
    for was_offered in [false, true] {
        for notes_unread in [false, true] {
            assert_eq!(
                project(None, was_offered, notes_unread),
                Projection {
                    offer: false,
                    action: None,
                    dot: notes_unread,
                }
            );
        }
    }
    assert_eq!(Model::default().stage_label(), "");
}

#[test]
fn stage_and_action_labels_use_the_localized_menu_keys() {
    for lang in Lang::ALL {
        with_current(lang, || {
            for (stage, key) in [
                (UpdateStage::Available, Words::UpdateRowAvailable),
                (UpdateStage::Verifying, Words::UpdateRowVerifying),
                (UpdateStage::Ready, Words::UpdateRowReady),
                (UpdateStage::Installing, Words::UpdateRowInstalling),
                (UpdateStage::Deferred, Words::UpdateRowDeferred),
            ] {
                assert_eq!(
                    model(stage).stage_label(),
                    t(lang, key),
                    "{lang:?}/{stage:?}"
                );
            }
            for stage in [
                UpdateStage::Idle,
                UpdateStage::Checking,
                UpdateStage::UpToDate,
                UpdateStage::InstalledNewer,
            ] {
                assert_eq!(model(stage).stage_label(), "", "{stage:?}");
            }
            for stage in [UpdateStage::Available, UpdateStage::Deferred] {
                let mut model = model(stage);
                assert_eq!(model.action_label(Action::Offer), t(lang, Words::Download));
                model.report.as_mut().unwrap().can_install = false;
                assert_eq!(
                    model.action_label(Action::Offer),
                    t(lang, Words::UpdateDownloadManually)
                );
                for url in [None, Some("".into()), Some(" \t ".into())] {
                    model.report.as_mut().unwrap().download_url = url;
                    assert_eq!(model.stage_label(), t(lang, Words::UpdateRowNoRoute));
                    assert_eq!(
                        model.action_label(Action::Offer),
                        t(lang, Words::UpdateRowNoRoute)
                    );
                    assert_eq!(model.projection().action, None);
                }
            }
            let mut ready = model(UpdateStage::Ready);
            ready.report.as_mut().unwrap().can_install = false;
            ready.report.as_mut().unwrap().download_url = None;
            assert_eq!(
                ready.action_label(Action::Offer),
                t(lang, Words::UpdateRestart)
            );
            let mut failed = model(UpdateStage::Failed);
            failed.was_offered = true;
            assert_eq!(failed.stage_label(), t(lang, Words::TryAgain));
            assert_eq!(failed.action_label(Action::Offer), t(lang, Words::TryAgain));
            failed.report.as_mut().unwrap().message = Some("Fixture failure detail".into());
            assert_eq!(failed.stage_label(), "Fixture failure detail");
            assert_eq!(failed.action_label(Action::Offer), t(lang, Words::TryAgain));
            for (action, key) in [
                (Action::Current, Words::WhatsNew),
                (Action::Releases, Words::HelpViewAllReleases),
                (Action::Docs, Words::HelpAndDocs),
                (Action::Feedback, Words::SendFeedback),
                (Action::GitHub, Words::StarOnGitHub),
                (Action::X, Words::HelpFollowOnX),
            ] {
                assert_eq!(Model::default().action_label(action), t(lang, key));
            }
        });
    }
}

#[test]
fn download_progress_handles_unknown_total_rounding_overflow_and_local_digits() {
    for lang in Lang::ALL {
        with_current(lang, || {
            for (received, total, percent) in [
                (0, 0, 0),
                (10, 0, 0),
                (1, 3, 33),
                (3, 3, 100),
                (20, 10, 100),
                (u64::MAX, u64::MAX, 100),
            ] {
                let mut model = model(UpdateStage::Downloading);
                let report = model.report.as_mut().unwrap();
                report.received = received;
                report.total = total;
                let expected = fill(
                    t(lang, Words::UpdateRowDownloading),
                    &[&koru_i18n::number::decimal(lang, percent)],
                );
                assert_eq!(
                    model.stage_label(),
                    expected,
                    "{lang:?}: {received}/{total}"
                );
                assert_eq!(model.action_label(Action::Offer), expected);
                assert_eq!(model.projection().action, None);
            }
        });
    }
    with_current(Lang::Ar, || {
        let mut model = model(UpdateStage::Downloading);
        model.report.as_mut().unwrap().received = 1;
        model.report.as_mut().unwrap().total = 4;
        assert_eq!(model.stage_label(), "جارٍ التنزيل · ٢٥%");
    });
}

#[test]
fn ready_warns_about_active_sessions_without_changing_restart_action() {
    for lang in Lang::ALL {
        with_current(lang, || {
            let mut model = model(UpdateStage::Ready);
            model.report.as_mut().unwrap().active_session_count = 12_345;
            assert_eq!(
                model.stage_label(),
                fill(
                    t(lang, Words::UpdateReadyWithSessions),
                    &[&koru_i18n::number::decimal(lang, 12_345)]
                )
            );
            assert_eq!(
                model.action_label(Action::Offer),
                t(lang, Words::UpdateRestart)
            );
            assert_eq!(model.projection().action, Some(OfferAction::Restart));
            model.report.as_mut().unwrap().active_session_count = 0;
            assert_eq!(model.stage_label(), t(lang, Words::UpdateRowReady));
        });
    }
}

#[test]
fn current_release_uses_the_first_heading_or_the_localized_missing_notes_fallback() {
    for lang in Lang::ALL {
        with_current(lang, || {
            let mut model = Model::default();
            assert_eq!(
                model.current_label(),
                t(lang, Words::NotShippedWithThisBuild)
            );
            model.headline = Some("A fixture release".into());
            assert_eq!(
                model.current_label(),
                fill(t(lang, Words::HelpCurrentRelease), &["A fixture release"])
            );
            model.report = Some(report(UpdateStage::Available));
            model.report.as_mut().unwrap().title = "The offered build's title".into();
            assert_eq!(
                model.current_label(),
                fill(t(lang, Words::HelpCurrentRelease), &["A fixture release"])
            );
        });
    }
}
