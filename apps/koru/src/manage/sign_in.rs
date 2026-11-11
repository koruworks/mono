// SPDX-License-Identifier: MPL-2.0

//! A Profile's *Sign-in and sync*: how the Profile is signed in, and whether
//! it syncs through Koru Cloud.
//!
//! ```text
//! SIGN-IN AND SYNC
//! ┌──────────────────────────────────────────────────┐
//! │ repair — collision, erased identity, enrollment, │  maintainers' decision 3: the
//! │ account creation — when the account needs it     │  Profile's own repair,
//! ├──────────────────────────────────────────────────┤  instead of the rows
//! │ person@example.com                      Unlink…  │  decision 2: one row
//! │ Google                                            │  per Google Account
//! ├──────────────────────────────────────────────────┤
//! │ Add another Google Account…  Make offline-only…  │  decision 1: Sign out
//! │ Sign out…                                         │  is the device's
//! ├──────────────────────────────────────────────────┤
//! │ Cloud sync                               (on)    │  or Get Koru Cloud…
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! A Profile only on this computer has one row instead of the accounts: a
//! sentence saying so and the way into an account — *Sign in with Google*
//! while signed out, *Connect to your account* (the attach flow) while
//! signed in. Every control here is an `Intent` the old Manage Profiles
//! already sent; nothing in this file decides what an account operation is.

use super::profile_page::{Control, Look, Placed, Row, Section};
use super::*;
use koru_draw::geometry::Rect;
use koru_ui::button::{self, ActionButton, ActionStyle, ButtonRole};
use koru_ui::card::{Card, CardRow};
use koru_ui::toggle::Toggle;
use koru_workspace_host::account_ui::{Action, Surface, SurfaceKind};
use profiles::{LinkControl, SessionAction, SyncAvailability};

/// Between text buttons in the actions strip.
const STRIP_GAP: f32 = 8.0;

const LOCAL_LABEL: &str = "On this computer only";
const LOCAL_HINT: &str = "This profile lives on this computer and the devices linked to it.";
const SIGNED_OUT_LABEL: &str = "Signed out";
const SIGNED_OUT_HINT: &str = "Sign in to sync this profile with your Koru account.";
const UNLINKED_LABEL: &str = "Not linked to this sign-in";
const UNLINKED_HINT: &str =
    "This profile is on this computer only. Connect it to sync it with your Koru account.";
const CONNECTED_LABEL: &str = "Connected to a Koru account";
const CONNECTED_HINT: &str = "Sign-in details will appear when this account is available.";
const CLOUD_LABEL: &str = "Cloud sync";
const CLOUD_HINT: &str = "Sync this profile's workspaces through Koru Cloud.";
const RESUME_TITLE: &str = "Finish setting up your account";
const RESUME_BODY: &str = "Koru is waiting for your answer about creating an account.";

impl ManageView {
    /// The Profile's own account repair (decision 3), or `None`. The
    /// account-wide can't-connect state is not this Profile's to repair: it
    /// lives on Koru Cloud, and the page's status line points there.
    pub(super) fn repair_surface(&self, profile: ProfileId) -> Option<Surface> {
        self.c
            .account_surface_for(Some(profile))
            .filter(|surface| surface.kind != SurfaceKind::Unavailable)
    }

    pub(super) fn lay_sign_in(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        y: f32,
        controls: &mut Vec<Placed>,
    ) -> Section {
        let top = y + Card::new(Some(profile_page::SIGN_IN_TITLE)).header_h();
        let area = Rect::new(column.x, top, column.w, 0.0);
        let mut repair = None;
        let mut rows = Vec::new();
        let mut cy = top;
        if let Some(surface) = self.repair_surface(profile.id) {
            let buttons = surface
                .actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    (
                        Control::Repair(index),
                        self.account_action_label(*action).to_owned(),
                        if index == 0 {
                            ButtonRole::Primary
                        } else {
                            ButtonRole::Secondary
                        },
                    )
                })
                .collect();
            let words = self.lay_words(
                area,
                surface.title.to_owned(),
                surface.body.clone(),
                buttons,
                controls,
            );
            cy = words.rect.y + words.rect.h;
            repair = Some(words);
        } else if self.c.session_action() == SessionAction::ResumeAccountChoice {
            let resume = koru_i18n::t(crate::format::lang(), koru_i18n::Key::AccountCreationResume);
            let words = self.lay_words(
                area,
                RESUME_TITLE.into(),
                RESUME_BODY.into(),
                vec![(Control::Resume, resume.to_owned(), ButtonRole::Primary)],
                controls,
            );
            cy = words.rect.y + words.rect.h;
            repair = Some(words);
        } else {
            self.lay_sign_in_rows(profile, column, &mut cy, &mut rows, controls);
        }
        Section {
            rect: Rect::new(column.x, y, column.w, cy - y),
            repair,
            rows,
        }
    }

    fn lay_sign_in_rows(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
    ) {
        let signed_in = self.c.account().stage == AccountStage::SignedIn;
        let local_only = profile.account.is_none();
        if !signed_in {
            let session = match self.c.session_action() {
                SessionAction::SignInGoogle => {
                    let label = "Sign in with Google";
                    // The mark and its gap ride beside the label.
                    let w = button::button_width(&mut self.engine, label) + 28.0;
                    Some((
                        Control::Session,
                        label.to_owned(),
                        Look::Google,
                        w,
                        button::H,
                    ))
                }
                SessionAction::CancelSignIn => {
                    let w = button::button_width(&mut self.engine, "Cancel");
                    let look = Look::Button(ButtonRole::Secondary);
                    Some((Control::Session, "Cancel".to_owned(), look, w, button::H))
                }
                _ => None,
            };
            let (label, hint) = if local_only {
                (LOCAL_LABEL, LOCAL_HINT)
            } else {
                (SIGNED_OUT_LABEL, SIGNED_OUT_HINT)
            };
            // A build with no Koru Cloud has no sign-in to offer, so the row
            // says why there is no button rather than how to use one.
            let hint = if self.c.session_action() == SessionAction::NotConfigured {
                koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::CloudNotConfiguredBody,
                )
            } else {
                hint
            };
            self.lay_row(
                column,
                cy,
                rows,
                controls,
                label,
                Some(hint.into()),
                session,
            );
        } else if local_only || self.c.profile_unlinked_from_sign_in(profile) {
            // Local-only, or the account's but not linked to the sign-in in
            // use: either way it is connected with the same button.
            let (label, resting) = if local_only {
                (LOCAL_LABEL, LOCAL_HINT)
            } else {
                (UNLINKED_LABEL, UNLINKED_HINT)
            };
            let attach = match self.c.account_link_control() {
                Some((LinkControl::Attach(_), label)) => {
                    let w = button::button_width(&mut self.engine, &label);
                    let look = Look::Button(ButtonRole::Secondary);
                    Some((Control::Attach, label, look, w, button::H))
                }
                _ => None,
            };
            // While the attach is in flight its status takes the sentence's
            // place, where the button was.
            let hint = self.c.attach_status().unwrap_or_else(|| resting.into());
            self.lay_row(column, cy, rows, controls, label, Some(hint), attach);
        } else {
            let identities = self.c.identities_of(profile.id);
            if identities.is_empty() {
                let hint = Some(CONNECTED_HINT.to_owned());
                self.lay_row(column, cy, rows, controls, CONNECTED_LABEL, hint, None);
            }
            for (index, identity) in identities.iter().enumerate() {
                let email = identity
                    .verified_email
                    .as_deref()
                    .unwrap_or("Connected Google Account");
                let provider = match identity.provider.as_str() {
                    "google" => "Google Account",
                    "apple" => "Apple Account",
                    other => other,
                };
                let label = "Unlink…";
                let w = ActionButton::new(label, ActionStyle::Ghost, None).width(&mut self.engine);
                let unlink = (
                    Control::Unlink(index),
                    label.to_owned(),
                    Look::Action(ActionStyle::Ghost),
                    w,
                    ActionButton::H,
                );
                let hint = Some(provider.to_owned());
                self.lay_row(column, cy, rows, controls, email, hint, Some(unlink));
            }
        }
        if signed_in {
            let mut strip: Vec<(Control, &str)> = Vec::new();
            if !local_only {
                if matches!(
                    self.c.account_link_control(),
                    Some((LinkControl::AddGoogle(_), _))
                ) {
                    strip.push((Control::AddGoogle, "Add another Google Account…"));
                }
                if self.c.identities_of(profile.id).len() == 1 {
                    strip.push((Control::MakeOffline, "Make offline-only…"));
                }
            }
            strip.push((Control::SignOut, "Sign out…"));
            self.lay_strip(column, cy, rows, controls, &strip);
        }
        self.lay_cloud_row(profile, column, cy, rows, controls);
    }

    /// The Cloud sync row: the shared switch and what it says, or *Get Koru
    /// Cloud…* while the account has no subscription. None for a Profile
    /// that is not a Cloud thing at all.
    fn lay_cloud_row(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
    ) {
        let availability = self.c.cloud_sync_availability(profile);
        if availability == SyncAvailability::Hidden {
            return;
        }
        if self.c.cloud_status() == profiles::CloudStatus::Subscribe {
            let label = "Get Koru Cloud…";
            let w = button::button_width(&mut self.engine, label);
            let look = Look::Button(ButtonRole::Secondary);
            let get = (Control::GetCloud, label.to_owned(), look, w, button::H);
            let hint = Some(CLOUD_HINT.to_owned());
            self.lay_row(column, cy, rows, controls, CLOUD_LABEL, hint, Some(get));
            return;
        }
        let toggle = Toggle::new(self.c.cloud_sync_on(profile.id))
            .enabled(availability == SyncAvailability::Enabled)
            .pending(self.c.cloud_sync_pending_for(profile.id).is_some());
        let hint = self
            .c
            .cloud_sync_subtitle(profile)
            .unwrap_or_else(|| CLOUD_HINT.to_owned());
        let switch = (
            Control::CloudSync,
            CLOUD_LABEL.to_owned(),
            Look::Toggle(toggle),
            toggle.width(),
            Toggle::H,
        );
        self.lay_row(
            column,
            cy,
            rows,
            controls,
            CLOUD_LABEL,
            Some(hint),
            Some(switch),
        );
    }

    /// One row with its control against the right edge.
    #[allow(clippy::too_many_arguments)]
    fn lay_row(
        &mut self,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
        label: &str,
        hint: Option<String>,
        control: Option<(Control, String, Look, f32, f32)>,
    ) {
        let (cw, ch) = control.as_ref().map_or((0.0, 0.0), |c| (c.3, c.4));
        let text_w = CardRow::text_w(column.w, cw);
        let mut row = CardRow::new(label);
        if let Some(hint) = hint.as_deref() {
            row = row.hint(hint);
        }
        let h = row.height(&mut self.engine, text_w, ch).max(CardRow::MIN_H);
        let rect = Rect::new(column.x, *cy, column.w, h);
        if let Some((control, label, look, w, h)) = control {
            controls.push(Placed {
                control,
                rect: CardRow::control_slot(rect, w, h),
                label,
                look,
            });
        }
        rows.push(Row {
            rect,
            label: Some(label.to_owned()),
            hint,
            text_w,
        });
        *cy += h;
    }

    /// A row of text buttons, wrapping when the column is narrow.
    fn lay_strip(
        &mut self,
        column: Rect,
        cy: &mut f32,
        rows: &mut Vec<Row>,
        controls: &mut Vec<Placed>,
        strip: &[(Control, &str)],
    ) {
        let left = column.x + CardRow::PAD_X;
        let right = column.x + column.w - CardRow::PAD_X;
        let mut x = left;
        let mut y = *cy + CardRow::PAD_Y;
        for (control, label) in strip {
            let w = ActionButton::new(label, ActionStyle::Ghost, None)
                .width(&mut self.engine)
                .min(right - left);
            if x > left && x + w > right {
                x = left;
                y += ActionButton::H + STRIP_GAP;
            }
            controls.push(Placed {
                control: *control,
                rect: Rect::new(x, y, w, ActionButton::H),
                label: (*label).to_owned(),
                look: Look::Action(ActionStyle::Ghost),
            });
            x += w + STRIP_GAP;
        }
        let h = y + ActionButton::H + CardRow::PAD_Y - *cy;
        rows.push(Row {
            rect: Rect::new(column.x, *cy, column.w, h),
            label: None,
            hint: None,
            text_w: 0.0,
        });
        *cy += h;
    }

    pub(super) fn paint_sign_in(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        section: &Section,
        laid: &profile_page::Laid,
    ) {
        let r = section.rect;
        let _section = inspect::scope(
            "Manage sign-in and sync",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("repair", section.repair.is_some(), "controller");
                rec.property("rows", section.rows.len(), "sign_in::lay_sign_in");
            },
        );
        let dividers: Vec<f32> = section.rows.iter().skip(1).map(|row| row.rect.y).collect();
        let _card = Card::new(Some(profile_page::SIGN_IN_TITLE)).draw(
            canvas,
            root,
            &mut self.engine,
            r,
            &dividers,
            1.0,
        );
        if let Some(words) = &section.repair {
            let w = words.rect;
            let _attention = inspect::scope(
                "Manage attention",
                "part",
                root,
                scene::rect(w.x, w.y, w.w, w.h),
                |rec| rec.property("about", "this Profile's account (decision 3)", "controller"),
            );
            self.paint_words(canvas, root, words, laid);
        }
        for row in &section.rows {
            match &row.label {
                Some(label) => {
                    let mut card_row = CardRow::new(label);
                    if let Some(hint) = row.hint.as_deref() {
                        card_row = card_row.hint(hint);
                    }
                    let _row =
                        card_row.draw(canvas, root, &mut self.engine, row.rect, row.text_w, 1.0);
                    self.paint_controls_in(canvas, root, row.rect, laid);
                }
                None => {
                    let s = row.rect;
                    let _strip = inspect::scope(
                        "Manage account actions",
                        "part",
                        root,
                        scene::rect(s.x, s.y, s.w, s.h),
                        |_| {},
                    );
                    self.paint_controls_in(canvas, root, row.rect, laid);
                }
            }
        }
    }

    /// What an account repair action says, in the old Manage Profiles'
    /// words — the same action under the same name on both surfaces.
    pub(super) fn account_action_label(&self, action: Action) -> &'static str {
        let lang = crate::format::lang();
        let code = self.c.account().failure_code.as_deref();
        match action {
            Action::ConfirmAccountCreation => {
                koru_i18n::t(lang, koru_i18n::Key::AccountCreationConfirm)
            }
            Action::CancelAccountCreation => {
                koru_i18n::t(lang, koru_i18n::Key::AccountCreationCancel)
            }
            Action::SignInGoogle if matches!(code, Some("collision" | "collision_required")) => {
                "Sign in with the connected Google Account"
            }
            Action::SignInGoogle if matches!(code, Some("account_erased" | "identity_erased")) => {
                "Reconnect with Google"
            }
            Action::SignInGoogle => "Sign in with Google",
            Action::SignInAnotherGoogle => "Use another Google Account",
            Action::ChooseProfile => "Choose another Profile",
            Action::Retry => koru_i18n::t(lang, koru_i18n::Key::ReminderRetry),
            Action::RetryEnrollment => "Try again",
            Action::ContactSupport => "Contact support",
            Action::ContinueOffline => koru_i18n::t(lang, koru_i18n::Key::AccountContinueOffline),
            Action::Cancel => "Cancel",
            Action::CancelEnrollment => "Cancel connection",
        }
    }
}
