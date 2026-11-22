// SPDX-License-Identifier: MPL-2.0

//! **What the first-run window offers a screen reader, and what a request to
//! one of its controls means.**
//!
//! One sentence, and what it excludes: *this module turns the first-run
//! controller's current screen into semantic nodes, and turns a request back
//! into the same call the keyboard makes.* It excludes what a control **is** —
//! a screen, a field, a swatch are [`super::OnboardingView`]'s and stay there —
//! and it excludes the window: who
//! installs a provider, when a tree is worth building and which requests are
//! still about something that exists belong to
//! [`crate::accessibility`](crate::accessibility) and to
//! `koru_window::accessibility`. It is a child module rather than a section of
//! `onboarding.rs` for the reason §7.10 of the roadmap gives: that file is
//! 3,400 lines and gets a call, not another embedded subsystem.
//!
//! ## The route this projects, and the one it does not
//!
//! Plan U0.1 is the **account-free** route: *Welcome → the setup card →
//! finishing → the result* (welcome step 1 made the old Profile and
//! Workspace steps one card). Those states are projected in full. The
//! Google route's four screens — the browser wait, the account status, the
//! hosted-profile chooser and the restore — are named by a single empty
//! [`Role::Group`] so an inspector shows a boundary rather than a gap, and are
//! otherwise left to a later plan. §3 of the roadmap is explicit that account
//! and enrollment flows are not to be pulled into minimum local setup by
//! association; an accessible name for a surface nobody has designed the
//! semantics of would be worse than an honest edge.
//!
//! ## Four rules it is built on
//!
//! **A screen is an instance, and a transition mints a new one.** The screens
//! are states of one window, not separate windows, and a control can stay in
//! the same place while it does a different thing — the welcome screen's
//! *Continue without an account* and the card's *Start using Koru* are both
//! [`Hit::Primary`], and the card's primary is a different control while the
//! card is finishing. If both carried one locator, a request queued against
//! what just went would land on the control that replaced it. So the object
//! every control belongs to is
//! [`ObjectKey::Instance`] with a number this module hands out and never
//! reuses, exactly the mechanism ADR-338 minted for menus and tabs. Its
//! successor holds a different number, so it cannot inherit the request;
//! `PaneAccessibility::resolve` finds nothing in the tree on screen and counts
//! the request refused.
//!
//! **The number is noticed, not bumped.** `self.screen` is assigned at
//! seventeen places in `onboarding.rs`, and a plan whose correctness rests on
//! every one of them calling a helper is a plan with a hole in it. Instead
//! [`OnboardingView::accessibility_semantics`] compares the screen it is about
//! to publish — the screen and whether the card is finishing — with the one
//! it published last, and mints on a difference.
//! That is not merely easier — it is *exactly* the right condition, because an
//! identity only has to differ between two trees a client actually saw. A
//! screen the client was never shown cannot have a request queued against it.
//!
//! **Geometry is never re-derived.** Every rectangle published here comes back
//! from what drawing and hit-testing already share — `welcome_layout`,
//! `content`, `koru_ui::caption::direct_controls`, and for the setup card its
//! **frame**: `frame.node(key).rect` for a control, and
//! `ColorPickerRow::wrapped_swatches` or `EmojiRow::cells` inside the row's
//! leaf for a cell.
//! `el::Frame::semantics()` and `focus_order()` are not used for the card
//! (welcome D7): the element layer has no text-input role, value or live
//! region yet, and this contract needs all three. A second copy of the
//! arithmetic is a control a client can find somewhere other than where it
//! answers a press.
//!
//! **An action is a keystroke.** [`OnboardingView::accessibility_action`] moves
//! the form's own `focus` index and then hands the equivalent key to
//! [`OnboardingView::key`]. There is no second activation route and no second
//! focus state: the ring a sighted person sees *is* what a client is told,
//! because both read `OnboardingView::focus` against
//! `OnboardingView::controls`.
//!
//! ## Losing content, and creating two Profiles
//!
//! The dangerous case in this surface is not a missing label. It is a person
//! retrying and getting two Profiles. Nothing here can do that, and it is
//! structural rather than careful: pressing *Start using Koru* puts the card
//! into finishing, whose `controls()` is empty and whose `hit()` is `None`,
//! so the primary is neither published as activatable nor reachable from a
//! client; and a request queued against the card before it was finishing
//! names an instance that is no longer on screen, so it is refused. When
//! creation fails, `creation_failed` brings the card back with every typed
//! value intact and a fresh instance, and the one retry is the person's.

use accesskit::{Action, ActionData, Live, Role};
use koru_i18n::{Key, Lang, t};

use crate::accessibility::{
    CaptionGeometry, ControlPart, LogicalRect, ObjectKey, Occurrence, Revision, SemanticNode,
    ViewSlot, WindowSlot,
};

use super::checkout_modal::{
    KEY_BACK as CM_BACK, KEY_BILLING_MONTHLY as CM_BILLING_MONTHLY,
    KEY_BILLING_YEARLY as CM_BILLING_YEARLY, KEY_BODY as CM_BODY, KEY_CANCEL as CM_CANCEL,
    KEY_CARD as CM_CARD, KEY_OFFLINE as CM_OFFLINE, KEY_REOPEN as CM_REOPEN,
    KEY_RETRY as CM_RETRY,
};
use super::setup::{
    KEY_AVATAR, KEY_BACK, KEY_CARD, KEY_LEAD, KEY_NAME, KEY_NOTE, KEY_PRIMARY, KEY_RECOVERY,
    KEY_RESUME, KEY_STATUS, KEY_TILE, KEY_WORKSPACE_NAME, KEY_WORKSPACE_ROW, KEY_YOU_ROW,
};
use super::welcome_modal::{
    KEY_ACCOUNT_CARD as WM_ACCOUNT_CARD, KEY_APPLE as WM_APPLE,
    KEY_BILLING_MONTHLY as WM_BILLING_MONTHLY, KEY_BILLING_YEARLY as WM_BILLING_YEARLY,
    KEY_CARD as WM_CARD, KEY_COMPARE_PLANS as WM_COMPARE_PLANS, KEY_CONTINUE as WM_CONTINUE,
    KEY_EMAIL_LINK as WM_EMAIL_LINK, KEY_EXISTING_CARD as WM_EXISTING_CARD,
    KEY_GOOGLE as WM_GOOGLE, KEY_LEAD as WM_LEAD, KEY_OFFLINE_CARD as WM_OFFLINE_CARD,
    KEY_RECOVERY as WM_RECOVERY, KEY_STATUS as WM_STATUS,
};
use super::{Hit, OnboardingView, Screen, WelcomeBillingInterval, WelcomePath};

/// The object kind every first-run control belongs to. A locator reads
/// `koru:w1/v1/onb:3#primary` — the `3` is the screen instance, and a person
/// looking at an inspector can tell one screen's controls from the next.
const FORM: &str = "onb";

fn next_screen_instance() -> u64 {
    static NEXT_SCREEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 32);
    NEXT_SCREEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The parts of the first-run window. Keywords chosen by the code, never a
/// label and never an index: §5's *renaming, reordering or translating must
/// not change the identity of a surviving control*.
pub mod parts {
    use crate::accessibility::ControlPart;

    /// The screen itself, and the sentences on it that name no control.
    pub const FORM: ControlPart = ControlPart::new("form");
    pub const STEP: ControlPart = ControlPart::new("step");
    pub const BLURB: ControlPart = ControlPart::new("blurb");
    pub const NOTE: ControlPart = ControlPart::new("note");
    /// What the application is doing on its own, and what went wrong.
    pub const STATUS: ControlPart = ControlPart::new("status");

    /// The welcome screen.
    pub const GOOGLE: ControlPart = ControlPart::new("google");
    pub const APPLE: ControlPart = ControlPart::new("apple");
    pub const CONTINUE_OFFLINE: ControlPart = ControlPart::new("continue-offline");
    pub const COMPARE_PLANS: ControlPart = ControlPart::new("compare-plans");
    pub const WELCOME_OFFLINE: ControlPart = ControlPart::new("welcome-offline");
    pub const WELCOME_ACCOUNT: ControlPart = ControlPart::new("welcome-account");
    pub const WELCOME_EXISTING: ControlPart = ControlPart::new("welcome-existing");
    pub const BILLING_MONTHLY: ControlPart = ControlPart::new("billing-monthly");
    pub const BILLING_YEARLY: ControlPart = ControlPart::new("billing-yearly");
    pub const REOPEN_CHECKOUT: ControlPart = ControlPart::new("reopen-checkout");
    pub const CANCEL_CHECKOUT: ControlPart = ControlPart::new("cancel-checkout");
    pub const RETRY_CHECKOUT: ControlPart = ControlPart::new("retry-checkout");
    /// *Try again* in the welcome screen's failure banner.
    pub const RETRY: ControlPart = ControlPart::new("retry");
    pub const TERMS: ControlPart = ControlPart::new("terms");
    pub const PRIVACY: ControlPart = ControlPart::new("privacy");

    /// The email journey: the welcome form, then its two screens.
    pub const EMAIL_ADDRESS: ControlPart = ControlPart::new("email-address");
    pub const EMAIL_PASSWORD: ControlPart = ControlPart::new("email-password");
    /// The eye inside the password field.
    pub const REVEAL_PASSWORD: ControlPart = ControlPart::new("reveal-password");
    pub const CONTINUE_WITH_EMAIL: ControlPart = ControlPart::new("continue-with-email");
    pub const EMAIL_CODE: ControlPart = ControlPart::new("email-code");
    pub const RESEND_CODE: ControlPart = ControlPart::new("resend-code");
    pub const ANOTHER_ADDRESS: ControlPart = ControlPart::new("another-address");
    pub const NEW_PASSWORD: ControlPart = ControlPart::new("new-password");
    pub const CONFIRM_PASSWORD: ControlPart = ControlPart::new("confirm-password");

    /// The setup card. The parts that were the old Profile and Workspace
    /// steps' keep their keywords, so a surviving control's locator does not
    /// change.
    pub const RESUME_ACCOUNT: ControlPart = ControlPart::new("resume-account");
    /// The *You* (or *Profile*) group: the profile tile and the name field.
    pub const YOU: ControlPart = ControlPart::new("you");
    pub const PROFILE_ICON: ControlPart = ControlPart::new("profile-icon");
    pub const OWNER_NAME: ControlPart = ControlPart::new("owner-name");
    /// The *First workspace* group: the tile and the workspace's name.
    pub const FIRST_WORKSPACE: ControlPart = ControlPart::new("first-workspace");
    pub const WORKSPACE_NAME: ControlPart = ControlPart::new("workspace-name");
    pub const WORKSPACE_ICON: ControlPart = ControlPart::new("workspace-icon");

    /// The card's primary, and the way back.
    pub const PRIMARY: ControlPart = ControlPart::new("primary");
    pub const BACK: ControlPart = ControlPart::new("back");
    /// The card's way to `NeedsProfile` recovery, and that screen's
    /// two lists. A restorable Profile's row is keyed by the Profile itself
    /// (`ObjectKey::Model`), as a workspace row is elsewhere.
    pub const RECOVERY: ControlPart = ControlPart::new("recovery");
    pub const DELETED_PROFILES: ControlPart = ControlPart::new("deleted-profiles");
    pub const RESTORE: ControlPart = ControlPart::new("restore");
    pub const UNASSIGNED: ControlPart = ControlPart::new("unassigned");

    /// The icon-and-colour chooser, and the Google route's screens.
    pub const APPEARANCE: ControlPart = ControlPart::new("appearance");
    pub const ACCOUNT_ROUTE: ControlPart = ControlPart::new("account-route");
}

/// **The part a control answers to on a given screen**, and the one place the
/// mapping is written.
///
/// Screen-scoped because the same [`Hit`] means different things on different
/// screens: `Hit::Primary` is *Continue offline* on the welcome screen and
/// *Start using Koru* on the card. Its inverse is [`hit_of`], and a test walks
/// every control of every screen through both.
fn part_of(screen: Screen, hit: Hit) -> Option<ControlPart> {
    use parts as p;
    Some(match (screen, hit) {
        (Screen::Entry, Hit::EmailAddress) => p::EMAIL_ADDRESS,
        (Screen::Entry, Hit::EmailPassword) => p::EMAIL_PASSWORD,
        (Screen::Entry, Hit::ContinueWithEmail) => p::CONTINUE_WITH_EMAIL,
        (Screen::Entry, Hit::Secondary) => p::GOOGLE,
        (Screen::Entry, Hit::Apple) => p::APPLE,
        (Screen::Entry, Hit::Primary) => p::CONTINUE_OFFLINE,
        (Screen::Entry, Hit::ComparePlans) => p::COMPARE_PLANS,
        (Screen::Entry, Hit::WelcomeOffline) => p::WELCOME_OFFLINE,
        (Screen::Entry, Hit::WelcomeAccount) => p::WELCOME_ACCOUNT,
        (Screen::Entry, Hit::WelcomeExisting) => p::WELCOME_EXISTING,
        (Screen::Entry | Screen::CheckoutIncomplete, Hit::BillingMonthly) => p::BILLING_MONTHLY,
        (Screen::Entry | Screen::CheckoutIncomplete, Hit::BillingYearly) => p::BILLING_YEARLY,
        (Screen::CheckoutPending, Hit::ReopenCheckout) => p::REOPEN_CHECKOUT,
        (Screen::CheckoutPending, Hit::CancelCheckout) => p::CANCEL_CHECKOUT,
        (Screen::CheckoutIncomplete, Hit::RetryCheckout) => p::RETRY_CHECKOUT,
        (Screen::CheckoutIncomplete, Hit::ContinueOffline) => p::CONTINUE_OFFLINE,
        (Screen::CheckoutIncomplete, Hit::Back) => p::BACK,
        (Screen::Entry, Hit::Recovery) => p::RECOVERY,
        (Screen::Entry, Hit::Retry) => p::RETRY,
        (Screen::Entry, Hit::RevealPassword) => p::REVEAL_PASSWORD,
        (Screen::Entry, Hit::Terms) => p::TERMS,
        (Screen::Entry, Hit::Privacy) => p::PRIVACY,
        (Screen::Setup, Hit::ResumeAccount) => p::RESUME_ACCOUNT,
        (Screen::Setup, Hit::ProfileAppearance) => p::PROFILE_ICON,
        (Screen::Setup, Hit::Name) => p::OWNER_NAME,
        (Screen::Setup, Hit::WorkspaceAppearance) => p::WORKSPACE_ICON,
        (Screen::Setup, Hit::WorkspaceName) => p::WORKSPACE_NAME,
        (Screen::Setup, Hit::Recovery) => p::RECOVERY,
        (Screen::Recovery, Hit::Back) => p::BACK,
        (Screen::EmailCode, Hit::EmailCode) => p::EMAIL_CODE,
        (Screen::EmailCode, Hit::ResendCode) => p::RESEND_CODE,
        (Screen::EmailCode, Hit::AnotherAddress) => p::ANOTHER_ADDRESS,
        (Screen::EmailPassword, Hit::NewPassword) => p::NEW_PASSWORD,
        (Screen::EmailPassword, Hit::ConfirmPassword) => p::CONFIRM_PASSWORD,
        (Screen::Setup | Screen::EmailCode | Screen::EmailPassword, Hit::Primary) => p::PRIMARY,
        (Screen::Setup, Hit::Back) => p::BACK,
        _ => return None,
    })
}

/// The inverse of [`part_of`]: which control a client's request is about.
fn hit_of(screen: Screen, part: ControlPart) -> Option<Hit> {
    use parts as p;
    let hit = match screen {
        Screen::Entry => match part {
            x if x == p::EMAIL_ADDRESS => Hit::EmailAddress,
            x if x == p::EMAIL_PASSWORD => Hit::EmailPassword,
            x if x == p::CONTINUE_WITH_EMAIL => Hit::ContinueWithEmail,
            x if x == p::GOOGLE => Hit::Secondary,
            x if x == p::APPLE => Hit::Apple,
            x if x == p::CONTINUE_OFFLINE => Hit::Primary,
            x if x == p::COMPARE_PLANS => Hit::ComparePlans,
            x if x == p::WELCOME_OFFLINE => Hit::WelcomeOffline,
            x if x == p::WELCOME_ACCOUNT => Hit::WelcomeAccount,
            x if x == p::WELCOME_EXISTING => Hit::WelcomeExisting,
            x if x == p::BILLING_MONTHLY => Hit::BillingMonthly,
            x if x == p::BILLING_YEARLY => Hit::BillingYearly,
            x if x == p::RECOVERY => Hit::Recovery,
            x if x == p::RETRY => Hit::Retry,
            x if x == p::REVEAL_PASSWORD => Hit::RevealPassword,
            x if x == p::TERMS => Hit::Terms,
            x if x == p::PRIVACY => Hit::Privacy,
            _ => return None,
        },
        Screen::Setup => match part {
            x if x == p::RESUME_ACCOUNT => Hit::ResumeAccount,
            x if x == p::PROFILE_ICON => Hit::ProfileAppearance,
            x if x == p::OWNER_NAME => Hit::Name,
            x if x == p::WORKSPACE_ICON => Hit::WorkspaceAppearance,
            x if x == p::WORKSPACE_NAME => Hit::WorkspaceName,
            x if x == p::PRIMARY => Hit::Primary,
            x if x == p::BACK => Hit::Back,
            x if x == p::RECOVERY => Hit::Recovery,
            _ => return None,
        },
        // The rows are model-keyed and routed by `accessibility_action`
        // itself; only *Back* is a form part here.
        Screen::Recovery => match part {
            x if x == p::BACK => Hit::Back,
            _ => return None,
        },
        Screen::EmailCode => match part {
            x if x == p::EMAIL_CODE => Hit::EmailCode,
            x if x == p::RESEND_CODE => Hit::ResendCode,
            x if x == p::ANOTHER_ADDRESS => Hit::AnotherAddress,
            x if x == p::PRIMARY => Hit::Primary,
            _ => return None,
        },
        Screen::EmailPassword => match part {
            x if x == p::NEW_PASSWORD => Hit::NewPassword,
            x if x == p::CONFIRM_PASSWORD => Hit::ConfirmPassword,
            x if x == p::PRIMARY => Hit::Primary,
            _ => return None,
        },
        Screen::CheckoutPending => match part {
            x if x == p::REOPEN_CHECKOUT => Hit::ReopenCheckout,
            x if x == p::CANCEL_CHECKOUT => Hit::CancelCheckout,
            _ => return None,
        },
        Screen::CheckoutIncomplete => match part {
            x if x == p::BACK => Hit::Back,
            x if x == p::BILLING_MONTHLY => Hit::BillingMonthly,
            x if x == p::BILLING_YEARLY => Hit::BillingYearly,
            x if x == p::RETRY_CHECKOUT => Hit::RetryCheckout,
            x if x == p::CONTINUE_OFFLINE => Hit::ContinueOffline,
            _ => return None,
        },
        // The Google route, and the wait the email form parks on. Their
        // screens publish one empty node and route nothing; see the module
        // note.
        Screen::Browser
        | Screen::EmailWorking
        | Screen::AccountStatus
        | Screen::ExistingProfiles
        | Screen::RestoringProfile => return None,
    };
    Some(hit)
}

impl OnboardingView {
    /// **The language this window speaks**, asked of the OS once when the form
    /// was built.
    ///
    /// The host's window commands are labelled from it too, so the caption and
    /// the form cannot end up in two languages. The same answer
    /// [`crate::accessibility::ui_lang`] gives, read off the view that already
    /// holds it rather than asked a second time.
    pub fn language(&self) -> Lang {
        self.language
    }

    /// **Where the view draws the window's own commands.**
    ///
    /// The host performs minimise, maximise and close; this window has no OS
    /// caption, so the rectangles are wherever the form painted them — from
    /// `koru_ui::caption::direct_controls`, the same function
    /// `caption_action_at` hit-tests against and `draw_caption` draws from.
    /// Empty on macOS, where the titlebar is the platform's.
    pub fn window_command_bounds(&self) -> CaptionGeometry {
        if !self.custom_caption {
            return CaptionGeometry::default();
        }
        let mut geometry = CaptionGeometry::default();
        for control in koru_ui::caption::direct_controls(self.caption_rect()) {
            let rect = Some(control.rect);
            match control.action {
                koru_ui::caption::Action::Minimise => geometry.minimize = rect,
                koru_ui::caption::Action::ToggleMaximise => geometry.maximize = rect,
                koru_ui::caption::Action::Close => geometry.close = rect,
                koru_ui::caption::Action::WindowMenu => {}
            }
        }
        geometry
    }

    /// **Everything this window's tree is built from**, folded into one number.
    ///
    /// The counterpart of `PaneAccessibility::observe`, and the whole of why an
    /// ordinary session pays nothing: with no screen reader attached this is
    /// never asked, and with one attached it changes only when something the
    /// tree carries changed.
    ///
    /// What is deliberately absent is everything that moves without changing
    /// what is published: `pointer`, `hover`, `caption_hover`,
    /// `existing_profile_scroll` and the appearance picker's own internal
    /// state. Hover is not the keyboard's position here — `focus` is, and it is
    /// folded in.
    pub fn semantics_revision(&self) -> u64 {
        let mut rev = Revision::new();
        rev.u64(self.screen_instance);
        rev.str(match self.screen {
            Screen::Entry => "entry",
            Screen::Browser => "browser",
            Screen::EmailWorking => "email-working",
            Screen::EmailCode => "email-code",
            Screen::EmailPassword => "email-password",
            Screen::AccountStatus => "account",
            Screen::CheckoutPending => "checkout-pending",
            Screen::CheckoutIncomplete => "checkout-incomplete",
            Screen::ExistingProfiles => "existing",
            Screen::RestoringProfile => "restoring",
            Screen::Recovery => "recovery",
            Screen::Setup => "setup",
        });
        rev.str(self.language.tag());
        // The recovery lists change the setup card (its link) and the
        // recovery screen (its rows).
        rev.u64(self.recovery_deleted().len() as u64);
        for row in self.recovery_deleted() {
            rev.str(&row.profile.to_storage());
            rev.str(&row.name);
            rev.u64(row.workspaces as u64);
            rev.u64(row.unavailable as u64);
        }
        rev.u64(self.recovery_unassigned().len() as u64);
        for row in self.recovery_unassigned() {
            rev.str(&row.name);
        }
        rev.f32(self.viewport.0);
        rev.f32(self.viewport.1);
        rev.bool(self.custom_caption);
        rev.bool(self.maximized);
        rev.u64(self.focus as u64);
        // The code is folded in by length, and the three secrets not at all:
        // this number is handed to the platform's accessibility bus, and a
        // password has no business on it. What a screen reader is told about
        // them is "how many characters", which is what changes the tree.
        rev.u64(self.email_address.raw_text().len() as u64);
        rev.str(self.email_address.raw_text());
        rev.u64(self.email_password.raw_text().chars().count() as u64);
        rev.u64(self.email_code.raw_text().chars().count() as u64);
        rev.u64(self.new_password.raw_text().chars().count() as u64);
        rev.u64(self.confirm_password.raw_text().chars().count() as u64);
        rev.str(self.owner_name.borrow().raw_text());
        rev.str(self.workspace_name.borrow().raw_text());
        // The card's choices, whether it is finishing and what its first field
        // is called all change what it publishes.
        rev.str(self.profile_icon.as_deref().unwrap_or(""));
        rev.str(self.profile_color.as_deref().unwrap_or(""));
        rev.str(self.workspace_icon.as_deref().unwrap_or(""));
        rev.str(self.workspace_color.as_deref().unwrap_or(""));
        rev.bool(self.setup_finishing);
        rev.bool(self.owner_label == super::SetupOwnerLabel::Profile);
        rev.bool(self.workspace_name_edited);
        rev.bool(self.connect_account);
        rev.bool(self.workspace_modal);
        rev.byte(match self.welcome_path {
            WelcomePath::Offline => 0,
            WelcomePath::CreateAccount => 1,
            WelcomePath::ExistingAccount => 2,
        });
        rev.bool(self.welcome_billing == WelcomeBillingInterval::Yearly);
        rev.str(self.account_email.as_deref().unwrap_or(""));
        rev.bool(self.creation_conflict);
        rev.bool(self.can_reopen_account_creation());
        rev.bool(self.workspace_appearance.is_some());
        match self.error.as_deref() {
            Some(error) => {
                rev.byte(1);
                rev.str(error);
            }
            None => {
                rev.byte(0);
            }
        }
        rev.finish()
    }

    /// **The first-run window, as a screen reader sees it right now.**
    ///
    /// `&mut self` and not `&self`, unlike `View::semantics`: every rectangle
    /// here comes back from a geometry function that measures text, and
    /// `TextEngine`'s layout cache is `&mut`. What that costs is the compiler's
    /// guarantee that nothing here reaches the Store — so the seam is asserted
    /// two other ways instead. `OnboardingView` owns no `Workspace`, no store
    /// session and no network handle *at all*: every effect leaves as a typed
    /// [`super::Action`] that the host performs off this thread. And a test
    /// pins the imports of this file and asserts that building a tree raises
    /// no action and no telemetry.
    ///
    /// This is also where a screen transition is noticed and the next screen's
    /// identity is minted — see the module note.
    pub fn accessibility_semantics(
        &mut self,
        window: WindowSlot,
        view: ViewSlot,
    ) -> Vec<SemanticNode> {
        let shown = (self.screen, self.setup_finishing);
        if self.screen_shown != Some(shown) {
            self.screen_shown = Some(shown);
            // Embedded forms can be closed and recreated in the same tab.
            // A new form must not reuse a departed form's control locators.
            self.screen_instance = if self.embedded {
                next_screen_instance()
            } else {
                self.screen_instance.wrapping_add(1)
            };
        }
        let cx = Context {
            window,
            view,
            instance: self.screen_instance,
            lang: self.language,
        };
        // **A modal owns everything under it**, so nothing under it is
        // published. The appearance chooser takes every key while it is up and
        // the form behind it answers nothing; a tree that still offered the
        // form would be describing controls a person cannot reach. Escape
        // closes it, which is the route out. Its own contents are
        // `koru-appearance-ui`'s surface and are not projected here.
        if self.workspace_appearance.is_some() {
            return vec![
                SemanticNode::new(cx.at(parts::APPEARANCE), Role::Dialog)
                    .with_label(t(cx.lang, Key::A11yFirstRunAppearance))
                    .with_bounds(self.appearance_area())
                    .modal(),
            ];
        }
        match self.screen {
            Screen::Entry if self.workspace_modal => vec![self.welcome_modal_semantics(&cx)],
            Screen::Entry => vec![self.entry_semantics(&cx)],
            Screen::EmailCode => vec![self.email_code_semantics(&cx)],
            Screen::EmailPassword => vec![self.email_password_semantics(&cx)],
            Screen::Setup => vec![self.setup_semantics(&cx)],
            Screen::CheckoutPending | Screen::CheckoutIncomplete => {
                vec![self.checkout_modal_semantics(&cx)]
            }
            Screen::Recovery => vec![self.recovery_semantics(&cx)],
            // The Google route, and the wait between the welcome form and
            // whichever screen follows it: named, and left to a later plan.
            Screen::Browser
            | Screen::EmailWorking
            | Screen::AccountStatus
            | Screen::ExistingProfiles
            | Screen::RestoringProfile => vec![
                SemanticNode::new(cx.at(parts::ACCOUNT_ROUTE), Role::Group)
                    .with_label(t(cx.lang, Key::A11yFirstRunAccountRoute))
                    .with_bounds(self.content()),
            ],
        }
    }

    /// **Which control the keyboard is on**, said as an occurrence.
    ///
    /// Read straight off `focus` indexing `controls()` — the same pair every
    /// `draw_*` reads to decide where to paint the focus ring — so §7.1's
    /// *visible focus and accessible focus are one state* is structural rather
    /// than two fields kept in step. `None` means the window itself, which is
    /// the honest answer while a modal is up or a screen offers no controls at
    /// all.
    /// For a row of swatches or emoji, the cell its cursor is on.
    pub fn accessibility_focus(&self, window: WindowSlot, view: ViewSlot) -> Option<Occurrence> {
        if self.workspace_appearance.is_some() {
            return Some(
                Context {
                    window,
                    view,
                    instance: self.screen_instance,
                    lang: self.language,
                }
                .at(parts::APPEARANCE),
            );
        }
        let hit = self.controls().get(self.focus).copied()?;
        if let (Screen::Recovery, Hit::RestoreDeleted(index)) = (self.screen, hit) {
            let row = self.recovery_deleted().get(index)?;
            return Some(Occurrence::model(
                window,
                view,
                row.profile.as_uuid(),
                parts::RESTORE,
            ));
        }
        let part = part_of(self.screen, hit)?;
        Some(Occurrence::instance(
            window,
            view,
            FORM,
            self.screen_instance,
            part,
        ))
    }

    /// **Do what a client asked, through the key the keyboard already uses.**
    ///
    /// `false` means *this form does not perform that action on that control*,
    /// which is the honest answer to the `Focus` Windows sends ahead of every
    /// invoke on something that is not focusable, and to a request naming a
    /// screen that has gone.
    ///
    /// The first guard is the whole of stale refusal at this level: a request
    /// is performed only when the screen it names is still the screen the tree
    /// was built for **and** the instance number matches. Both halves are
    /// needed, because a batch of requests is resolved after one publish and
    /// the first of them may itself have changed the screen.
    pub fn accessibility_action(
        &mut self,
        target: Occurrence,
        action: Action,
        data: Option<&ActionData>,
    ) -> bool {
        if self.screen_shown != Some((self.screen, self.setup_finishing)) {
            return false;
        }
        // Nothing under the icon-and-colour chooser is published while it is
        // up, so nothing under it is routed either. `PaneAccessibility` would
        // refuse it anyway — the control is not in the tree on screen — and
        // giving the same answer locally keeps this module from reading as if
        // it offered a route it does not have.
        if self.workspace_appearance.is_some() {
            return false;
        }
        let hit = match target.object {
            // A restorable Profile's row is the Profile itself.
            ObjectKey::Model(profile)
                if self.screen == Screen::Recovery && target.part == parts::RESTORE =>
            {
                let Some(index) = self
                    .recovery_deleted()
                    .iter()
                    .position(|row| row.profile.as_uuid() == profile)
                else {
                    return false;
                };
                Hit::RestoreDeleted(index)
            }
            ObjectKey::Instance { kind, id } => {
                if kind != FORM || id != self.screen_instance {
                    return false;
                }
                let Some(hit) = hit_of(self.screen, target.part) else {
                    return false;
                };
                hit
            }
            _ => return false,
        };
        if action == Action::SetValue {
            let Some(ActionData::Value(value)) = data else {
                return false;
            };
            return self.accessibility_set_value(hit, value);
        }
        let controls = self.controls();
        let Some(index) = controls.iter().position(|control| *control == hit) else {
            // Drawn but not activatable — a disabled primary, or anything on
            // a finishing card. Published as `disabled`, and
            // `accesskit_windows` refuses an action on a disabled node before
            // it reaches us; this is the belt for that brace.
            return false;
        };
        match action {
            // **The press moves the focus; the release activates.** Matching
            // the pointer exactly, which matters because Windows sends `Focus`
            // immediately before `Invoke` and the pair has to behave like one
            // click rather than two unrelated commands.
            Action::Focus => {
                let before = controls.get(self.focus).copied();
                self.focus = index;
                self.keep_focused_existing_profile_visible(&controls);
                // Entering the unedited workspace name selects it, whoever
                // moved the focus there.
                self.setup_focus_entered(before);
                true
            }
            Action::Click => {
                self.focus = index;
                self.key(
                    &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                    None,
                    winit::keyboard::ModifiersState::empty(),
                )
            }
            _ => false,
        }
    }

    /// A client filling a field, through the same three post-conditions typing
    /// into it has.
    ///
    /// It is handled rather than declined because UIA offers `ValuePattern` on
    /// any node that publishes a value, whatever actions it advertised — so a
    /// client can send this whether or not we invite it, and a `SetValue` that
    /// silently does nothing is a lie the client acts on. Nothing new is
    /// possible here: the same text typed by hand reaches the same state, and a
    /// test asserts the two are byte-identical.
    fn accessibility_set_value(&mut self, hit: Hit, value: &str) -> bool {
        match (self.screen, hit) {
            // A finishing card takes nothing: a `SetValue` is routed before
            // the controls are consulted, so it is refused here.
            (Screen::Setup, Hit::Name) if !self.setup_finishing => {
                if self.owner_name.borrow().raw_text() == value {
                    return true;
                }
                self.owner_name
                    .borrow_mut()
                    .set_text(&mut self.engine, value);
                true
            }
            (Screen::Setup, Hit::WorkspaceName) if !self.setup_finishing => {
                if self.workspace_name.borrow().raw_text() == value {
                    return true;
                }
                self.workspace_name
                    .borrow_mut()
                    .set_text(&mut self.engine, value);
                self.workspace_name_edited = true;
                true
            }
            (Screen::Entry, Hit::EmailAddress) => {
                self.email_address.set_text(&mut self.engine, value);
                true
            }
            (Screen::Entry, Hit::EmailPassword) => {
                self.email_password.set_text(&mut self.engine, value);
                true
            }
            // Trimmed to six digits exactly as typing is, in the one place
            // that rule lives.
            (Screen::EmailCode, Hit::EmailCode) => {
                let digits: String = value
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .take(super::email::CODE_LEN)
                    .collect();
                self.email_code.set_text(&mut self.engine, &digits);
                true
            }
            (Screen::EmailPassword, Hit::NewPassword) => {
                self.new_password.set_text(&mut self.engine, value);
                true
            }
            (Screen::EmailPassword, Hit::ConfirmPassword) => {
                self.confirm_password.set_text(&mut self.engine, value);
                true
            }
            _ => false,
        }
    }

    // -------------------------------------------------- the email journey

    fn email_code_semantics(&mut self, cx: &Context) -> SemanticNode {
        let c = self.content();
        let row = self.email_code_row();
        let (resend, another) = self.email_code_actions();
        let ready = self.email_code_complete();
        let primary = self.button("Continue", 64.0);
        let mut children = vec![
            label(
                cx.at(parts::BLURB),
                t(cx.lang, Key::A11yFirstRunEmailCodeBlurb),
                LogicalRect::new(c.x, c.y + 156.0, c.w, 44.0),
            ),
            // One field, however many boxes it wears: a client sets six
            // digits, not a digit per box.
            field(
                cx.at(parts::EMAIL_CODE),
                t(cx.lang, Key::A11yFirstRunEmailCodeField),
                self.email_code.raw_text().to_owned(),
            )
            .with_bounds(row),
            button(cx.at(parts::PRIMARY), t(cx.lang, Key::Continue), primary).disabled(!ready),
            button(
                cx.at(parts::RESEND_CODE),
                t(cx.lang, Key::A11yFirstRunResendCode),
                resend,
            ),
            button(
                cx.at(parts::ANOTHER_ADDRESS),
                t(cx.lang, Key::A11yFirstRunAnotherAddress),
                another,
            ),
        ];
        children
            .extend(self.status_semantics(cx, LogicalRect::new(c.x, primary.y - 44.0, c.w, 40.0)));
        SemanticNode::new(cx.at(parts::FORM), Role::Group)
            .with_label(t(cx.lang, Key::A11yFirstRunEmailCodeScreen))
            .with_bounds(c)
            .with_children(children)
    }

    fn email_password_semantics(&mut self, cx: &Context) -> SemanticNode {
        let c = self.content();
        let new_rect = self.new_password_rect();
        let confirm_rect = self.confirm_password_rect();
        let ready = self.new_password_ready();
        let primary = self.button(super::email::CREATE_ACCOUNT_LABEL, 64.0);
        let mut children = vec![
            label(
                cx.at(parts::BLURB),
                t(cx.lang, Key::A11yFirstRunPasswordRule),
                LogicalRect::new(c.x, c.y + 156.0, c.w, 44.0),
            ),
            secret(
                cx.at(parts::NEW_PASSWORD),
                t(cx.lang, Key::A11yFirstRunEmailPassword),
                self.new_password.raw_text().chars().count(),
            )
            .with_bounds(new_rect),
            secret(
                cx.at(parts::CONFIRM_PASSWORD),
                t(cx.lang, Key::A11yFirstRunConfirmPassword),
                self.confirm_password.raw_text().chars().count(),
            )
            .with_bounds(confirm_rect),
            button(
                cx.at(parts::PRIMARY),
                t(cx.lang, Key::A11yFirstRunCreateAccount),
                primary,
            )
            .disabled(!ready),
        ];
        children
            .extend(self.status_semantics(cx, LogicalRect::new(c.x, primary.y - 44.0, c.w, 40.0)));
        SemanticNode::new(cx.at(parts::FORM), Role::Group)
            .with_label(t(cx.lang, Key::A11yFirstRunChoosePassword))
            .with_bounds(c)
            .with_children(children)
    }

    // ------------------------------------------------------- the welcome step

    fn entry_semantics(&mut self, cx: &Context) -> SemanticNode {
        let layout = self.welcome_layout();
        let mut children = Vec::new();
        // Absent exactly when it is not drawn: a short window gives it up to
        // hold a failure.
        if let Some(lead) = layout.lead {
            children.push(label(
                cx.at(parts::BLURB),
                t(cx.lang, Key::A11yFirstRunWelcomeBlurb),
                lead,
            ));
        }
        // A build with no Koru Cloud: the note in place of the sign-in
        // choices, then the one way in. Exactly what is drawn.
        if let Some(note) = layout.cloud_note {
            children.push(
                label(
                    cx.at(parts::NOTE),
                    t(cx.lang, Key::CloudNotConfigured),
                    note,
                )
                .with_value(t(cx.lang, Key::CloudNotConfiguredBody)),
            );
            children.push(button(
                cx.at(parts::CONTINUE_OFFLINE),
                t(cx.lang, Key::A11yFirstRunContinueWithoutAccount),
                layout.offline,
            ));
            children.push(link(
                cx.at(parts::TERMS),
                t(cx.lang, Key::A11yFirstRunTerms),
                layout.terms,
            ));
            children.push(link(
                cx.at(parts::PRIVACY),
                t(cx.lang, Key::A11yFirstRunPrivacy),
                layout.privacy,
            ));
            return SemanticNode::new(cx.at(parts::FORM), Role::Group)
                .with_label(t(cx.lang, Key::A11yFirstRunWelcome))
                .with_bounds(layout.right)
                .with_children(children);
        }
        children.extend([
            field(
                cx.at(parts::EMAIL_ADDRESS),
                t(cx.lang, Key::A11yFirstRunEmailAddress),
                self.email_address.raw_text().to_owned(),
            )
            .with_bounds(layout.address),
            secret(
                cx.at(parts::EMAIL_PASSWORD),
                t(cx.lang, Key::A11yFirstRunEmailPassword),
                self.email_password.raw_text().chars().count(),
            )
            .with_bounds(layout.password),
        ]);
        // A toggle: pressed while the password is showing.
        if let Some(eye) = self.email_password.end_icon_rect(layout.password) {
            children.push(
                button(
                    cx.at(parts::REVEAL_PASSWORD),
                    t(cx.lang, Key::A11yFirstRunShowPassword),
                    eye,
                )
                .with_toggled(self.email_password.revealed()),
            );
        }
        children.extend([
            // **Reachable with Tab even while it cannot be pressed**, unlike
            // the form steps' primary: this one sits between the two fields
            // and the other ways in, and a control that appeared under the
            // keyboard as the second field filled would move the order out
            // from under somebody mid-form. `disabled` says the true thing,
            // and a client cannot invoke it.
            button(
                cx.at(parts::CONTINUE_WITH_EMAIL),
                t(cx.lang, Key::A11yFirstRunContinueWithEmail),
                layout.email_submit,
            )
            .disabled(!self.email_form_ready()),
            button(
                cx.at(parts::GOOGLE),
                t(cx.lang, Key::A11yFirstRunSignInGoogle),
                layout.google,
            ),
            button(
                cx.at(parts::APPLE),
                t(cx.lang, Key::A11yFirstRunSignInApple),
                layout.apple,
            ),
        ]);
        // In drawing order: the failure sits under the row it is about, so
        // it is read after that row and before the no-account choice.
        // `layout.notice` is `Some` exactly when the welcome screen drew one,
        // so the tree and the pixels cannot disagree.
        if let Some(rect) = layout.notice {
            children.extend(self.status_semantics(cx, rect));
        }
        if let Some(rect) = layout.retry {
            children.push(button(cx.at(parts::RETRY), t(cx.lang, Key::TryAgain), rect));
        }
        children.extend([
            // Its own row, not the account surface's *Continue offline*: the
            // welcome screen's button says something else, and an accessible
            // name that disagrees with the label beside it is the defect this
            // whole tree exists to avoid.
            button(
                cx.at(parts::CONTINUE_OFFLINE),
                t(cx.lang, Key::A11yFirstRunContinueWithoutAccount),
                layout.offline,
            ),
        ]);
        children.push(link(
            cx.at(parts::TERMS),
            t(cx.lang, Key::A11yFirstRunTerms),
            layout.terms,
        ));
        children.push(link(
            cx.at(parts::PRIVACY),
            t(cx.lang, Key::A11yFirstRunPrivacy),
            layout.privacy,
        ));
        SemanticNode::new(cx.at(parts::FORM), Role::Group)
            .with_label(t(cx.lang, Key::A11yFirstRunWelcome))
            .with_bounds(layout.right)
            .with_children(children)
    }

    /// **Screen 1 of the workspace-modal onboarding**: the three starting
    /// paths (*Continue offline*, *Create a Koru account*, *I already have an
    /// account*) and the action row they reveal.
    fn welcome_modal_semantics(&mut self, cx: &Context) -> SemanticNode {
        let (_, frame) = self.welcome_modal_frame();
        let lang = cx.lang;
        let none = LogicalRect::new(0.0, 0.0, 0.0, 0.0);
        let rect = |key| frame.node(key).map_or(none, |node| node.rect);
        let mut children = vec![
            label(
                cx.at(parts::BLURB),
                t(lang, Key::WelcomeModalLead),
                rect(WM_LEAD),
            ),
            link(
                cx.at(parts::COMPARE_PLANS),
                t(lang, Key::WelcomeModalComparePlans),
                rect(WM_COMPARE_PLANS),
            ),
            button(
                cx.at(parts::WELCOME_OFFLINE),
                t(lang, Key::WelcomeModalOfflineTitle),
                rect(WM_OFFLINE_CARD),
            ),
            button(
                cx.at(parts::WELCOME_ACCOUNT),
                t(lang, Key::WelcomeModalAccountTitle),
                rect(WM_ACCOUNT_CARD),
            ),
        ];
        if self.welcome_path == WelcomePath::CreateAccount {
            children.push(button(
                cx.at(parts::BILLING_MONTHLY),
                t(lang, Key::WelcomeModalBillingMonthly),
                rect(WM_BILLING_MONTHLY),
            ));
            children.push(button(
                cx.at(parts::BILLING_YEARLY),
                t(lang, Key::WelcomeModalBillingYearly),
                rect(WM_BILLING_YEARLY),
            ));
        }
        children.push(button(
            cx.at(parts::WELCOME_EXISTING),
            t(lang, Key::WelcomeModalExistingTitle),
            rect(WM_EXISTING_CARD),
        ));
        if frame.node(WM_STATUS).is_some() {
            children.extend(self.status_semantics(cx, rect(WM_STATUS)));
        }
        match self.welcome_path {
            WelcomePath::Offline => {
                children.push(button(
                    cx.at(parts::CONTINUE_OFFLINE),
                    t(lang, Key::Continue),
                    rect(WM_CONTINUE),
                ));
            }
            WelcomePath::CreateAccount | WelcomePath::ExistingAccount => {
                let email_key = if self.welcome_path == WelcomePath::ExistingAccount {
                    Key::WelcomeModalEmailSignInLink
                } else {
                    Key::WelcomeModalEmailLink
                };
                children.push(button(
                    cx.at(parts::GOOGLE),
                    t(lang, Key::A11yFirstRunSignInGoogle),
                    rect(WM_GOOGLE),
                ));
                children.push(button(
                    cx.at(parts::APPLE),
                    t(lang, Key::A11yFirstRunSignInApple),
                    rect(WM_APPLE),
                ));
                children.push(button(
                    cx.at(parts::CONTINUE_WITH_EMAIL),
                    t(lang, email_key),
                    rect(WM_EMAIL_LINK),
                ));
            }
        }
        if frame.node(WM_RECOVERY).is_some() {
            children.push(button(
                cx.at(parts::RECOVERY),
                t(lang, Key::OnboardingRecoveryLink),
                rect(WM_RECOVERY),
            ));
        }
        SemanticNode::new(cx.at(parts::FORM), Role::Dialog)
            .with_label(t(lang, Key::WelcomeModalTitle))
            .with_bounds(rect(WM_CARD))
            .with_children(children)
            .modal()
    }

    fn checkout_modal_semantics(&mut self, cx: &Context) -> SemanticNode {
        let (_, frame) = self.checkout_modal_frame();
        let lang = cx.lang;
        let none = LogicalRect::new(0.0, 0.0, 0.0, 0.0);
        let rect = |key| frame.node(key).map_or(none, |node| node.rect);
        let pending = self.screen == Screen::CheckoutPending;
        let mut children = vec![label(
            cx.at(parts::BLURB),
            t(
                lang,
                if pending {
                    Key::CheckoutPendingBody
                } else {
                    Key::CheckoutIncompleteBody
                },
            ),
            rect(CM_BODY),
        )];
        if pending {
            children.push(button(
                cx.at(parts::REOPEN_CHECKOUT),
                t(lang, Key::CheckoutReopenButton),
                rect(CM_REOPEN),
            ));
            children.push(button(
                cx.at(parts::CANCEL_CHECKOUT),
                t(lang, Key::CheckoutCancelLink),
                rect(CM_CANCEL),
            ));
        } else {
            children.push(button(
                cx.at(parts::BACK),
                t(lang, Key::Back),
                rect(CM_BACK),
            ));
            children.push(button(
                cx.at(parts::BILLING_MONTHLY),
                t(lang, Key::WelcomeModalBillingMonthly),
                rect(CM_BILLING_MONTHLY),
            ));
            children.push(button(
                cx.at(parts::BILLING_YEARLY),
                t(lang, Key::WelcomeModalBillingYearly),
                rect(CM_BILLING_YEARLY),
            ));
            children.push(button(
                cx.at(parts::RETRY_CHECKOUT),
                t(lang, Key::CheckoutRetryButton),
                rect(CM_RETRY),
            ));
            children.push(button(
                cx.at(parts::CONTINUE_OFFLINE),
                t(lang, Key::CheckoutContinueOffline),
                rect(CM_OFFLINE),
            ));
        }
        SemanticNode::new(cx.at(parts::FORM), Role::Dialog)
            .with_label(t(
                lang,
                if pending {
                    Key::CheckoutPendingTitle
                } else {
                    Key::CheckoutIncompleteTitle
                },
            ))
            .with_bounds(rect(CM_CARD))
            .with_children(children)
            .modal()
    }

    // ------------------------------------------------------- the setup card

    /// **The setup card** (welcome step 1, PLAN §9), in reading order: the
    /// form named by the title, the lead, the *You* group with the name field,
    /// the *First workspace* group with the tile and the workspace's name, the
    /// swatch row and the emoji row as a `ListBox` of ten options each, the
    /// note and the status when drawn, the primary, then *Back*, *Resume* and
    /// recovery when offered.
    ///
    /// **Every rectangle is the card's frame's**, and a cell's is
    /// `ColorPickerRow`'s wrapped geometry inside the row's leaf — the same
    /// two answers paint and the pointer read.
    ///
    /// **The primary is published even when it cannot be pressed.** That is
    /// what the pixels do: it is drawn inert while the card is incomplete, and
    /// `setup_controls` leaves it out so neither Tab nor a pointer reaches it.
    /// A tree that omitted it would tell a screen reader the button is absent
    /// while a sighted person is looking straight at it. `disabled` says the
    /// true thing, and `accesskit_windows` will not deliver an action to a
    /// disabled node.
    ///
    /// **`live` on the primary while the card is finishing.** The button's
    /// own label becomes *Finishing…*, so that is where the news is;
    /// `accesskit_windows` raises `UIA_LiveRegionChangedEventId` when a live
    /// node with a name is added, which entering finishing does (it mints a
    /// new instance) — see `guide/contributing/deps/ACCESSKIT.md`.
    ///
    /// The chip is decorative and publishes nothing.
    fn setup_semantics(&mut self, cx: &Context) -> SemanticNode {
        let (_, frame) = self.setup_frame();
        let lang = cx.lang;
        let finishing = self.setup_finishing;
        let none = LogicalRect::new(0.0, 0.0, 0.0, 0.0);
        let rect = |key| frame.node(key).map_or(none, |node| node.rect);
        let (label_key, placeholder_key) = self.setup_owner_keys();
        let mut children = Vec::new();
        // Embedded in Manage the card has no lead (Q-C), so none is
        // published; the group keeps the title as its name either way.
        if frame.node(KEY_LEAD).is_some() {
            children.push(label(
                cx.at(parts::BLURB),
                t(lang, self.setup_lead_key()),
                rect(KEY_LEAD),
            ));
        }
        children.push(
            SemanticNode::new(cx.at(parts::YOU), Role::Group)
                .with_label(t(lang, label_key))
                .with_bounds(rect(KEY_YOU_ROW))
                .with_children(vec![
                    button(
                        cx.at(parts::PROFILE_ICON),
                        t(lang, Key::A11yFirstRunProfileIcon),
                        rect(KEY_AVATAR),
                    )
                    .disabled(finishing),
                    field(
                        cx.at(parts::OWNER_NAME),
                        t(lang, placeholder_key),
                        self.owner_name.borrow().raw_text().to_owned(),
                    )
                    .with_bounds(rect(KEY_NAME))
                    .disabled(finishing),
                ]),
        );
        children.push(
            SemanticNode::new(cx.at(parts::FIRST_WORKSPACE), Role::Group)
                .with_label(t(lang, Key::SetupWorkspaceLabel))
                .with_bounds(rect(KEY_WORKSPACE_ROW))
                .with_children(vec![
                    button(
                        cx.at(parts::WORKSPACE_ICON),
                        t(lang, Key::A11yFirstRunWorkspaceIcon),
                        rect(KEY_TILE),
                    )
                    .disabled(finishing),
                    field(
                        cx.at(parts::WORKSPACE_NAME),
                        t(lang, Key::WorkspaceName),
                        self.workspace_name.borrow().raw_text().to_owned(),
                    )
                    .with_bounds(rect(KEY_WORKSPACE_NAME))
                    .disabled(finishing),
                ]),
        );
        if frame.node(KEY_NOTE).is_some() {
            children.push(label(
                cx.at(parts::NOTE),
                t(lang, Key::SetupConnectedNote),
                rect(KEY_NOTE),
            ));
        }
        if frame.node(KEY_STATUS).is_some() {
            children.extend(self.status_semantics(cx, rect(KEY_STATUS)));
        }
        let mut primary = button(
            cx.at(parts::PRIMARY),
            self.setup_primary_label(),
            rect(KEY_PRIMARY),
        )
        .disabled(!self.setup_primary_enabled());
        if finishing {
            primary = primary.with_live(Live::Polite);
        }
        children.push(primary);
        // *Back*, *Resume* and recovery are in the frame exactly when they
        // are offered and the card is not finishing.
        if frame.node(KEY_BACK).is_some() {
            let back_label = if self.setup_has_change() {
                t(lang, Key::SetupChange)
            } else {
                t(lang, Key::Back)
            };
            children.push(button(cx.at(parts::BACK), back_label, rect(KEY_BACK)));
        }
        if frame.node(KEY_RESUME).is_some() {
            children.push(button(
                cx.at(parts::RESUME_ACCOUNT),
                t(lang, Key::AccountCreationResume),
                rect(KEY_RESUME),
            ));
        }
        // Drawn as a wrapping link, but a `Button`: it opens a screen inside
        // Koru, and `Role::Link` is kept for what leaves Koru for a web page.
        if frame.node(KEY_RECOVERY).is_some() {
            children.push(button(
                cx.at(parts::RECOVERY),
                t(lang, Key::OnboardingRecoveryLink),
                rect(KEY_RECOVERY),
            ));
        }
        let role = if self.workspace_modal {
            Role::Dialog
        } else {
            Role::Group
        };
        let node = SemanticNode::new(cx.at(parts::FORM), role)
            .with_label(t(lang, Key::SetupTitle))
            .with_bounds(rect(KEY_CARD))
            .with_children(children);
        if self.workspace_modal {
            node.modal()
        } else {
            node
        }
    }

    // ---------------------------------------------------- the recovery screen

    /// **The recovery screen** (root-catalog §6.3): a heading, the
    /// restorable Profiles as a list whose options are the Profiles
    /// themselves — each with its *Restore* as a child button — the
    /// unassigned workspaces as a second list, the one line under them, and
    /// *Back*.
    fn recovery_semantics(&mut self, cx: &Context) -> SemanticNode {
        let layout = self.recovery_layout();
        let lang = cx.lang;
        let mut children = vec![label(
            cx.at(parts::STEP),
            t(lang, Key::OnboardingRecoveryTitle),
            layout.title,
        )];
        let deleted: Vec<SemanticNode> = self
            .recovery_deleted()
            .iter()
            .zip(&layout.deleted)
            .enumerate()
            .map(|(index, (row, (rect, button_rect)))| {
                let line = self.recovery_deleted_line(row);
                SemanticNode::new(
                    Occurrence::model(cx.window, cx.view, row.profile.as_uuid(), parts::FORM),
                    Role::ListBoxOption,
                )
                .with_label(format!("{}, {line}", row.name))
                .with_bounds(*rect)
                .with_position(index)
                .with_children(vec![button(
                    Occurrence::model(cx.window, cx.view, row.profile.as_uuid(), parts::RESTORE),
                    t(lang, Key::WorkspacesRestoreDeleted),
                    *button_rect,
                )])
            })
            .collect();
        let deleted_bounds = if deleted.is_empty() {
            layout.deleted_heading
        } else {
            band(&deleted)
        };
        children.push(
            SemanticNode::new(cx.at(parts::DELETED_PROFILES), Role::ListBox)
                .with_label(t(lang, Key::OnboardingRestoreDeleted))
                .with_bounds(deleted_bounds)
                .with_size_of_set(deleted.len())
                .with_children(deleted),
        );
        if let Some(none) = layout.none {
            children.push(label(
                cx.at(parts::NOTE),
                t(lang, Key::OnboardingNoDeletedProfiles),
                none,
            ));
        }
        if let Some(heading) = layout.unassigned_heading {
            let rows: Vec<SemanticNode> = self
                .recovery_unassigned()
                .iter()
                .zip(&layout.unassigned)
                .enumerate()
                .map(|(index, (row, rect))| {
                    SemanticNode::new(
                        Occurrence::model(cx.window, cx.view, row.workspace.as_uuid(), parts::FORM),
                        Role::ListBoxOption,
                    )
                    .with_label(row.name.clone())
                    .with_bounds(*rect)
                    .with_position(index)
                })
                .collect();
            let bounds = if rows.is_empty() {
                heading
            } else {
                band(&rows)
            };
            children.push(
                SemanticNode::new(cx.at(parts::UNASSIGNED), Role::ListBox)
                    .with_label(t(lang, Key::OnboardingUnassignedWorkspaces))
                    .with_bounds(bounds)
                    .with_size_of_set(rows.len())
                    .with_children(rows),
            );
            if let Some(note) = layout.note {
                children.push(label(
                    cx.at(parts::BLURB),
                    t(lang, Key::OnboardingUnassignedNeedsProfile),
                    note,
                ));
            }
        }
        if let (Some(rect), Some(message)) = (layout.error, self.error.as_deref()) {
            children.push(label(cx.at(parts::STATUS), message, rect).with_live(Live::Polite));
        }
        children.push(button(cx.at(parts::BACK), "Back", layout.back));
        let role = if self.workspace_modal {
            Role::Dialog
        } else {
            Role::Group
        };
        let node = SemanticNode::new(cx.at(parts::FORM), role)
            .with_label(t(lang, Key::OnboardingRecoveryTitle))
            .with_bounds(self.content())
            .with_children(children);
        if self.workspace_modal {
            node.modal()
        } else {
            node
        }
    }

    /// The error the form is showing, if it is showing one.
    ///
    /// **Only where it is drawn**, which is why every caller hands it the
    /// rectangle its own screen paints the message in. The welcome screen was
    /// the one exception — `account_failed` set a message and `draw_entry`
    /// painted nothing — and publishing it then would have put words in a
    /// screen reader that no sighted person was being shown. Q-U1 settled
    /// that by drawing it, so the welcome screen now has a rectangle to be
    /// published at, exactly like the setup card.
    fn status_semantics(&self, cx: &Context, at: LogicalRect) -> Option<SemanticNode> {
        let error = self.error.as_deref()?;
        Some(
            label(cx.at(parts::STATUS), error, at)
                // Assertive: something has just failed and the person is
                // waiting on it. The announcement follows the **label**, which
                // is why the message is the label rather than the value.
                .with_live(Live::Assertive),
        )
    }
}

/// Everything a control needs from the window it is drawn in.
#[derive(Copy, Clone, Debug)]
struct Context {
    window: WindowSlot,
    view: ViewSlot,
    /// The screen instance every control on this screen belongs to.
    instance: u64,
    lang: Lang,
}

impl Context {
    fn at(&self, part: ControlPart) -> Occurrence {
        Occurrence::instance(self.window, self.view, FORM, self.instance, part)
    }
}

/// A sentence with no control behind it.
fn label(occurrence: Occurrence, text: &str, bounds: LogicalRect) -> SemanticNode {
    SemanticNode::new(occurrence, Role::Label)
        .with_label(text)
        .with_bounds(bounds)
}

fn button(occurrence: Occurrence, text: &str, bounds: LogicalRect) -> SemanticNode {
    SemanticNode::new(occurrence, Role::Button)
        .with_label(text)
        .with_bounds(bounds)
        .with_actions([Action::Click, Action::Focus])
}

/// A control that leaves Koru for a web page. `Role::Link` rather than
/// `Role::Button` so a client can say what will happen.
fn link(occurrence: Occurrence, text: &str, bounds: LogicalRect) -> SemanticNode {
    SemanticNode::new(occurrence, Role::Link)
        .with_label(text)
        .with_bounds(bounds)
        .with_actions([Action::Click, Action::Focus])
}

/// A text field: named by the label drawn beside it, carrying what is in it.
///
/// **`raw_text`, not `value`.** `TextInput::value` is *authored and trimmed* —
/// what the form will act on — and `raw_text` is what is on the screen,
/// including a dictation still in progress. A screen reader reads the field,
/// so it gets the second: telling somebody their leading spaces are not there
/// while the caret is sitting after them is the kind of small lie that costs an
/// hour. The trimmed value is still what decides whether the primary is
/// enabled, because that is the form's own rule.
///
/// **No `Action::Click`.** `accesskit_consumer` excludes a text input from
/// `is_invocable` whatever it advertises, and pressing Enter in a field is not
/// what a client means by invoking one. `SetValue` is the action a client has
/// for a field, and it is routed.
fn field(occurrence: Occurrence, name: &str, value: String) -> SemanticNode {
    SemanticNode::new(occurrence, Role::TextInput)
        .with_label(name)
        .with_value(value)
        .with_actions([Action::Focus, Action::SetValue])
}

/// A password box: named, and carrying how long it is and nothing else.
///
/// **The characters never enter the tree.** An accessibility tree is handed to
/// the platform's bus and read by whatever is attached to it, so a field whose
/// pixels are bullets publishes bullets too. The count is what makes the node
/// change as somebody types, which is what a screen reader needs to hear that
/// the keystroke arrived.
fn secret(occurrence: Occurrence, name: &str, length: usize) -> SemanticNode {
    SemanticNode::new(occurrence, Role::TextInput)
        .with_label(name)
        .with_value("\u{2022}".repeat(length))
        .with_actions([Action::Focus, Action::SetValue])
}

/// The span of a set of cards, so the container they sit in has a rectangle
/// too. A node with no bounds has a zero-size one — in the tree, and
/// unreachable with a pointer or with spatial navigation.
fn band(nodes: &[SemanticNode]) -> LogicalRect {
    nodes
        .iter()
        .filter_map(|node| node.bounds)
        .fold(None::<LogicalRect>, |acc, rect| {
            Some(match acc {
                None => rect,
                Some(acc) => {
                    let x = acc.x.min(rect.x);
                    let y = acc.y.min(rect.y);
                    let right = (acc.x + acc.w).max(rect.x + rect.w);
                    let bottom = (acc.y + acc.h).max(rect.y + rect.h);
                    LogicalRect::new(x, y, right - x, bottom - y)
                }
            })
        })
        .unwrap_or(LogicalRect::new(0.0, 0.0, 0.0, 0.0))
}

#[cfg(test)]
mod tests;
