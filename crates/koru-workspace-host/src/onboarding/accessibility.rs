// SPDX-License-Identifier: MPL-2.0

//! **What the first-run window offers a screen reader, and what a request to
//! one of its controls means.**
//!
//! One sentence, and what it excludes: *this module turns the first-run
//! controller's current screen into semantic nodes, and turns a request back
//! into the same call the keyboard makes.* It excludes what a control **is** —
//! a screen, a field, a purpose card, a template card are
//! [`super::OnboardingView`]'s and stay there — and it excludes the window: who
//! installs a provider, when a tree is worth building and which requests are
//! still about something that exists belong to
//! [`crate::accessibility`](crate::accessibility) and to
//! `koru_window::accessibility`. It is a child module rather than a section of
//! `onboarding.rs` for the reason §7.10 of the roadmap gives: that file is
//! 3,400 lines and gets a call, not another embedded subsystem.
//!
//! ## The route this projects, and the one it does not
//!
//! Plan U0.1 is the **account-free** route: *Welcome → Profile → Workspace →
//! finishing → the result*. Those five states are projected in full. The
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
//! are states of one window, not separate windows, and two of them have a
//! control in the same place doing different things — the Profile step's
//! *Continue* and the Workspace step's *Create workspace* are both
//! [`Hit::Primary`] at the same rectangle. If both carried one locator, a
//! request queued against the screen that just went would land on the control
//! that replaced it. So the object every control belongs to is
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
//! to publish with the screen it published last, and mints on a difference.
//! That is not merely easier — it is *exactly* the right condition, because an
//! identity only has to differ between two trees a client actually saw. A
//! screen the client was never shown cannot have a request queued against it.
//!
//! **Geometry is never re-derived.** Every rectangle published here comes back
//! from the function drawing and hit-testing already share — `welcome_layout`,
//! `content`, `owner_name_rect`, `profile_name_rect`, `profile_icon_rect`,
//! `workspace_name_rect`, `workspace_icon_rect`, `card_rect`,
//! `form_action_buttons`, `koru_ui::caption::direct_controls`. A second copy
//! of the arithmetic is a control a client can find somewhere other than where
//! it answers a press.
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
//! structural rather than careful: pressing *Create workspace* moves the form
//! to [`Screen::Finishing`], whose `controls()` is empty and whose `hit()` is
//! `None`, so the primary is neither published as activatable nor reachable
//! from a client; and a request queued against the Workspace screen's primary
//! names an instance that is no longer on screen, so it is refused. When
//! creation fails, `creation_failed` returns to the Workspace step with every
//! typed value intact and a fresh instance, and the one retry is the person's.

use accesskit::{Action, ActionData, Live, Role};
use koru_i18n::{Key, Lang, t};

use crate::accessibility::{
    CaptionGeometry, ControlPart, LogicalRect, ObjectKey, Occurrence, Revision, SemanticNode,
    ViewSlot, WindowSlot,
};

use super::{Hit, OnboardingView, PURPOSES, Purpose, Screen, TEMPLATES, template_is_suggested};

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

    /// The Profile step.
    pub const RESUME_ACCOUNT: ControlPart = ControlPart::new("resume-account");
    pub const OWNER_NAME: ControlPart = ControlPart::new("owner-name");
    pub const PURPOSES: ControlPart = ControlPart::new("purposes");
    pub const PURPOSE_WORK: ControlPart = ControlPart::new("purpose-work");
    pub const PURPOSE_SCHOOL: ControlPart = ControlPart::new("purpose-school");
    pub const PURPOSE_PERSONAL: ControlPart = ControlPart::new("purpose-personal");
    pub const PROFILE_ICON: ControlPart = ControlPart::new("profile-icon");
    pub const PROFILE_NAME: ControlPart = ControlPart::new("profile-name");

    /// The Workspace step.
    pub const WORKSPACE_NAME: ControlPart = ControlPart::new("workspace-name");
    pub const WORKSPACE_ICON: ControlPart = ControlPart::new("workspace-icon");
    pub const TEMPLATES: ControlPart = ControlPart::new("templates");
    /// One per starting point, in `TEMPLATES` order. A keyword rather than the
    /// index alone so a locator reads `#template-project`, which survives a
    /// reordering of the array — the template's own stored `key`, which a
    /// created workspace already carries.
    pub const TEMPLATE: [ControlPart; 6] = [
        ControlPart::new("template-project"),
        ControlPart::new("template-research"),
        ControlPart::new("template-course"),
        ControlPart::new("template-recipes"),
        ControlPart::new("template-home"),
        ControlPart::new("template-empty"),
    ];

    /// The action row both steps share.
    pub const PRIMARY: ControlPart = ControlPart::new("primary");
    pub const BACK: ControlPart = ControlPart::new("back");

    /// The icon-and-colour chooser, and the Google route's screens.
    pub const APPEARANCE: ControlPart = ControlPart::new("appearance");
    pub const ACCOUNT_ROUTE: ControlPart = ControlPart::new("account-route");
}

/// **The part a control answers to on a given screen**, and the one place the
/// mapping is written.
///
/// Screen-scoped because the same [`Hit`] means different things on different
/// screens: `Hit::Primary` is *Continue offline* on the welcome screen and
/// *Create workspace* on the Workspace step, and `Hit::Name` is the owner's
/// name on one step and the workspace's on the next. Its inverse is
/// [`hit_of`], and a test walks every control of every screen through both.
fn part_of(screen: Screen, hit: Hit) -> Option<ControlPart> {
    use parts as p;
    Some(match (screen, hit) {
        (Screen::Entry, Hit::EmailAddress) => p::EMAIL_ADDRESS,
        (Screen::Entry, Hit::EmailPassword) => p::EMAIL_PASSWORD,
        (Screen::Entry, Hit::ContinueWithEmail) => p::CONTINUE_WITH_EMAIL,
        (Screen::Entry, Hit::Secondary) => p::GOOGLE,
        (Screen::Entry, Hit::Apple) => p::APPLE,
        (Screen::Entry, Hit::Primary) => p::CONTINUE_OFFLINE,
        (Screen::Entry, Hit::Retry) => p::RETRY,
        (Screen::Entry, Hit::RevealPassword) => p::REVEAL_PASSWORD,
        (Screen::Entry, Hit::Terms) => p::TERMS,
        (Screen::Entry, Hit::Privacy) => p::PRIVACY,
        (Screen::Profile, Hit::ResumeAccount) => p::RESUME_ACCOUNT,
        (Screen::Profile, Hit::Name) => p::OWNER_NAME,
        (Screen::Profile, Hit::Purpose(Purpose::Work)) => p::PURPOSE_WORK,
        (Screen::Profile, Hit::Purpose(Purpose::School)) => p::PURPOSE_SCHOOL,
        (Screen::Profile, Hit::Purpose(Purpose::Personal)) => p::PURPOSE_PERSONAL,
        (Screen::Profile, Hit::ProfileAppearance) => p::PROFILE_ICON,
        (Screen::Profile, Hit::ProfileName) => p::PROFILE_NAME,
        (Screen::Workspace | Screen::Finishing, Hit::Name) => p::WORKSPACE_NAME,
        (Screen::Workspace | Screen::Finishing, Hit::WorkspaceAppearance) => p::WORKSPACE_ICON,
        (Screen::Workspace | Screen::Finishing, Hit::Template(index)) => *p::TEMPLATE.get(index)?,
        (Screen::EmailCode, Hit::EmailCode) => p::EMAIL_CODE,
        (Screen::EmailCode, Hit::ResendCode) => p::RESEND_CODE,
        (Screen::EmailCode, Hit::AnotherAddress) => p::ANOTHER_ADDRESS,
        (Screen::EmailPassword, Hit::NewPassword) => p::NEW_PASSWORD,
        (Screen::EmailPassword, Hit::ConfirmPassword) => p::CONFIRM_PASSWORD,
        (
            Screen::Profile
            | Screen::Workspace
            | Screen::Finishing
            | Screen::EmailCode
            | Screen::EmailPassword,
            Hit::Primary,
        ) => p::PRIMARY,
        (Screen::Profile | Screen::Workspace | Screen::Finishing, Hit::Back) => p::BACK,
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
            x if x == p::RETRY => Hit::Retry,
            x if x == p::REVEAL_PASSWORD => Hit::RevealPassword,
            x if x == p::TERMS => Hit::Terms,
            x if x == p::PRIVACY => Hit::Privacy,
            _ => return None,
        },
        Screen::Profile => match part {
            x if x == p::RESUME_ACCOUNT => Hit::ResumeAccount,
            x if x == p::OWNER_NAME => Hit::Name,
            x if x == p::PURPOSE_WORK => Hit::Purpose(Purpose::Work),
            x if x == p::PURPOSE_SCHOOL => Hit::Purpose(Purpose::School),
            x if x == p::PURPOSE_PERSONAL => Hit::Purpose(Purpose::Personal),
            x if x == p::PROFILE_ICON => Hit::ProfileAppearance,
            x if x == p::PROFILE_NAME => Hit::ProfileName,
            x if x == p::PRIMARY => Hit::Primary,
            x if x == p::BACK => Hit::Back,
            _ => return None,
        },
        Screen::Workspace | Screen::Finishing => match part {
            x if x == p::WORKSPACE_NAME => Hit::Name,
            x if x == p::WORKSPACE_ICON => Hit::WorkspaceAppearance,
            x if x == p::PRIMARY => Hit::Primary,
            x if x == p::BACK => Hit::Back,
            x => Hit::Template(p::TEMPLATE.iter().position(|part| *part == x)?),
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
            Screen::ExistingProfiles => "existing",
            Screen::RestoringProfile => "restoring",
            Screen::Profile => "profile",
            Screen::Workspace => "workspace",
            Screen::Finishing => "finishing",
        });
        rev.str(self.language.tag());
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
        rev.str(self.owner_name.raw_text());
        rev.str(self.profile_name.raw_text());
        rev.str(self.workspace_name.raw_text());
        rev.byte(match self.purpose {
            None => 0,
            Some(Purpose::Work) => 1,
            Some(Purpose::School) => 2,
            Some(Purpose::Personal) => 3,
        });
        rev.u64(self.template.map_or(u64::MAX, |index| index as u64));
        rev.bool(self.workspace_name_edited);
        rev.bool(self.connect_account);
        rev.bool(self.can_reopen_account_creation());
        rev.bool(self.profile_appearance.is_some() || self.workspace_appearance.is_some());
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
        if self.screen_shown != Some(self.screen) {
            self.screen_shown = Some(self.screen);
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
        if self.profile_appearance.is_some() || self.workspace_appearance.is_some() {
            return vec![
                SemanticNode::new(cx.at(parts::APPEARANCE), Role::Dialog)
                    .with_label(t(cx.lang, Key::A11yFirstRunAppearance))
                    .with_bounds(self.appearance_area())
                    .modal(),
            ];
        }
        match self.screen {
            Screen::Entry => vec![self.entry_semantics(&cx)],
            Screen::EmailCode => vec![self.email_code_semantics(&cx)],
            Screen::EmailPassword => vec![self.email_password_semantics(&cx)],
            Screen::Profile => vec![self.profile_semantics(&cx)],
            Screen::Workspace | Screen::Finishing => vec![self.workspace_semantics(&cx)],
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
    pub fn accessibility_focus(&self, window: WindowSlot, view: ViewSlot) -> Option<Occurrence> {
        if self.profile_appearance.is_some() || self.workspace_appearance.is_some() {
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
        if self.screen_shown != Some(self.screen) {
            return false;
        }
        // Nothing under the icon-and-colour chooser is published while it is
        // up, so nothing under it is routed either. `PaneAccessibility` would
        // refuse it anyway — the control is not in the tree on screen — and
        // giving the same answer locally keeps this module from reading as if
        // it offered a route it does not have.
        if self.profile_appearance.is_some() || self.workspace_appearance.is_some() {
            return false;
        }
        let ObjectKey::Instance { kind, id } = target.object else {
            return false;
        };
        if kind != FORM || id != self.screen_instance {
            return false;
        }
        let Some(hit) = hit_of(self.screen, target.part) else {
            return false;
        };
        if action == Action::SetValue {
            let Some(ActionData::Value(value)) = data else {
                return false;
            };
            return self.accessibility_set_value(hit, value);
        }
        let controls = self.controls();
        let Some(index) = controls.iter().position(|control| *control == hit) else {
            // Drawn but not activatable — a disabled primary, or a template
            // card locked by a typed workspace name. Published as `disabled`,
            // and `accesskit_windows` refuses an action on a disabled node
            // before it reaches us; this is the belt for that brace.
            return false;
        };
        match action {
            // **The press moves the focus; the release activates.** Matching
            // the pointer exactly, which matters because Windows sends `Focus`
            // immediately before `Invoke` and the pair has to behave like one
            // click rather than two unrelated commands.
            Action::Focus => {
                self.focus = index;
                self.keep_focused_existing_profile_visible(&controls);
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
            (Screen::Profile, Hit::Name) => {
                if self.owner_name.raw_text() == value {
                    return true;
                }
                self.owner_name.set_text(&mut self.engine, value);
                self.update_profile_name_suggestion();
                true
            }
            (Screen::Profile, Hit::ProfileName) => {
                if self.profile_name.raw_text() == value {
                    return true;
                }
                self.profile_name.set_text(&mut self.engine, value);
                self.profile_name_edited = true;
                true
            }
            (Screen::Workspace, Hit::Name) => {
                if self.workspace_name.raw_text() == value {
                    return true;
                }
                self.workspace_name.set_text(&mut self.engine, value);
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

    // ------------------------------------------------------- the Profile step

    fn profile_semantics(&mut self, cx: &Context) -> SemanticNode {
        let c = self.content();
        let mut children = Vec::new();
        // Either the step counter or, when an account creation is parked, the
        // button that returns to it. Exactly what is drawn in that corner.
        if self.can_reopen_account_creation() {
            let rect = self.resume_account_rect();
            children.push(button(
                cx.at(parts::RESUME_ACCOUNT),
                t(cx.lang, Key::AccountCreationResume),
                rect,
            ));
        } else {
            children.push(label(
                cx.at(parts::STEP),
                t(cx.lang, Key::Step1Of2),
                LogicalRect::new(c.x, c.y + 24.0, c.w, 18.0),
            ));
        }
        children.push(label(
            cx.at(parts::BLURB),
            t(cx.lang, Key::A11yFirstRunOwnerBlurb),
            LogicalRect::new(c.x, c.y + 108.0, c.w, 22.0),
        ));
        children.push(
            field(
                cx.at(parts::OWNER_NAME),
                t(cx.lang, Key::DisplayName),
                self.owner_name.raw_text().to_owned(),
            )
            .with_bounds(self.owner_name_rect()),
        );

        let metrics = self.profile_card_metrics();
        let cards: Vec<SemanticNode> = PURPOSES
            .into_iter()
            .enumerate()
            .map(|(index, purpose)| {
                choice(
                    cx.at(part_of(Screen::Profile, Hit::Purpose(purpose)).expect("a purpose card")),
                    t(cx.lang, purpose_key(purpose)),
                    self.card_rect(index, metrics),
                    self.purpose == Some(purpose),
                    index,
                )
            })
            .collect();
        children.push(
            SemanticNode::new(cx.at(parts::PURPOSES), Role::ListBox)
                .with_label(t(cx.lang, Key::PurposeQuestion))
                .with_bounds(band(&cards))
                // On the container, because that is where an item's total is
                // read from — see `SemanticNode::size_of_set`.
                .with_size_of_set(cards.len())
                .with_children(cards),
        );

        let icon = self.profile_icon_rect();
        children.push(button(
            cx.at(parts::PROFILE_ICON),
            t(cx.lang, Key::A11yFirstRunProfileIcon),
            icon,
        ));
        children.push(
            field(
                cx.at(parts::PROFILE_NAME),
                t(cx.lang, Key::A11yFirstRunProfileName),
                self.profile_name.raw_text().to_owned(),
            )
            .with_bounds(self.profile_name_rect()),
        );
        let footer_y = icon.y + icon.h + 16.0 + super::profile_note_h(self.connect_account);
        children.extend(self.status_semantics(cx, LogicalRect::new(c.x, footer_y, c.w, 20.0)));

        let complete = self.profile_form_complete();
        // "Continue" is the literal `draw_profile` paints and measures; the
        // name beside it is the translated row.
        children.extend(self.action_row_semantics(
            cx,
            "Continue",
            t(cx.lang, Key::Continue),
            complete,
        ));

        SemanticNode::new(cx.at(parts::FORM), Role::Group)
            .with_label(t(cx.lang, Key::IdentityTitle))
            .with_bounds(c)
            .with_children(children)
    }

    // ----------------------------------------------------- the Workspace step

    fn workspace_semantics(&mut self, cx: &Context) -> SemanticNode {
        let c = self.content();
        let finishing = self.screen == Screen::Finishing;
        let locked = self.workspace_name_edited;
        let mut children = vec![
            label(
                cx.at(parts::STEP),
                t(cx.lang, Key::Step2Of2),
                LogicalRect::new(c.x, c.y + 18.0, c.w, 18.0),
            ),
            label(
                cx.at(parts::BLURB),
                t(cx.lang, Key::A11yFirstRunWorkspaceBlurb),
                LogicalRect::new(c.x, c.y + 86.0, c.w, 22.0),
            ),
        ];

        let metrics = self.workspace_card_metrics();
        let detailed = metrics.1 >= 100.0;
        let purpose = self.purpose;
        let selected = self.template;
        let cards: Vec<SemanticNode> = (0..TEMPLATES.len())
            .map(|index| {
                let rect = self.card_rect(index, metrics);
                let mut card = choice(
                    cx.at(parts::TEMPLATE[index]),
                    t(cx.lang, template_key(index)),
                    rect,
                    selected == Some(index),
                    index,
                )
                // **Locked, not gone.** A typed workspace name freezes the
                // starting points: they are still drawn, dimmed, and
                // `form_controls` stops offering them. `disabled` is what says
                // that, and `accesskit_windows` refuses an action on a disabled
                // node, so the projection and the form agree without a second
                // guard.
                .disabled(locked);
                if detailed {
                    let mut inner = vec![label(
                        cx.at(ControlPart::new(DETAIL[index])),
                        t(cx.lang, template_detail_key(index)),
                        LogicalRect::new(rect.x + 12.0, rect.y + 69.0, rect.w - 24.0, 32.0),
                    )];
                    if template_is_suggested(purpose, index) {
                        inner.push(label(
                            cx.at(ControlPart::new(SUGGESTED[index])),
                            t(cx.lang, Key::Suggested),
                            LogicalRect::new(rect.x + rect.w - 80.0, rect.y + 9.0, 74.0, 20.0),
                        ));
                    }
                    card = card.with_children(inner);
                }
                card
            })
            .collect();
        children.push(
            SemanticNode::new(cx.at(parts::TEMPLATES), Role::ListBox)
                .with_label(t(cx.lang, Key::A11yFirstRunStartingPoint))
                .with_bounds(band(&cards))
                .with_size_of_set(cards.len())
                .with_children(cards),
        );

        children.push(button(
            cx.at(parts::WORKSPACE_ICON),
            t(cx.lang, Key::A11yFirstRunWorkspaceIcon),
            self.workspace_icon_rect(),
        ));
        children.push(
            field(
                cx.at(parts::WORKSPACE_NAME),
                t(cx.lang, Key::WorkspaceName),
                self.workspace_name.raw_text().to_owned(),
            )
            .with_bounds(self.workspace_name_rect()),
        );
        children
            .extend(self.status_semantics(cx, LogicalRect::new(c.x, c.y + c.h - 150.0, c.w, 20.0)));

        let complete = self.workspace_form_complete();
        // The pair `draw_workspace` uses to paint and the pair a screen reader
        // hears. Same control, same rectangle, two languages until the drawn
        // copy catches up.
        let (drawn, name) = if finishing {
            ("Finishing...", t(cx.lang, Key::A11yFirstRunCreating))
        } else if self.creation_conflict {
            (
                self.workspace_action_label(),
                t(cx.lang, Key::ProfileCreationManage),
            )
        } else {
            ("Create workspace", t(cx.lang, Key::CreateWorkspace))
        };
        children.extend(self.action_row_semantics(cx, drawn, name, complete && !finishing));

        SemanticNode::new(cx.at(parts::FORM), Role::Group)
            .with_label(t(cx.lang, Key::WorkspaceTitle))
            .with_bounds(c)
            .with_children(children)
    }

    // ----------------------------------------------------------- both steps

    /// The primary action and the way back, from the same two rectangles the
    /// two steps draw them at.
    ///
    /// **The primary is published even when it cannot be pressed.** That is
    /// what the pixels do: `draw_button_enabled` paints it greyed while the
    /// form is incomplete, and `form_controls` leaves it out so neither Tab nor
    /// a pointer reaches it. A tree that omitted it would tell a screen reader
    /// the button is absent while a sighted person is looking straight at it,
    /// and would leave no way to hear *why* nothing is happening. `disabled`
    /// says the true thing, and `accesskit_windows` will not deliver an action
    /// to a disabled node.
    ///
    /// **`live` on the primary while a creation is in flight.** There is no
    /// other status text on this screen: the button's own label becomes
    /// *Finishing…*, so that is where the news is. `accesskit_windows` raises
    /// `UIA_LiveRegionChangedEventId` when a live node with a name is added,
    /// which is what a transition to `Finishing` does — see
    /// `guide/contributing/deps/ACCESSKIT.md`.
    ///
    /// **`drawn` and `name` are two arguments on purpose.** The button sizes
    /// itself to the string it paints, and that string is an English literal
    /// (`docs/contributing/techdebt/README.md`) while the accessible name is translated. If
    /// the rectangle were measured from the translated one it would be a
    /// different width from the button on screen, and a client would find the
    /// control somewhere it does not answer a press. So the geometry comes from
    /// what is drawn and the name from `koru-i18n`, and the day the drawn copy
    /// is localized the two arguments collapse into one.
    fn action_row_semantics(
        &mut self,
        cx: &Context,
        drawn: &str,
        name: &str,
        enabled: bool,
    ) -> Vec<SemanticNode> {
        let finishing = self.screen == Screen::Finishing;
        let (primary_rect, back_rect) = self.form_action_buttons(drawn);
        let mut primary = button(cx.at(parts::PRIMARY), name, primary_rect).disabled(!enabled);
        if finishing {
            primary = primary.with_live(Live::Polite);
        }
        let mut out = vec![primary];
        // The Workspace step stops drawing Back once creation has started.
        // `form_action_buttons` already measures the ghost button from its own
        // drawn literal, so only the primary needs the pair above.
        if !finishing {
            out.push(button(cx.at(parts::BACK), t(cx.lang, Key::Back), back_rect));
        }
        out
    }

    /// The error the form is showing, if it is showing one.
    ///
    /// **Only where it is drawn**, which is why every caller hands it the
    /// rectangle its own screen paints the message in. The welcome screen was
    /// the one exception — `account_failed` set a message and `draw_entry`
    /// painted nothing — and publishing it then would have put words in a
    /// screen reader that no sighted person was being shown. Q-U1 settled
    /// that by drawing it, so the welcome screen now has a rectangle to be
    /// published at, exactly like the two form steps.
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

/// One card in a group where choosing *is* the action.
///
/// `selected` on every card, true and false alike: these are what selection is
/// for, "not selected" is worth announcing about one, and a client reaches it
/// through `SelectionItem`, which `accesskit_windows` turns straight back into
/// the `Click` below. It costs the card its Invoke pattern — a node carrying
/// `selected` is excluded from `is_invocable` — which is exactly the trade
/// ADR-338 took for the navigator's projection buttons.
fn choice(
    occurrence: Occurrence,
    text: &str,
    bounds: LogicalRect,
    selected: bool,
    index: usize,
) -> SemanticNode {
    SemanticNode::new(occurrence, Role::ListBoxOption)
        .with_label(text)
        .with_bounds(bounds)
        .with_actions([Action::Click, Action::Focus])
        .with_selected(selected)
        // Zero-based: `accesskit_windows` adds the one a person hears.
        .with_position(index)
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

/// The parts of a template card's own two sentences. Separate arrays rather
/// than a formatted string, because a `ControlPart` is a `&'static str` on
/// purpose: a part cannot accidentally become a translated label.
const DETAIL: [&str; 6] = [
    "template-project-detail",
    "template-research-detail",
    "template-course-detail",
    "template-recipes-detail",
    "template-home-detail",
    "template-empty-detail",
];
const SUGGESTED: [&str; 6] = [
    "template-project-suggested",
    "template-research-suggested",
    "template-course-suggested",
    "template-recipes-suggested",
    "template-home-suggested",
    "template-empty-suggested",
];

/// The rows the forms already had and nothing read.
///
/// Both tables were written when these steps were built — the drawn copy is a
/// hard-coded English literal beside them — so this is reuse rather than a new
/// vocabulary. That the drawn copy is still English is the defect recorded in
/// `docs/features/accessibility/README.md`; §7.2 requires the accessible name to be
/// localized regardless, and papering over the difference by publishing the
/// English literal would hide it.
fn purpose_key(purpose: Purpose) -> Key {
    match purpose {
        Purpose::Work => Key::PurposeWork,
        Purpose::School => Key::PurposeSchool,
        Purpose::Personal => Key::PurposePersonal,
    }
}

fn template_key(index: usize) -> Key {
    [
        Key::TemplateProject,
        Key::TemplateResearch,
        Key::TemplateCourse,
        Key::TemplateRecipes,
        Key::TemplateHome,
        Key::TemplateEmpty,
    ][index]
}

fn template_detail_key(index: usize) -> Key {
    [
        Key::TemplateProjectDetail,
        Key::TemplateResearchDetail,
        Key::TemplateCourseDetail,
        Key::TemplateRecipesDetail,
        Key::TemplateHomeDetail,
        Key::TemplateEmptyDetail,
    ][index]
}

#[cfg(test)]
mod tests;
