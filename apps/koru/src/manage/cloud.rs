// SPDX-License-Identifier: MPL-2.0

//! The *Koru Cloud* page: the account this computer is signed in to, the
//! subscription, and which Profiles sync — with each syncing Profile's upload
//! progress a disclosure away.
//!
//! ```text
//! (•) Koru Cloud                                   the dot: subscribed
//! Subscribed · syncing 1 of 2 profiles             the lead: today's line —
//! Manage subscription                              or, not subscribed, the
//!                                                  sentence — and its action
//! ┌ attention ─────────────────────────────────┐   the account-wide can't-
//! │ Koru can't connect right now               │   connect state (decision 3)
//! │ [Try again] [Continue offline]             │
//! └────────────────────────────────────────────┘
//! Account
//! ┌────────────────────────────────────────────┐
//! │ person@example.com              [Accounts…] │   the account switcher
//! │ Signed in on this computer                  │   (decision 16's home)
//! └────────────────────────────────────────────┘
//! ┌ attention ─────────────────────────────────┐   decision 4: beside the
//! │ This device has Profiles from another …    │   switcher
//! │ [Create a new Profile with …] [Sign in …]  │
//! └────────────────────────────────────────────┘
//! Subscription                                     not subscribed: the
//! ┌────────────────────────────────────────────┐   features, Subscribe and
//! │ • 100 GB encrypted backup …                 │   Learn more
//! │ [Subscribe to Koru Cloud]  Learn more       │
//! └────────────────────────────────────────────┘
//! Which profiles sync
//! ┌────────────────────────────────────────────┐
//! │ [tile] Work                          (on)   │   a signed-in Profile's
//! │        person@… · Syncing via Koru Cloud    │   switch (D258 on turning
//! │        ▸ Show sync progress                 │   on); decision 20: its
//! │          1 of 3 workspaces synced           │   progress under its row
//! ├────────────────────────────────────────────┤
//! │ [tile] Scratch                   [Sign in…] │   that Profile's page
//! │        Not signed in                        │
//! ├────────────────────────────────────────────┤
//! │ [tile] Family                        (off)  │   the account's Profile
//! │        Not on this computer                 │   that is not here: on
//! └────────────────────────────────────────────┘   brings it, and it syncs
//! ```
//!
//! **The account's Profiles that are not on this computer are rows here**
//! (maintainer, 2026-11-11), after the ones that are. They were the nav's
//! *Other Profiles*, each with a *Connect*; this is the page that answers
//! *which profiles sync*, so this is where one is turned on. The switch is
//! the controller's inline connect (`Intent::Connect`), and a Profile brought
//! here starts with Cloud sync on (ADR-000-createdwithaccount).
//!
//! **Everything here is the controller's**, as Manage Profiles' Cloud card,
//! switches and progress section were: the same `Intent`s, the same words
//! (maintainers' decision 8), and the same poll — pointed at the one Profile whose
//! disclosure is open (`ProgressOwner::Disclosed`), only while this page is
//! the tab in front. The page lays itself out into `profile_page::Laid`, so
//! drawing, the hit test, keyboard order and the accessibility tree read one
//! answer, as every Manage page does.

use super::profile_page::{Control, Look, Placed, Words};
use super::*;
use koru_draw::geometry::Rect;
use koru_draw::tile::Tile;
use koru_i18n::Key as TextKey;
use koru_ui::PageShell;
use koru_ui::button::{self, ActionButton, ActionStyle, ButtonRole};
use koru_ui::card::{Card, CardRow};
use koru_ui::text_paragraph::KoruTextParagraph;
use koru_ui::toggle::Toggle;
use profiles::{CloudStatus, ProgressOwner, SyncAvailability};

pub(crate) mod progress;
#[cfg(test)]
mod tests;

/// What the not-subscribed card lists, in the order the maintainers' design lists
/// it — the code's list, which Manage Profiles drew too.
pub(crate) const FEATURES: [TextKey; 10] = [
    TextKey::CloudFeatureBackup,
    TextKey::CloudFeatureWebDocs,
    TextKey::CloudFeatureReview,
    TextKey::CloudFeatureForms,
    TextKey::CloudFeatureWebhooks,
    TextKey::CloudFeatureEmail,
    TextKey::CloudFeatureAgents,
    TextKey::CloudFeatureSchedules,
    TextKey::CloudFeatureMcp,
    TextKey::CloudFeatureApi,
];

const LEAD: &str = "One subscription covers every profile on this computer. Each signed-in profile chooses whether it syncs.";
const ACCOUNT_TITLE: &str = "Account";
const ACCOUNTS: &str = "Accounts\u{2026}";
const SIGNED_IN_HINT: &str = "Signed in on this computer";
/// Before the time Koru Cloud last accepted the sign-in.
const CHECKED: &str = "checked";
const SIGNING_IN: &str = "Signing in\u{2026}";
const NOT_SIGNED_IN: &str = "Not signed in";
const SIGNED_OUT_HINT: &str = "Sign in to sync your profiles through Koru Cloud.";
pub(super) const SYNC_TITLE: &str = "Which profiles sync";
const NOT_SYNCING: &str = "Not syncing";
const NOT_HERE: &str = "Not on this computer";
const SIGNED_IN: &str = "Signed in";
const SIGN_IN: &str = "Sign in\u{2026}";
/// On a Profile this sign-in is not linked to: its page, where it is
/// connected again.
pub(super) const CONNECT: &str = "Connect\u{2026}";
pub(super) const SHOW_PROGRESS: &str = "Show sync progress";
pub(super) const HIDE_PROGRESS: &str = "Hide sync progress";
const DIFFERENT_TITLE: &str = "This device has Profiles from another account";
const DIFFERENT_SIGN_IN: &str = "Sign in with another Google Account";
const NO_PROFILES: &str = "No profiles on this computer yet.";
const FEATURES_TITLE: &str = "Subscription";

/// Between a lead's action and the lead.
const GAP: f32 = 12.0;
/// The status dot before the title, and the room it takes.
const DOT_R: f32 = 5.0;
const DOT_W: f32 = 20.0;
/// A bullet's indent and the gap between bullets.
const BULLET_INDENT: f32 = 18.0;
const BULLET_GAP: f32 = 6.0;
/// Inside the features card.
const CARD_PAD: f32 = 16.0;
/// A sync row's tile, and the gap after it.
pub(super) const ROW_TILE: f32 = 28.0;
const TILE_GAP: f32 = 12.0;
/// Between a control and the text beside it.
const CONTROL_GAP: f32 = 12.0;

/// The account row inside the *Account* card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AccountRow {
    pub rect: Rect,
    pub label: String,
    pub hint: String,
    pub text_w: f32,
}

/// The subscription block: whichever of its shapes the state has.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Subscription {
    /// The page's lead: the not-subscribed sentence, or the status line in
    /// every other state.
    pub text: Option<(Rect, String)>,
    /// The not-subscribed card around the features and its two actions, its
    /// caption included.
    pub card: Option<Rect>,
    pub bullets: Vec<(Rect, String)>,
}

/// One local Profile's row on *Which profiles sync*.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SyncRow {
    pub profile: ProfileId,
    /// The whole row, its progress included.
    pub rect: Rect,
    pub tile: Rect,
    pub name: Rect,
    pub name_text: String,
    pub sub: Rect,
    pub sub_text: String,
    /// The switch as drawn, its box and whether it is enabled; only an
    /// enabled one is a control.
    pub toggle: Option<(Toggle, Rect, bool)>,
    /// The open disclosure's progress (maintainers' decision 20).
    pub progress: Option<progress::ProgressLaid>,
}

/// Where everything on the page goes, unscrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CloudLaid {
    /// The title and the lead — the subscription's line.
    pub head: page::Head,
    /// The status dot's centre, while the account is subscribed.
    pub dot: Option<(f32, f32)>,
    /// The account-wide can't-connect card.
    pub unreachable: Option<Words>,
    pub account_card: Rect,
    pub account: AccountRow,
    /// *This device has Profiles from another account*.
    pub different: Option<Words>,
    pub subscription: Subscription,
    /// *Which profiles sync*, its title included.
    pub sync_card: Rect,
    pub rows: Vec<SyncRow>,
    /// The account's Profiles that are not on this computer, after `rows`:
    /// the same row, whose switch brings the Profile here.
    pub elsewhere: Vec<SyncRow>,
    pub empty: Option<Rect>,
}

impl ManageView {
    // ---- visibility ----------------------------------------------------------

    /// Whether the window has this tab in front. Cloud status is read, and
    /// progress polled, only while it is — and only for a page that shows
    /// them.
    pub fn set_in_front(&mut self, front: bool) {
        if self.in_front != front {
            self.in_front = front;
            self.sync();
        }
    }

    /// The window came back, or this tab did: ask Koru Cloud again — when the
    /// page showing reads it. Any other page would only forget what the nav's
    /// *On* / *Off* says, with nothing to ask again until a page that reads
    /// it shows.
    pub fn refresh_cloud(&mut self) {
        if self.wants_cloud() {
            self.delegate(|c| c.refresh_cloud());
        }
    }

    /// The pages that read Koru Cloud: this one, and a Profile's page whose
    /// *Sign-in and sync* carries the switch or *Get Koru Cloud…* — the
    /// Profiles bound to an account. A local-only Profile's page, *Linked
    /// devices* and the create form read nothing.
    fn wants_cloud(&self) -> bool {
        match self.page {
            Some(Page::Cloud) => true,
            Some(Page::Profile(_)) => self
                .page_profile()
                .is_some_and(|profile| profile.account.is_some()),
            _ => false,
        }
    }

    /// Tell the controller what is showing: whether Cloud is visible, and
    /// which Profile's progress the poll follows — none unless this page is
    /// in front with a disclosure open.
    pub(super) fn apply_cloud_visibility(&mut self) {
        let showing = self.in_front && self.page == Some(Page::Cloud);
        let disclosed = self.disclosed.filter(|_| showing);
        self.c
            .set_progress_owner(ProgressOwner::Disclosed(disclosed));
        let visible = self.in_front && self.wants_cloud();
        self.c.set_cloud_visible(visible);
    }

    // ---- the account switcher -------------------------------------------------

    /// Open the account switcher over the page — *Accounts…*, or the nav's
    /// *{n} Profiles hidden* (maintainers' decision 16).
    pub(super) fn open_account_switcher(&mut self) {
        // Accounts are Koru Cloud's (koru-features), and a build with no
        // Koru Cloud has none to switch between.
        if !koru_features::cloud_sync() || self.cloud_not_configured() {
            return;
        }
        if self.page != Some(Page::Cloud) {
            self.show(Page::Cloud);
        }
        self.switcher.open = true;
    }

    /// The switcher's size: the page area, which it replaces while open.
    fn switcher_size(&self) -> (f32, f32) {
        let area = self.page_area();
        (area.w, area.h)
    }

    /// A key while the switcher is open: every key is its.
    pub(super) fn switcher_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !self.switcher.open {
            return None;
        }
        let command = self.switcher.key(
            self.c.account(),
            crate::format::lang(),
            self.switcher_size().1,
            key,
            mods,
        );
        if let Some(command) = command {
            self.perform(Intent::AccountSwitcher(command));
        }
        Some(true)
    }

    /// A press while the switcher is open: a row of it, or — in the nav —
    /// closing it and doing what the press landed on (`None`).
    pub(super) fn switcher_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if !self.switcher.open {
            return None;
        }
        let area = self.page_area();
        if !area.contains(x, y) {
            self.switcher.open = false;
            return None;
        }
        let command = self.switcher.click(
            self.c.account(),
            crate::format::lang(),
            self.switcher_size(),
            x - area.x,
            y - area.y,
        );
        if let Some(command) = command {
            self.perform(Intent::AccountSwitcher(command));
        }
        Some(true)
    }

    /// The wheel over the open switcher moves its rows.
    pub(super) fn switcher_scroll(&mut self, delta: MouseScrollDelta) -> bool {
        if !self.switcher.open || !self.page_area().contains(self.pointer.0, self.pointer.1) {
            return false;
        }
        let amount = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(point) => point.y as f32,
        };
        let height = self.switcher_size().1;
        self.switcher.scroll(self.c.account(), height, amount);
        true
    }

    // ---- layout --------------------------------------------------------------

    pub(super) fn lay_cloud_page(&mut self) -> profile_page::Laid {
        let column = self.column();
        let mut controls = Vec::new();
        let subscribed =
            self.c.account().stage == AccountStage::SignedIn && self.c.cloud_access() == Some(true);

        // The title, with the subscribed dot before it, and the
        // subscription's line as its lead; then what can be done about it.
        let title_text = koru_i18n::t(crate::format::lang(), TextKey::CloudTitle);
        let indent = if subscribed { DOT_W } else { 0.0 };
        let lead = self.subscription_line();
        let head = self.lay_head(column, indent, 0.0, title_text, &lead);
        let line = theme::role::title().metrics.line_height;
        let dot = subscribed.then(|| (column.x + DOT_R + 1.0, column.y + line * 0.5));
        let mut subscription = Subscription {
            text: Some((head.lead, lead)),
            ..Default::default()
        };
        let mut y = head.lead.y + head.lead.h;
        self.lay_lead_action(column, &mut y, &mut controls);
        y += PageShell::LEAD_GAP;

        // The account-wide can't-connect state, at the top (decision 3).
        let unreachable = self.c.account_wide_surface().map(|surface| {
            let buttons = surface
                .actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    (
                        Control::Unreachable(index),
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
                Rect::new(column.x, y, column.w, 0.0),
                surface.title.to_owned(),
                surface.body.clone(),
                buttons,
                &mut controls,
            );
            y = words.rect.y + words.rect.h + Card::GAP;
            words
        });

        // The account, and its switcher.
        let card_top = y;
        let row_y = y + Card::new(Some(ACCOUNT_TITLE)).header_h();
        let (label, hint) = self.account_words();
        // No *Accounts…* in a build with no Koru Cloud: there is no account
        // to switch to, and the hint beside the row says why.
        let switcher = !self.cloud_not_configured();
        let accounts_w = if switcher {
            button::button_width(&mut self.engine, ACCOUNTS)
        } else {
            0.0
        };
        let text_w = CardRow::text_w(column.w, accounts_w);
        let row_h = CardRow::new(&label)
            .hint(&hint)
            .height(&mut self.engine, text_w, button::H)
            .max(CardRow::MIN_H);
        let row = Rect::new(column.x, row_y, column.w, row_h);
        if switcher {
            controls.push(Placed {
                control: Control::Accounts,
                rect: CardRow::control_slot(row, accounts_w, button::H),
                label: ACCOUNTS.into(),
                look: Look::Button(ButtonRole::Secondary),
            });
        }
        let account_card = Rect::new(column.x, card_top, column.w, row_y + row_h - card_top);
        y = account_card.y + account_card.h + Card::GAP;

        // Another account's Profiles, beside the switcher (decision 4).
        let different = self.c.different_account_email().map(str::to_owned).map(|email| {
            let buttons = vec![
                (
                    Control::DifferentCreate,
                    format!("Create a new Profile with {email}"),
                    ButtonRole::Primary,
                ),
                (
                    Control::DifferentSignIn,
                    DIFFERENT_SIGN_IN.to_owned(),
                    ButtonRole::Secondary,
                ),
                (
                    Control::DifferentOffline,
                    "Continue offline".to_owned(),
                    ButtonRole::Secondary,
                ),
            ];
            let words = self.lay_words(
                Rect::new(column.x, y, column.w, 0.0),
                DIFFERENT_TITLE.to_owned(),
                format!(
                    "You're signed in as {email}. Profiles already on this device remain available offline."
                ),
                buttons,
                &mut controls,
            );
            y = words.rect.y + words.rect.h + Card::GAP;
            words
        });

        if let Some((card, bullets)) = self.lay_features(column, y, &mut controls) {
            y = card.y + card.h + Card::GAP;
            subscription.card = Some(card);
            subscription.bullets = bullets;
        }

        // Which profiles sync.
        let sync_top = y;
        let mut ry = y + Card::new(Some(SYNC_TITLE)).header_h();
        let profiles = self.c.profiles().to_vec();
        let mut rows = Vec::new();
        for profile in &profiles {
            let row = self.lay_sync_row(column, ry, profile, &mut controls);
            ry = row.rect.y + row.rect.h;
            rows.push(row);
        }
        // Then the account's Profiles that are not on this computer.
        let missing: Vec<(ProfileId, String)> = self
            .c
            .missing_account_profiles()
            .iter()
            .filter_map(|row| {
                let id = ProfileId::parse(&row.profile_id)?;
                Some((id, row.name.clone().unwrap_or_else(|| "Profile".into())))
            })
            .collect();
        let mut elsewhere = Vec::new();
        for (profile, name) in &missing {
            let row = self.lay_elsewhere_row(column, ry, *profile, name, &mut controls);
            ry = row.rect.y + row.rect.h;
            elsewhere.push(row);
        }
        let empty = (rows.is_empty() && elsewhere.is_empty()).then(|| {
            let h = CardRow::MIN_H;
            let rect = Rect::new(column.x, ry, column.w, h);
            ry += h;
            rect
        });
        let sync_card = Rect::new(column.x, sync_top, column.w, ry - sync_top);

        profile_page::Laid {
            cloud: Some(CloudLaid {
                head,
                dot,
                unreachable,
                account_card,
                account: AccountRow {
                    rect: row,
                    label,
                    hint,
                    text_w,
                },
                different,
                subscription,
                sync_card,
                rows,
                elsewhere,
                empty,
            }),
            controls,
            height: ry - column.y,
            ..Default::default()
        }
    }

    /// Whether this build has no Koru Cloud (`AccountStage::NotConfigured`):
    /// the page then says so, and offers no sign-in and no switcher.
    fn cloud_not_configured(&self) -> bool {
        self.c.account().stage == AccountStage::NotConfigured
    }

    /// The account row's name and the line under it.
    fn account_words(&self) -> (String, String) {
        let account = self.c.account();
        match account.stage {
            // The Google Docs importer's words for the same situation, beside
            // the thing that cannot be used.
            AccountStage::NotConfigured => (
                NOT_SIGNED_IN.to_owned(),
                koru_i18n::t(crate::format::lang(), TextKey::NotSetUpInThisBuild).to_owned(),
            ),
            AccountStage::SignedIn => {
                // The email, or plain *Signed in* — never the tail of an
                // account id (2026-11-10), which tells a person nothing.
                // …and when Koru Cloud last accepted the sign-in: every
                // start refreshes it, and nothing on screen said so.
                let hint = match account
                    .session_checked_at_millis
                    .map(|at| crate::format::absolute_local(crate::format::lang(), at))
                    .filter(|when| !when.is_empty())
                {
                    Some(when) => format!("{SIGNED_IN_HINT} \u{b7} {CHECKED} {when}"),
                    None => SIGNED_IN_HINT.to_owned(),
                };
                (
                    self.c
                        .signed_in_email()
                        .map_or_else(|| SIGNED_IN.to_owned(), str::to_owned),
                    hint,
                )
            }
            AccountStage::Restoring
            | AccountStage::Refreshing
            | AccountStage::WaitingForBrowser
            | AccountStage::Exchanging
            | AccountStage::AwaitingAccountCreation => {
                (SIGNING_IN.to_owned(), SIGNED_OUT_HINT.to_owned())
            }
            _ => (NOT_SIGNED_IN.to_owned(), SIGNED_OUT_HINT.to_owned()),
        }
    }

    /// Whether the account is signed in and known not to be subscribed: the
    /// state the features card is for.
    fn offering_subscription(&self) -> bool {
        self.c.account().stage == AccountStage::SignedIn
            && self.c.cloud_status() == CloudStatus::Subscribe
    }

    /// The page's lead: the not-subscribed sentence while there is a
    /// subscription to offer, today's line in every other state.
    fn subscription_line(&self) -> String {
        if self.offering_subscription() {
            LEAD.to_owned()
        } else {
            self.c.cloud_body_text(crate::format::lang())
        }
    }

    /// What can be done about the lead's line, under it: *Manage
    /// subscription*, *Try again*, or signing in. While a subscription is
    /// offered, its button is in the features card instead.
    fn lay_lead_action(&mut self, column: Rect, y: &mut f32, controls: &mut Vec<Placed>) {
        if self.offering_subscription() || !self.c.cloud_actionable() {
            return;
        }
        let signed_in = self.c.account().stage == AccountStage::SignedIn;
        let label = self.c.cloud_action_label().to_owned();
        let placed = if self.c.cloud_action_is_link() {
            let line = theme::role::meta().metrics.line_height;
            let w = self
                .engine
                .simple_layout(&label, theme::role::meta())
                .width();
            Placed {
                control: Control::CloudPrimary,
                rect: Rect::new(column.x, *y + 6.0, w.min(column.w), line),
                label,
                look: Look::Link,
            }
        } else if !signed_in {
            // The mark and its gap ride beside the label.
            let w = button::button_width(&mut self.engine, &label) + 28.0;
            Placed {
                control: Control::CloudPrimary,
                rect: Rect::new(column.x, *y + GAP, w.min(column.w), button::H),
                label,
                look: Look::Google,
            }
        } else {
            let w = button::button_width(&mut self.engine, &label);
            Placed {
                control: Control::CloudPrimary,
                rect: Rect::new(column.x, *y + GAP, w.min(column.w), button::H),
                label,
                look: Look::Button(ButtonRole::Secondary),
            }
        };
        *y = placed.rect.y + placed.rect.h;
        controls.push(placed);
    }

    /// While a subscription is offered: a captioned card of what it brings,
    /// with *Subscribe* and *Learn more* at its foot. Returns the card, its
    /// caption included, and the bullets.
    #[allow(clippy::type_complexity)]
    fn lay_features(
        &mut self,
        column: Rect,
        top: f32,
        controls: &mut Vec<Placed>,
    ) -> Option<(Rect, Vec<(Rect, String)>)> {
        if !self.offering_subscription() {
            return None;
        }
        let lang = crate::format::lang();
        let p = theme::p();
        let x = column.x + CARD_PAD;
        let w = (column.w - CARD_PAD * 2.0).max(1.0);
        let mut cy = top + Card::new(Some(FEATURES_TITLE)).header_h() + CARD_PAD;
        let mut bullets = Vec::new();
        for feature in FEATURES {
            let text = koru_i18n::t(lang, feature).to_owned();
            let h = KoruTextParagraph::new(&text, theme::role::meta(), "", p.text_dim)
                .measure(&mut self.engine, (w - BULLET_INDENT).max(1.0));
            bullets.push((
                Rect::new(x + BULLET_INDENT, cy, (w - BULLET_INDENT).max(1.0), h),
                text,
            ));
            cy += h + BULLET_GAP;
        }
        cy += 10.0;
        if self.c.cloud_actionable() {
            let label = self.c.cloud_action_label();
            let bw = button::button_width(&mut self.engine, label).min(w);
            controls.push(Placed {
                control: Control::CloudPrimary,
                rect: Rect::new(x, cy, bw, button::H),
                label: label.to_owned(),
                look: Look::Button(ButtonRole::Primary),
            });
            if self.c.cloud_learn_more_visible() {
                let more = koru_i18n::t(lang, TextKey::CloudLearnMore);
                let line = theme::role::meta().metrics.line_height;
                let lw = self.engine.simple_layout(more, theme::role::meta()).width();
                let (lx, ly) = if bw + CONTROL_GAP * 2.0 + lw <= w {
                    (x + bw + CONTROL_GAP * 2.0, cy + (button::H - line) * 0.5)
                } else {
                    (x, cy + button::H + 8.0)
                };
                controls.push(Placed {
                    control: Control::CloudLearnMore,
                    rect: Rect::new(lx, ly, lw, line),
                    label: more.to_owned(),
                    look: Look::Link,
                });
                cy = cy.max(ly + line - button::H);
            }
            cy += button::H;
        }
        Some((
            Rect::new(column.x, top, column.w, cy + CARD_PAD - top),
            bullets,
        ))
    }

    /// One Profile's row: its tile, name and sub-line, then its switch or
    /// *Sign in…*, then — while it syncs — its progress disclosure.
    fn lay_sync_row(
        &mut self,
        column: Rect,
        y: f32,
        profile: &ProfileSummary,
        controls: &mut Vec<Placed>,
    ) -> SyncRow {
        let signed_in =
            profile.account.is_some() && self.c.account().stage == AccountStage::SignedIn;
        let availability = self.c.cloud_sync_availability(profile);
        let on = self.c.cloud_sync_on(profile.id);
        let pending = self.c.cloud_sync_pending_for(profile.id).is_some();
        let x = column.x + CardRow::PAD_X;
        let right = column.x + column.w - CardRow::PAD_X;

        // The trailing control: the switch for a signed-in Profile, *Sign
        // in…* for any other — and nothing at all in a build with no Koru
        // Cloud, whose sub-line says why.
        // A Profile this sign-in is not linked to has no switch to offer:
        // *Connect…* opens its page, where it is linked again.
        let unlinked = self.c.profile_unlinked_from_sign_in(profile);
        let action = if unlinked { CONNECT } else { SIGN_IN };
        let toggle = (signed_in && !unlinked).then(|| {
            Toggle::new(on)
                .enabled(availability == SyncAvailability::Enabled)
                .pending(pending)
        });
        let sign_in = toggle.is_none() && !self.cloud_not_configured();
        let (control_w, control_h) = match toggle {
            Some(toggle) => (toggle.width(), Toggle::H),
            None if !sign_in => (0.0, 0.0),
            None => (
                ActionButton::new(action, ActionStyle::Secondary, None).width(&mut self.engine),
                ActionButton::H,
            ),
        };
        let text_x = x + ROW_TILE + TILE_GAP;
        let text_w = (right - control_w - CONTROL_GAP - text_x).max(1.0);
        let name_style = theme::role::row();
        let name_line = name_style.metrics.line_height;
        let name_text = self.engine.elided(&profile.name, name_style, text_w);
        let name_w = self
            .engine
            .simple_layout(&name_text, name_style)
            .width()
            .min(text_w);
        let name = Rect::new(text_x, y + CardRow::PAD_Y, name_w, name_line);
        let sub_text = self.sync_sub_line(profile, signed_in, on);
        let sub_h = KoruTextParagraph::new(&sub_text, theme::role::meta(), "", theme::p().text_dim)
            .measure(&mut self.engine, text_w);
        let sub = Rect::new(text_x, name.y + name_line + 2.0, text_w, sub_h);
        let head_h = (CardRow::PAD_Y * 2.0 + name_line + 2.0 + sub_h).max(CardRow::MIN_H);
        let head = Rect::new(column.x, y, column.w, head_h);
        let tile = Rect::new(x, y + (head_h - ROW_TILE) * 0.5, ROW_TILE, ROW_TILE);
        let slot = CardRow::control_slot(head, control_w, control_h);
        match toggle {
            Some(toggle) if availability == SyncAvailability::Enabled => controls.push(Placed {
                control: Control::ProfileSync(profile.id),
                rect: slot,
                label: format!("Cloud sync for {}", profile.name),
                look: Look::Toggle(toggle),
            }),
            Some(_) => {}
            None if !sign_in => {}
            None => controls.push(Placed {
                control: Control::ProfileSignIn(profile.id),
                rect: slot,
                label: action.into(),
                look: Look::Action(ActionStyle::Secondary),
            }),
        }
        let mut bottom = y + head_h;

        // Decision 20: a syncing Profile's progress, a disclosure away.
        let syncs = availability == SyncAvailability::Enabled && on && !pending;
        let mut progress = None;
        if syncs {
            let open = self.disclosed == Some(profile.id);
            let label = if open { HIDE_PROGRESS } else { SHOW_PROGRESS };
            let line = theme::role::meta().metrics.line_height;
            let w = self
                .engine
                .simple_layout(label, theme::role::meta())
                .width()
                + progress::DISCLOSURE_W;
            let rect = Rect::new(text_x - 2.0, bottom - CardRow::PAD_Y + 2.0, w + 4.0, line);
            controls.push(Placed {
                control: Control::Progress(profile.id),
                rect,
                label: label.into(),
                look: Look::Disclosure(open),
            });
            bottom = rect.y + rect.h + CardRow::PAD_Y;
            if open {
                let laid = self.lay_progress(text_x, right - text_x, bottom - 4.0, controls);
                if let Some(laid) = laid {
                    bottom = laid.rect.y + laid.rect.h + CardRow::PAD_Y;
                    progress = Some(laid);
                }
            }
        }
        SyncRow {
            profile: profile.id,
            rect: Rect::new(column.x, y, column.w, bottom - y),
            tile,
            name,
            name_text,
            sub,
            sub_text,
            toggle: toggle.map(|toggle| (toggle, slot, availability == SyncAvailability::Enabled)),
            progress,
        }
    }

    /// An account Profile that is not on this computer: its tile, name and
    /// where it stands, then a switch that brings it here. While its connect
    /// is out the switch shows where it is going and is no control; while
    /// another Profile's is, none of them is — one connect at a time is the
    /// controller's rule. After a failed attempt the switch is the retry.
    fn lay_elsewhere_row(
        &mut self,
        column: Rect,
        y: f32,
        profile: ProfileId,
        name: &str,
        controls: &mut Vec<Placed>,
    ) -> SyncRow {
        let x = column.x + CardRow::PAD_X;
        let right = column.x + column.w - CardRow::PAD_X;
        let connecting = self.c.inline_connect() == Some((profile, false));
        let pressable = self.c.connect_label(profile).is_some();
        let toggle = Toggle::new(connecting)
            .enabled(pressable)
            .pending(connecting);
        let text_x = x + ROW_TILE + TILE_GAP;
        let text_w = (right - toggle.width() - CONTROL_GAP - text_x).max(1.0);
        let name_style = theme::role::row();
        let name_line = name_style.metrics.line_height;
        let name_text = self.engine.elided(name, name_style, text_w);
        let name_w = self
            .engine
            .simple_layout(&name_text, name_style)
            .width()
            .min(text_w);
        let name_rect = Rect::new(text_x, y + CardRow::PAD_Y, name_w, name_line);
        let sub_text = self
            .c
            .profile_connection_status(profile)
            .unwrap_or(NOT_HERE)
            .to_owned();
        let sub_h = KoruTextParagraph::new(&sub_text, theme::role::meta(), "", theme::p().text_dim)
            .measure(&mut self.engine, text_w);
        let sub = Rect::new(text_x, name_rect.y + name_line + 2.0, text_w, sub_h);
        let head_h = (CardRow::PAD_Y * 2.0 + name_line + 2.0 + sub_h).max(CardRow::MIN_H);
        let head = Rect::new(column.x, y, column.w, head_h);
        let tile = Rect::new(x, y + (head_h - ROW_TILE) * 0.5, ROW_TILE, ROW_TILE);
        let slot = CardRow::control_slot(head, toggle.width(), Toggle::H);
        if pressable {
            controls.push(Placed {
                control: Control::ProfileBring(profile),
                rect: slot,
                label: format!("Sync {name} to this computer"),
                look: Look::Toggle(toggle),
            });
        }
        SyncRow {
            profile,
            rect: head,
            tile,
            name: name_rect,
            name_text,
            sub,
            sub_text,
            toggle: Some((toggle, slot, pressable)),
            progress: None,
        }
    }

    /// A row's sub-line: *Not signed in*, or who it is signed in as and how
    /// its Cloud sync stands, in today's words (decision 8).
    fn sync_sub_line(&self, profile: &ProfileSummary, signed_in: bool, on: bool) -> String {
        if self.cloud_not_configured() {
            return koru_i18n::t(crate::format::lang(), TextKey::CloudNotConfigured).to_owned();
        }
        if !signed_in {
            return NOT_SIGNED_IN.to_owned();
        }
        // Not linked to the sign-in in use: the email in front of the line
        // would say the opposite, so the line is the reason alone.
        if self.c.profile_unlinked_from_sign_in(profile) {
            return koru_i18n::t(crate::format::lang(), TextKey::CloudSyncNotLinkedReason)
                .to_owned();
        }
        let who = self
            .c
            .identities_of(profile.id)
            .into_iter()
            .find_map(|identity| identity.verified_email)
            .or_else(|| self.c.signed_in_email().map(str::to_owned))
            .unwrap_or_else(|| SIGNED_IN.to_owned());
        let how = self.c.cloud_sync_subtitle(profile).unwrap_or_else(|| {
            if on {
                koru_i18n::t(crate::format::lang(), TextKey::CloudSyncOn).to_owned()
            } else {
                NOT_SYNCING.to_owned()
            }
        });
        format!("{who} \u{b7} {how}")
    }

    // ---- input ---------------------------------------------------------------

    /// What a control of this page does. `None` for one that is not.
    pub(super) fn activate_cloud_control(&mut self, control: Control) -> Option<bool> {
        let intent = match control {
            Control::Accounts => {
                self.open_account_switcher();
                return Some(true);
            }
            Control::Unreachable(index) => {
                let action = *self.c.account_wide_surface()?.actions.get(index)?;
                Intent::AccountWideAction(action)
            }
            Control::DifferentCreate => {
                self.perform(Intent::CreateWithSignedInAccount);
                // The create form is the New profile page's.
                self.show(Page::NewProfile);
                return Some(true);
            }
            Control::DifferentSignIn => Intent::SignInAnotherAccount,
            Control::DifferentOffline => Intent::DismissDifferentAccount,
            Control::CloudPrimary => Intent::CloudPrimary,
            Control::CloudLearnMore => Intent::CloudLearnMore,
            // D258's question comes up from the controller when this turns
            // the Profile's sync on, as it does from the Profile's page.
            Control::ProfileSync(profile) => Intent::CloudSync(profile),
            // Bring the account's Profile to this computer — what the nav's
            // *Connect* did. It then syncs by itself
            // (ADR-000-createdwithaccount).
            Control::ProfileBring(profile) => {
                if self.c.connect_label(profile).is_none() {
                    return Some(false);
                }
                Intent::Connect(profile)
            }
            Control::ProfileSignIn(profile) => {
                self.show(Page::Profile(profile));
                self.reveal_sign_in();
                return Some(true);
            }
            Control::Progress(profile) => {
                self.disclosed = match self.disclosed {
                    Some(open) if open == profile => None,
                    _ => Some(profile),
                };
                self.sync();
                return Some(true);
            }
            Control::ProgressRow(workspace) => Intent::ToggleProgress(workspace),
            _ => return None,
        };
        self.perform(intent);
        Some(true)
    }

    /// Bring a Profile page's *Sign-in and sync* into view — where *Sign in…*
    /// on Koru Cloud lands.
    fn reveal_sign_in(&mut self) {
        if let Some(section) = &self.laid.sign_in {
            self.page_scroll = (section.rect.y - 16.0).max(0.0);
            self.clamp_scroll();
        }
    }

    // ---- paint ---------------------------------------------------------------

    pub(super) fn paint_cloud_page(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(laid) = self.laid.cloud.clone() else {
            return;
        };
        let column = self.column();
        let counts = self.c.cloud_profile_counts();
        let _page = inspect::scope(
            "Manage Koru Cloud",
            "part",
            root,
            scene::rect(column.x, column.y, column.w, self.laid.height),
            |r| {
                r.property(
                    "status",
                    format!("{:?}", self.c.cloud_status()),
                    "controller",
                );
                r.property(
                    "syncing",
                    counts.map_or_else(|| "unknown".to_owned(), |(on, of)| format!("{on} of {of}")),
                    "cloud_profile_counts",
                );
                r.property(
                    "progress",
                    self.disclosed
                        .map_or_else(|| "closed".to_owned(), |id| id.to_storage()),
                    "the open disclosure (decision 20)",
                );
                r.property(
                    "polling",
                    self.c.maintenance_deadline().is_some(),
                    "only while this page is in front",
                );
                r.property(
                    "implementation",
                    "apps/koru manage::cloud",
                    "app implementation",
                );
                r.property("docs", "docs/product/INITIATIVES.md", "initiative");
            },
        );
        if let Some((x, y)) = laid.dot {
            scene::fill(canvas, root, dot_on(), &scene::circle(x, y, DOT_R));
        }
        self.paint_head(canvas, root, &laid.head);
        if let Some(words) = &laid.unreachable {
            self.paint_attention(
                canvas,
                root,
                words,
                "Manage can't connect",
                "account-wide (decision 3)",
            );
        }
        self.paint_account(canvas, root, &laid);
        if let Some(words) = &laid.different {
            self.paint_attention(
                canvas,
                root,
                words,
                "Manage different account",
                "another account's Profiles (decision 4)",
            );
        }
        self.paint_subscription(canvas, root, &laid.subscription);
        self.paint_sync_section(canvas, root, &laid);
    }

    fn paint_attention(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        words: &Words,
        name: &'static str,
        about: &'static str,
    ) {
        let r = words.rect;
        let _card = inspect::scope(name, "part", root, scene::rect(r.x, r.y, r.w, r.h), |rec| {
            rec.property("card", "attention", "manage::cloud");
            rec.property("about", about, "controller");
        });
        let _box = Card::new(None).draw(canvas, root, &mut self.engine, r, &[], 1.0);
        let laid = self.laid.clone();
        self.paint_words(canvas, root, words, &laid);
    }

    fn paint_account(&mut self, canvas: &mut Scene, root: Affine, laid: &CloudLaid) {
        let r = laid.account_card;
        let _accounts = inspect::scope(
            "Manage accounts",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property(
                    "stage",
                    format!("{:?}", self.c.account().stage),
                    "AccountState",
                );
                rec.property(
                    "held accounts",
                    self.c.account().held_accounts.len(),
                    "AccountState",
                );
                rec.property("opens", "the account switcher", "AccountSwitcher");
            },
        );
        let _card =
            Card::new(Some(ACCOUNT_TITLE)).draw(canvas, root, &mut self.engine, r, &[], 1.0);
        let row = &laid.account;
        let _row = CardRow::new(&row.label).hint(&row.hint).draw(
            canvas,
            root,
            &mut self.engine,
            row.rect,
            row.text_w,
            1.0,
        );
        if let Some(placed) = self.laid.placed(Control::Accounts).cloned() {
            self.paint_control(canvas, root, &placed);
        }
    }

    /// The lead's action, and — while a subscription is offered — the
    /// features card. The lead itself is the page head's.
    fn paint_subscription(&mut self, canvas: &mut Scene, root: Affine, block: &Subscription) {
        let p = theme::p();
        let top = block
            .text
            .as_ref()
            .map(|(rect, _)| rect.y)
            .or(block.card.map(|card| card.y))
            .unwrap_or_default();
        let bottom = block
            .card
            .map(|card| card.y + card.h)
            .or(block.text.as_ref().map(|(rect, _)| rect.y + rect.h))
            .unwrap_or(top);
        let column = self.column();
        let _block = inspect::scope(
            "Manage subscription",
            "part",
            root,
            scene::rect(column.x, top, column.w, (bottom - top).max(0.0)),
            |rec| {
                rec.property("features", block.bullets.len(), "FEATURES");
                rec.property("storage bar", false, "decision 7");
            },
        );
        if let Some(card) = block.card {
            let _card = Card::new(Some(FEATURES_TITLE)).draw(
                canvas,
                root,
                &mut self.engine,
                card,
                &[],
                1.0,
            );
            for (rect, text) in &block.bullets {
                let line = theme::role::meta().metrics.line_height;
                scene::fill(
                    canvas,
                    root,
                    p.text_dim,
                    &scene::circle(rect.x - BULLET_INDENT + 5.0, rect.y + line * 0.5, 2.0),
                );
                KoruTextParagraph::new(
                    text,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim,
                )
                .draw(canvas, root, &mut self.engine, *rect);
            }
        }
        for control in [Control::CloudPrimary, Control::CloudLearnMore] {
            if let Some(placed) = self.laid.placed(control).cloned() {
                self.paint_control(canvas, root, &placed);
            }
        }
    }

    fn paint_sync_section(&mut self, canvas: &mut Scene, root: Affine, laid: &CloudLaid) {
        let p = theme::p();
        let r = laid.sync_card;
        let _section = inspect::scope(
            "Manage profiles sync",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("rows", laid.rows.len(), "the local Profiles");
                rec.property(
                    "not on this computer",
                    laid.elsewhere.len(),
                    "missing_account_profiles",
                );
                rec.property(
                    "switch",
                    "Intent::CloudSync (D258 on turning on)",
                    "controller",
                );
            },
        );
        let dividers: Vec<f32> = laid
            .rows
            .iter()
            .chain(&laid.elsewhere)
            .skip(1)
            .map(|row| row.rect.y)
            .collect();
        let _card =
            Card::new(Some(SYNC_TITLE)).draw(canvas, root, &mut self.engine, r, &dividers, 1.0);
        if let Some(rect) = laid.empty {
            let line = theme::role::ui().metrics.line_height;
            KoruTextParagraph::new(
                NO_PROFILES,
                theme::role::ui(),
                "theme::role::ui()",
                p.text_dim,
            )
            .draw(
                canvas,
                root,
                &mut self.engine,
                Rect::new(
                    rect.x + CardRow::PAD_X,
                    rect.y + (rect.h - line) * 0.5,
                    rect.w - CardRow::PAD_X * 2.0,
                    line,
                ),
            );
        }
        let profiles = self.c.profiles().to_vec();
        for row in &laid.rows {
            let Some(profile) = profiles.iter().find(|profile| profile.id == row.profile) else {
                continue;
            };
            let tile = Tile {
                icon: profile.icon.as_deref(),
                color: profile.color.as_deref(),
                name: &profile.name,
            };
            self.paint_sync_row(canvas, root, row, tile);
        }
        // Not on this computer: no icon or color of its own is known here,
        // so the tile is the name's.
        for row in &laid.elsewhere {
            let tile = Tile {
                icon: None,
                color: None,
                name: &row.name_text,
            };
            self.paint_sync_row(canvas, root, row, tile);
        }
    }

    fn paint_sync_row(&mut self, canvas: &mut Scene, root: Affine, row: &SyncRow, tile: Tile<'_>) {
        let p = theme::p();
        let r = row.rect;
        let _row = inspect::scope(
            "Manage sync row",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property(
                    "switch",
                    match row.toggle {
                        Some((_, _, true)) => "enabled",
                        Some(_) => "disabled",
                        None => "Sign in…",
                    },
                    "cloud_sync_availability",
                );
                rec.property("progress open", row.progress.is_some(), "decision 20");
            },
        );
        tile.draw(canvas, root, &mut self.engine, row.tile);
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &row.name_text,
            row.name.x,
            row.name.y,
            theme::role::row(),
            "theme::role::row()",
            p.text,
        );
        KoruTextParagraph::new(
            &row.sub_text,
            theme::role::meta(),
            "theme::role::meta() — today's Cloud vocabulary",
            p.text_dim,
        )
        .draw(canvas, root, &mut self.engine, row.sub);
        // A disabled switch is drawn but is not a control: it is never a
        // keyboard stop, and the sub-line says why (decision 8's words).
        if let Some((toggle, rect, false)) = row.toggle {
            toggle.draw(
                canvas,
                root,
                &mut self.engine,
                rect,
                crate::format::lang().is_rtl(),
                false,
                false,
            );
        }
        for control in [
            Control::ProfileSync(row.profile),
            Control::ProfileBring(row.profile),
            Control::ProfileSignIn(row.profile),
            Control::Progress(row.profile),
        ] {
            if let Some(placed) = self.laid.placed(control).cloned() {
                self.paint_control(canvas, root, &placed);
            }
        }
        if let Some(progress) = &row.progress {
            self.paint_progress(canvas, root, progress);
        }
    }
}
