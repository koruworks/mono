// SPDX-License-Identifier: MPL-2.0

//! **The setup card's frame**, laid out in memory: its width beside the
//! welcome sidebar, its fit at the smallest windows in every language, its
//! targets, its square profile and workspace tiles with hover pencil
//! affordances, and its place in the inspector. Nothing here opens a window,
//! touches a disk or reaches a Store.

use super::*;

#[test]
fn workspace_setup_is_centered_and_switches_between_welcome_and_signin() {
    for (width, height) in [(720.0, 640.0), (1000.0, 680.0), (1440.0, 900.0)] {
        let mut view = view(width, height);
        view.set_custom_caption(true);
        view.show_workspace_setup();
        assert_eq!(view.screen, Screen::Entry);
        assert!(view.welcome_cloud_enabled);
        assert!(!view.welcome_signin_mode);
        let (_, welcome_frame) = view.welcome_modal_frame();
        let welcome_card = rect(&welcome_frame, super::super::welcome_modal::KEY_CARD);
        let name_r = rect(&welcome_frame, super::super::welcome_modal::KEY_NAME);
        let ws_r = rect(
            &welcome_frame,
            super::super::welcome_modal::KEY_WORKSPACE_NAME,
        );
        let avatar_r = rect(&welcome_frame, super::super::welcome_modal::KEY_AVATAR);
        let tile_r = rect(&welcome_frame, super::super::welcome_modal::KEY_TILE);
        assert_eq!(name_r.h, 38.0);
        assert_eq!(ws_r.h, 38.0);
        assert_eq!((avatar_r.w, avatar_r.h), (38.0, 38.0));
        assert_eq!((tile_r.w, tile_r.h), (38.0, 38.0));
        let area = view.setup_area();
        assert!((welcome_card.x + welcome_card.w * 0.5 - width * 0.5).abs() < 0.01);
        assert!((welcome_card.y + welcome_card.h * 0.5 - (area.y + area.h * 0.5)).abs() < 0.01);
        let records = capture(&mut view);
        assert!(!records.iter().any(|r| r.name == "WelcomeSidebar"));
        // The focused input's caret is recorded as a `Cursor` part inside
        // `KoruTextInput`, never as an anonymous `fill` primitive.
        let cursor_idx = records
            .iter()
            .position(|r| r.name == "Cursor" && r.kind == "part")
            .expect("focused input records a Cursor part");
        let input_idx = records
            .iter()
            .position(|r| {
                r.name == "KoruTextInput"
                    && within(
                        &records,
                        cursor_idx,
                        records.iter().position(|n| std::ptr::eq(n, r)).unwrap(),
                    )
            })
            .expect("Cursor is nested inside KoruTextInput");
        assert!(within(&records, cursor_idx, input_idx));

        // Click "Sign in" to enter the compact Welcome back view, then return.
        view.activate(Hit::WelcomeExisting);
        assert_eq!(view.screen, Screen::Entry);
        assert!(view.welcome_signin_mode);
        let (_, signin_frame) = view.welcome_modal_frame();
        let card = rect(&signin_frame, super::super::welcome_modal::KEY_CARD);
        assert!((card.x + card.w * 0.5 - width * 0.5).abs() < 0.01);
        assert!((card.y + card.h * 0.5 - (area.y + area.h * 0.5)).abs() < 0.01);
        assert!(
            signin_frame
                .node(super::super::welcome_modal::KEY_SET_UP_SPACE)
                .is_some()
        );
        assert!(view.controls().contains(&Hit::Back));
        assert!(view.pending.is_none());

        view.activate(Hit::Back);
        assert_eq!(view.screen, Screen::Entry);
        assert!(!view.welcome_signin_mode);
    }
}

#[test]
fn workspace_setup_keeps_input_in_the_card_and_submits_local_creation() {
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    let mut view = view(1000.0, 680.0);
    view.show_workspace_setup();
    let (_, expanded_f) = view.welcome_modal_frame();
    let expanded_cloud = rect(&expanded_f, super::super::welcome_modal::KEY_CLOUD_CARD);
    assert_eq!(
        rect(&expanded_f, super::super::welcome_modal::KEY_GOOGLE).h,
        38.0
    );
    assert_eq!(
        rect(&expanded_f, super::super::welcome_modal::KEY_APPLE).h,
        38.0
    );
    assert_eq!(
        koru_i18n::t(view.language, koru_i18n::Key::WelcomeCloudTitle),
        "Add Koru Cloud"
    );
    let track_r = rect(&expanded_f, super::super::welcome_modal::KEY_BILLING_TRACK);
    let monthly_r = rect(
        &expanded_f,
        super::super::welcome_modal::KEY_BILLING_MONTHLY,
    );
    let yearly_r = rect(&expanded_f, super::super::welcome_modal::KEY_BILLING_YEARLY);
    // Inline underline-tab selector: flush-left Monthly, spaced gap before Yearly.
    assert!((track_r.x - monthly_r.x).abs() < 0.01);
    assert!(yearly_r.x > monthly_r.x + monthly_r.w);
    assert!((track_r.h - monthly_r.h).abs() < 0.01);
    assert!((monthly_r.h - yearly_r.h).abs() < 0.01);
    view.activate(Hit::CloudToggle);
    assert!(!view.welcome_cloud_enabled);
    // Turning Koru Cloud off keeps the full card and billing selector visible,
    // disabled from pointer hits and keyboard navigation.
    let (_, f) = view.welcome_modal_frame();
    let disabled_cloud = rect(&f, super::super::welcome_modal::KEY_CLOUD_CARD);
    assert!(
        (disabled_cloud.h - expanded_cloud.h).abs() < 0.5,
        "cloud card height should stay steady when toggled off ({} vs {})",
        disabled_cloud.h,
        expanded_cloud.h
    );
    assert!(
        f.node(super::super::welcome_modal::KEY_BILLING_TRACK)
            .is_some()
    );
    let disabled_monthly = rect(&f, super::super::welcome_modal::KEY_BILLING_MONTHLY);
    let disabled_yearly = rect(&f, super::super::welcome_modal::KEY_BILLING_YEARLY);
    assert!(
        view.welcome_modal_hit(
            disabled_monthly.x + disabled_monthly.w * 0.5,
            disabled_monthly.y + disabled_monthly.h * 0.5,
        )
        .is_none()
    );
    assert!(
        view.welcome_modal_hit(
            disabled_yearly.x + disabled_yearly.w * 0.5,
            disabled_yearly.y + disabled_yearly.h * 0.5,
        )
        .is_none()
    );
    assert!(!view.controls().contains(&Hit::BillingMonthly));
    assert!(!view.controls().contains(&Hit::BillingYearly));
    // Hovering the disabled "Start using Koru" button explains why it is disabled.
    let toggle_r = rect(&f, super::super::welcome_modal::KEY_CLOUD_TOGGLE);
    assert_eq!(toggle_r.w, koru_ui::toggle::Toggle::TRACK_W);
    assert_eq!(toggle_r.h, koru_ui::toggle::Toggle::H);
    let primary_r = rect(&f, super::super::welcome_modal::KEY_CONTINUE);
    assert_eq!(primary_r.h, 38.0);
    view.mouse_move(
        primary_r.x + primary_r.w * 0.5,
        primary_r.y + primary_r.h * 0.5,
    );
    assert_eq!(
        view.tooltip.label(),
        Some(koru_i18n::t(
            view.language,
            koru_i18n::Key::WelcomePrimaryDisabledNeedName
        ))
    );
    view.owner_name
        .borrow_mut()
        .set_text(&mut view.engine, "Mara");
    let start = view.focus;
    assert_eq!(view.controls().get(start), Some(&Hit::Name));
    let controls = view.controls();
    for _ in 0..controls.len() {
        view.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
    }
    assert_eq!(view.focus, start);
    view.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty());
    assert_eq!(view.screen, Screen::Entry);
    assert!(view.pending.is_none());
    view.activate(Hit::Primary);
    assert!(matches!(
        view.pending,
        Some(super::super::Action::CreateLocalProfile {
            connect_account: false,
            ..
        })
    ));
}

#[test]
fn welcome_modal_dispatches_ab_oauth_signed_in_checkout_and_recovery() {
    let mut view = view(1000.0, 680.0);
    view.show_workspace_setup();
    assert!(view.welcome_cloud_enabled);

    // Every A/B experiment code is populated from the closed vocabulary.
    let ab = view.onboarding_ab();
    assert!(matches!(
        ab.code.as_deref(),
        Some("sync" | "backup" | "founding")
    ));
    assert!(matches!(
        ab.header.as_deref(),
        Some("speed" | "ownership" | "purpose")
    ));
    assert!(matches!(ab.billing.as_deref(), Some("monthly" | "yearly")));
    assert_eq!(view.take_telemetry(), vec![Telemetry::Shown]);

    view.activate(Hit::Secondary);
    assert_eq!(view.take_action(), Some(Action::StartGoogle));
    assert!(view.modal_account_active());
    view.account_cancelled();
    assert!(!view.modal_account_active());

    view.activate(Hit::Apple);
    assert_eq!(view.take_action(), Some(Action::StartApple));
    view.account_cancelled();

    view.activate(Hit::ContinueWithEmail);
    assert_eq!(view.take_action(), Some(Action::StartEmailBrowser));
    assert_eq!(view.browser_provider, BrowserProvider::Email);
    view.account_cancelled();

    // Billing interval toggles cleanly between Monthly ("s1") and Yearly ("y1")
    // while keeping the assigned A/B default billing immutable.
    let initial_ab_billing = view.onboarding_ab().billing;
    view.activate(Hit::BillingMonthly);
    assert_eq!(view.welcome_billing, WelcomeBillingInterval::Monthly);
    assert_eq!(view.checkout_plan(), "s1");
    view.activate(Hit::BillingYearly);
    assert_eq!(view.welcome_billing, WelcomeBillingInterval::Yearly);
    assert_eq!(view.checkout_plan(), "y1");
    assert_eq!(view.onboarding_ab().billing, initial_ab_billing);

    // Signed-in account with 0 profiles returns to the unified Welcome screen
    // with `You` prefilled and "Continue to checkout" ready.
    view.set_account_provider("google");
    view.set_account_email("steven@example.com".into());
    view.show_account_profile_choices("Steven Liechti", Vec::new());
    assert_eq!(view.screen, Screen::Entry);
    assert!(view.welcome_signed_in);
    assert_eq!(view.owner_name.borrow().value(), "Steven Liechti");

    view.activate(Hit::Primary);
    assert_eq!(
        view.take_action(),
        Some(Action::StartCheckout { plan: "y1" })
    );

    // Checkout pending & incomplete modals fit cleanly and dispatch retry/cancel/back/offline.
    view.show_checkout_pending();
    assert!(view.is_checkout_pending());
    let (_, pending_frame) = view.checkout_modal_frame();
    assert!(
        pending_frame
            .node(super::super::checkout_modal::KEY_REOPEN)
            .is_some()
    );
    assert!(
        pending_frame
            .node(super::super::checkout_modal::KEY_CANCEL)
            .is_some()
    );

    view.activate(Hit::ReopenCheckout);
    assert_eq!(
        view.take_action(),
        Some(Action::StartCheckout { plan: "y1" })
    );

    view.activate(Hit::CancelCheckout);
    assert_eq!(view.screen, Screen::CheckoutIncomplete);
    assert_eq!(view.take_action(), Some(Action::CancelCheckout));
    let (_, incomplete_frame) = view.checkout_modal_frame();
    assert!(
        incomplete_frame
            .node(super::super::checkout_modal::KEY_RETRY)
            .is_some()
    );
    assert!(
        incomplete_frame
            .node(super::super::checkout_modal::KEY_OFFLINE)
            .is_some()
    );

    view.activate(Hit::BillingMonthly);
    view.activate(Hit::RetryCheckout);
    assert_eq!(view.screen, Screen::CheckoutPending);
    assert_eq!(
        view.take_action(),
        Some(Action::StartCheckout { plan: "s1" })
    );

    // Back on CheckoutIncomplete returns to the signed-in Welcome screen.
    view.show_checkout_incomplete();
    assert!(view.controls().contains(&Hit::Back));
    view.activate(Hit::Back);
    assert_eq!(view.screen, Screen::Entry);
    assert!(view.welcome_signed_in);

    // Completing checkout immediately creates the profile from the Welcome inputs.
    view.show_checkout_pending();
    assert!(view.finish_signed_in_welcome_after_checkout());
    assert!(matches!(
        view.take_action(),
        Some(Action::CreateLocalProfile {
            connect_account: true,
            ..
        })
    ));

    // Pricing URL obeys origin normalization and locale segments.
    assert_eq!(
        super::super::pricing_url_for(None, koru_i18n::Lang::En),
        "https://www.koru.do/en/pricing"
    );
    assert_eq!(
        super::super::pricing_url_for(
            Some("https://staging.koru.do/support"),
            koru_i18n::Lang::PtBr
        ),
        "https://staging.koru.do/pt/pricing"
    );
}

#[test]
fn welcome_modal_fits_minimum_window_in_every_language() {
    for lang in koru_i18n::Lang::ALL {
        for (cloud_on, signed_in, signin_mode) in [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let mut view = view(720.0, 640.0);
            view.language = lang;
            view.set_custom_caption(true);
            view.show_workspace_setup();
            view.welcome_cloud_enabled = cloud_on;
            view.welcome_signed_in = signed_in;
            view.welcome_signin_mode = signin_mode;
            if signed_in {
                view.set_account_email("steven@example.com".into());
            }
            let area = view.setup_area();
            let (_, frame) = view.welcome_modal_frame();
            let card = rect(&frame, super::super::welcome_modal::KEY_CARD);
            assert!(
                card.x >= area.x - 0.5 && card.x + card.w <= area.x + area.w + 0.5,
                "{lang:?} ({cloud_on},{signed_in},{signin_mode}) horizontal overflow: card={card:?} area={area:?}"
            );
            assert!(
                card.y >= area.y - 0.5 && card.y + card.h <= area.y + area.h + 0.5,
                "{lang:?} ({cloud_on},{signed_in},{signin_mode}) vertical overflow: card={card:?} area={area:?}"
            );
        }
    }
}

fn view(width: f32, height: f32) -> OnboardingView {
    let mut view = OnboardingView::default();
    view.set_viewport(width, height);
    view.show_setup();
    view
}

fn frame(view: &mut OnboardingView) -> Frame {
    view.setup_frame().1
}

fn rect(frame: &Frame, key: ElKey) -> Rect {
    frame
        .node(key)
        .unwrap_or_else(|| panic!("{key:?} is on the card"))
        .rect
}

fn capture(view: &mut OnboardingView) -> Vec<inspect::Record> {
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(
        1,
        root,
        scene::rect(0.0, 0.0, view.viewport.0, view.viewport.1),
    );
    view.draw(&mut canvas, root);
    capture.finish().records
}

fn within(records: &[inspect::Record], mut at: usize, ancestor: usize) -> bool {
    while let Some(parent) = records[at].parent {
        if parent == ancestor {
            return true;
        }
        at = parent;
    }
    false
}

/// **At most 540, never wider than the area beside the sidebar less 32**
/// (D6, Q-B): 420 at the 452 minimum, 540 at 759, 408.8 at 760 where the
/// sidebar appears, 540 at the default 1000. Always wholly beside the
/// sidebar. Embedded, it takes the host's column and draws no panel.
#[test]
fn the_card_width_follows_the_area_beside_the_sidebar() {
    for (width, height, expected) in [
        (452.0, 640.0, 420.0),
        (759.0, 640.0, 540.0),
        (760.0, 640.0, 408.8),
        (1000.0, 680.0, 540.0),
    ] {
        let mut view = view(width, height);
        let frame = frame(&mut view);
        let card = rect(&frame, KEY_CARD);
        assert!(
            (card.w - expected).abs() < 0.01,
            "{width}×{height}: card {} wide, expected {expected}",
            card.w
        );
        let (_, right) = welcome::split(width, height);
        assert!(card.x >= right.x && card.x + card.w <= right.x + right.w + 0.01);
        assert!(card.y >= right.y && card.y + card.h <= right.y + right.h + 0.01);
    }

    let mut embedded = OnboardingView::default();
    embedded.set_embedded(true);
    embedded.set_owner_label(SetupOwnerLabel::Profile);
    embedded.show_setup();
    embedded.set_viewport(480.0, 900.0);
    assert!(
        !embedded.controls().contains(&Hit::Back),
        "no Back button when embedded"
    );
    assert_eq!(embedded.setup_primary_label(), "Create Profile");
    let frame = frame(&mut embedded);
    assert!(
        frame.node(KEY_BACK).is_none(),
        "Back is omitted in embedded new-profile mode"
    );
    let card = rect(&frame, KEY_CARD);
    assert_eq!((card.x, card.y), (0.0, 0.0));
    assert!(
        (card.w - 480.0).abs() < 0.01,
        "the host's column: {}",
        card.w
    );
    let records = capture(&mut embedded);
    assert!(
        !records.iter().any(|record| record.name == "FloatingPanel"),
        "no panel when embedded"
    );
}

/// **The card fits the smallest windows in every language**, the minimum and
/// the narrowest card beside the sidebar, on the local route and the
/// connected one with its note: nothing overflows, the card's bottom is in
/// the window, and the primary is whole.
#[test]
fn the_card_fits_the_minimum_window_in_every_language() {
    for lang in koru_i18n::Lang::ALL {
        for (width, height) in [(452.0, 640.0), (760.0, 640.0)] {
            for connected in [false, true] {
                let mut view = OnboardingView::default();
                view.language = lang;
                view.set_viewport(width, height);
                if connected {
                    view.begin_connected_profile_creation();
                } else {
                    view.show_setup();
                }
                let frame = frame(&mut view);
                let case = format!("{}/{width}×{height}/connected {connected}", lang.tag());
                for node in &frame.nodes {
                    assert!(!node.overflow, "{case}: {} overflows", node.name);
                }
                let card = rect(&frame, KEY_CARD);
                assert!(
                    card.y >= 0.0 && card.y + card.h <= height,
                    "{case}: {card:?}"
                );
                assert!(
                    card.x >= 0.0 && card.x + card.w <= width,
                    "{case}: {card:?}"
                );
                let primary = rect(&frame, KEY_PRIMARY);
                assert!(primary.w > 0.0 && primary.h > 0.0, "{case}");
                assert!(
                    primary.x >= card.x
                        && primary.x + primary.w <= card.x + card.w + 0.01
                        && primary.y + primary.h <= card.y + card.h + 0.01,
                    "{case}: the primary is cut off: {primary:?} in {card:?}"
                );
                assert!(
                    frame.diagnostics.is_empty(),
                    "{case}: {:?}",
                    frame.diagnostics
                );
                view.draw(&mut Scene::new(), Affine::IDENTITY);
            }
        }
    }
}

/// **Every target comes from the frame**: a press at the middle of each
/// keyed node answers the control the table maps it to, including both the
/// profile and workspace appearance tiles.
#[test]
fn every_target_comes_from_the_frame() {
    let mut view = OnboardingView::default();
    view.set_viewport(1000.0, 680.0);
    // A parked account creation offers *Resume*; a recovery list offers the
    // link; a name makes the primary pressable.
    view.account_creation_required(&AccountState::default());
    view.activate_account_action(2);
    assert_eq!(view.screen, Screen::Setup);
    view.show_recovery(
        vec![RecoverableProfile {
            profile: koru_core::ProfileId::new(),
            deletion: koru_store_core::ids::DeletionId::new(),
            name: "Work".into(),
            at_ms: 1_000,
            workspaces: 1,
            unavailable: 0,
        }],
        Vec::new(),
    );
    view.owner_name
        .borrow_mut()
        .set_text(&mut view.engine, "Ana");
    let frame = frame(&mut view);
    for (key, hit) in [
        (KEY_AVATAR, Hit::ProfileAppearance),
        (KEY_NAME, Hit::Name),
        (KEY_TILE, Hit::WorkspaceAppearance),
        (KEY_WORKSPACE_NAME, Hit::WorkspaceName),
        (KEY_PRIMARY, Hit::Primary),
        (KEY_BACK, Hit::Back),
        (KEY_RESUME, Hit::ResumeAccount),
        (KEY_RECOVERY, Hit::Recovery),
    ] {
        let (x, y) = rect(&frame, key).centre();
        assert_eq!(view.hit(x, y), Some(hit), "{key:?}");
        assert_eq!(hit_for_key(key), Some(hit));
        assert_eq!(key_for_hit(hit), Some(key));
    }
}

/// Hovering either the profile or workspace square tile marks it hovered for
/// the pencil overlay, and clicking it opens the icon-and-color modal for
/// that target.
#[test]
fn hovering_and_clicking_profile_or_workspace_tile_opens_its_appearance_modal() {
    let mut view = view(1000.0, 680.0);
    let layout = frame(&mut view);

    for (key, expected_hit, expected_component, expected_target) in [
        (
            KEY_AVATAR,
            Hit::ProfileAppearance,
            "ProfileIcon",
            koru_appearance_ui::Target::DraftProfile,
        ),
        (
            KEY_TILE,
            Hit::WorkspaceAppearance,
            "WorkspaceIcon",
            koru_appearance_ui::Target::Draft,
        ),
    ] {
        let tile = rect(&layout, key);
        assert_eq!(tile.w, tile.h, "{expected_component} is square");
        let (x, y) = tile.centre();
        assert!(view.mouse_move(x, y));
        assert_eq!(view.hover, Some(expected_hit));
        let records = capture(&mut view);
        let icon_record = records
            .iter()
            .find(|r| r.name == expected_component)
            .unwrap_or_else(|| panic!("{expected_component} is captured"));
        assert!(
            icon_record
                .properties
                .iter()
                .any(|p| p.name == "hovered" && p.value == "true"),
            "{expected_component} shows hover pencil state"
        );
        assert!(view.mouse_down(x, y));
        let panel = view
            .workspace_appearance
            .as_ref()
            .expect("clicking tile opens modal");
        assert_eq!(panel.target, expected_target);
        view.workspace_appearance = None;
    }
}

/// **One floating panel, and the card inside it** (D7): the inspector shows
/// the card's element tree under the panel, with the shared components —
/// the name fields, the square profile and workspace tiles, and the monochrome
/// primary button — and no inline color or emoji rows.
#[test]
fn the_card_tree_nests_under_one_floating_panel_in_the_inspector() {
    let mut view = view(1000.0, 680.0);
    view.owner_name
        .borrow_mut()
        .set_text(&mut view.engine, "Ana");
    let records = capture(&mut view);
    let panels: Vec<usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| (r.name == "FloatingPanel").then_some(i))
        .collect();
    assert_eq!(panels.len(), 1, "one floating panel");
    let card = records
        .iter()
        .position(|r| r.name == CARD)
        .expect("the card's element");
    assert!(within(&records, card, panels[0]));
    for component in [
        "NameField",
        "Profile tile",
        "ProfileIcon",
        "Workspace tile",
        "WorkspaceIcon",
        "KoruButton",
    ] {
        let found: Vec<usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| (r.name == component).then_some(i))
            .collect();
        assert!(!found.is_empty(), "{component} is drawn");
        for index in found {
            assert!(within(&records, index, card), "{component} is in the card");
        }
    }
    for removed in ["KoruAvatar", "ColorPickerRow", "KoruSwatch", "KoruEmojiRow"] {
        assert!(
            !records.iter().any(|r| r.name == removed),
            "{removed} is no longer on the setup card"
        );
    }
    let primary = records
        .iter()
        .find(|r| {
            r.name == "KoruButton"
                && r.properties
                    .iter()
                    .any(|p| p.name == "variant" && p.value == "Inverted")
        })
        .expect("Start using Koru wears the monochrome Inverted button role");
    assert!(
        primary
            .properties
            .iter()
            .any(|p| p.name == "background token" && p.value == "theme::p().text")
    );
}
