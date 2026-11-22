// SPDX-License-Identifier: MPL-2.0

//! **The first-run welcome choice modal** (Screen 1): centered floating card
//! over the live workspace shell offering three starting paths — *Continue
//! offline*, *Create a Koru account* (7-day free trial with Monthly / Yearly
//! billing toggle, Yearly selected by default), and *I already have an
//! account*.
//!
//! Geometry comes from one element tree laid out per frame; drawing,
//! hit-testing, and accessibility all read `frame.node(key).rect`.

use super::setup::leaves::{ButtonLeaf, ButtonStyle};
use super::*;

pub(super) mod leaves;
pub(super) mod tokens;

use koru_draw::el::{
    Align, El, Frame, FrameCx, Interaction, Key as ElKey, Length, Look, Ring, Spacing, StateRecipe,
    col, leaf, row, spacer, text,
};
use koru_draw::theme::{Ink, Line, Radius, Space, Surface, roles};
use koru_i18n::{Key as T, t};

use leaves::{
    BadgeLeaf, BadgeTone, BillingSegmentLeaf, CheckCircleLeaf, LinkLeaf, LogoLeaf,
    ProviderButtonLeaf, ProviderKind,
};
use tokens::*;

const SURFACE: &str = "welcome-modal";

pub(super) const KEY_CARD: ElKey = ElKey::surface(SURFACE, "card");
pub(super) const KEY_TITLE: ElKey = ElKey::surface(SURFACE, "title");
pub(super) const KEY_LEAD: ElKey = ElKey::surface(SURFACE, "lead");
pub(super) const KEY_COMPARE_PLANS: ElKey = ElKey::surface(SURFACE, "compare-plans");
pub(super) const KEY_OFFLINE_CARD: ElKey = ElKey::surface(SURFACE, "offline-card");
pub(super) const KEY_ACCOUNT_CARD: ElKey = ElKey::surface(SURFACE, "account-card");
pub(super) const KEY_BILLING_TRACK: ElKey = ElKey::surface(SURFACE, "billing-track");
pub(super) const KEY_BILLING_MONTHLY: ElKey = ElKey::surface(SURFACE, "billing-monthly");
pub(super) const KEY_BILLING_YEARLY: ElKey = ElKey::surface(SURFACE, "billing-yearly");
pub(super) const KEY_EXISTING_CARD: ElKey = ElKey::surface(SURFACE, "existing-card");
pub(super) const KEY_CONTINUE: ElKey = ElKey::surface(SURFACE, "continue");
pub(super) const KEY_GOOGLE: ElKey = ElKey::surface(SURFACE, "google");
pub(super) const KEY_APPLE: ElKey = ElKey::surface(SURFACE, "apple");
pub(super) const KEY_EMAIL_LINK: ElKey = ElKey::surface(SURFACE, "email-link");
pub(super) const KEY_STATUS: ElKey = ElKey::surface(SURFACE, "status");
pub(super) const KEY_RECOVERY: ElKey = ElKey::surface(SURFACE, "recovery");

const WINDOW: &str = "Welcome modal window";
const CARD: &str = "Welcome modal card";

static ROW_STATES: StateRecipe = StateRecipe {
    name: "welcome choice row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Hover),
    selected_pressed: Look::bg(Surface::Hover),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

fn hit_for_key(key: ElKey) -> Option<Hit> {
    Some(match key {
        k if k == KEY_COMPARE_PLANS => Hit::ComparePlans,
        k if k == KEY_OFFLINE_CARD => Hit::WelcomeOffline,
        k if k == KEY_ACCOUNT_CARD => Hit::WelcomeAccount,
        k if k == KEY_BILLING_MONTHLY => Hit::BillingMonthly,
        k if k == KEY_BILLING_YEARLY => Hit::BillingYearly,
        k if k == KEY_EXISTING_CARD => Hit::WelcomeExisting,
        k if k == KEY_CONTINUE => Hit::Primary,
        k if k == KEY_GOOGLE => Hit::Secondary,
        k if k == KEY_APPLE => Hit::Apple,
        k if k == KEY_EMAIL_LINK => Hit::ContinueWithEmail,
        k if k == KEY_RECOVERY => Hit::Recovery,
        _ => return None,
    })
}

pub(super) fn key_for_hit(hit: Hit) -> Option<ElKey> {
    Some(match hit {
        Hit::ComparePlans => KEY_COMPARE_PLANS,
        Hit::WelcomeOffline => KEY_OFFLINE_CARD,
        Hit::WelcomeAccount => KEY_ACCOUNT_CARD,
        Hit::BillingMonthly => KEY_BILLING_MONTHLY,
        Hit::BillingYearly => KEY_BILLING_YEARLY,
        Hit::WelcomeExisting => KEY_EXISTING_CARD,
        Hit::Primary => KEY_CONTINUE,
        Hit::Secondary => KEY_GOOGLE,
        Hit::Apple => KEY_APPLE,
        Hit::ContinueWithEmail => KEY_EMAIL_LINK,
        Hit::Recovery => KEY_RECOVERY,
        _ => return None,
    })
}

impl OnboardingView {
    pub(super) fn welcome_modal_controls(&self) -> Vec<Hit> {
        let mut controls = vec![
            Hit::ComparePlans,
            Hit::WelcomeOffline,
            Hit::WelcomeAccount,
        ];
        if self.welcome_path == WelcomePath::CreateAccount {
            controls.extend([Hit::BillingMonthly, Hit::BillingYearly]);
        }
        controls.push(Hit::WelcomeExisting);
        match self.welcome_path {
            WelcomePath::Offline => controls.push(Hit::Primary),
            WelcomePath::CreateAccount | WelcomePath::ExistingAccount => {
                controls.extend([Hit::Secondary, Hit::Apple, Hit::ContinueWithEmail]);
            }
        }
        if self.recovery_offered() {
            controls.push(Hit::Recovery);
        }
        controls
    }

    pub(super) fn activate_welcome_modal(&mut self, hit: Hit) {
        match hit {
            Hit::ComparePlans => {
                self.pending = Some(Action::OpenPricing);
            }
            Hit::WelcomeOffline => {
                self.welcome_path = WelcomePath::Offline;
                self.error = None;
            }
            Hit::WelcomeAccount => {
                self.welcome_path = WelcomePath::CreateAccount;
                self.error = None;
            }
            Hit::BillingMonthly => {
                self.welcome_path = WelcomePath::CreateAccount;
                self.welcome_billing = WelcomeBillingInterval::Monthly;
                self.error = None;
            }
            Hit::BillingYearly => {
                self.welcome_path = WelcomePath::CreateAccount;
                self.welcome_billing = WelcomeBillingInterval::Yearly;
                self.error = None;
            }
            Hit::WelcomeExisting => {
                self.welcome_path = WelcomePath::ExistingAccount;
                self.error = None;
            }
            Hit::Primary => {
                self.telemetry.push(Telemetry::EntryOffline);
                self.modal_account_flow = false;
                self.show_setup();
            }
            Hit::Secondary => {
                self.modal_account_flow = true;
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryGoogle);
                self.begin_browser_signin(BrowserProvider::Google);
            }
            Hit::Apple => {
                self.modal_account_flow = true;
                if self.reopen_account_creation() {
                    return;
                }
                self.telemetry.push(Telemetry::EntryApple);
                self.begin_browser_signin(BrowserProvider::Apple);
            }
            Hit::ContinueWithEmail => {
                self.modal_account_flow = true;
                if self.reopen_account_creation() {
                    return;
                }
                self.begin_browser_signin(BrowserProvider::Email);
            }
            Hit::Recovery if self.recovery_offered() => self.open_recovery(),
            _ => {}
        }
    }

    fn welcome_modal_tree(&mut self) -> El {
        let lang = self.language;
        let controls = self.controls();
        let focused = self
            .keyboard_focus
            .then(|| controls.get(self.focus).copied())
            .flatten();
        let hover = self.hover;
        let area = self.setup_area();
        let margin = Spacing::from(WELCOME_CARD_MARGIN).px(koru_draw::theme::Profile::Desktop);
        let width = Length::Runtime(
            WELCOME_CARD_MAX_W.value.min((area.w - margin * 2.0).max(280.0)),
            "welcome modal width",
        );

        let is_offline = self.welcome_path == WelcomePath::Offline;
        let is_create = self.welcome_path == WelcomePath::CreateAccount;
        let is_existing = self.welcome_path == WelcomePath::ExistingAccount;
        let is_yearly = self.welcome_billing == WelcomeBillingInterval::Yearly;

        let compare_leaf = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalComparePlans),
            true,
            false,
            hover == Some(Hit::ComparePlans),
            focused == Some(Hit::ComparePlans),
        );
        let trial_badge = BadgeLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalPlusTrialBadge),
            BadgeTone::Teal,
        );
        let monthly_segment = BillingSegmentLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalBillingMonthly),
            None,
            !is_yearly,
            hover == Some(Hit::BillingMonthly),
            focused == Some(Hit::BillingMonthly),
        );
        let yearly_segment = BillingSegmentLeaf::new(
            &mut self.engine,
            t(lang, T::WelcomeModalBillingYearly),
            Some(t(lang, T::WelcomeModalSave16)),
            is_yearly,
            hover == Some(Hit::BillingYearly),
            focused == Some(Hit::BillingYearly),
        );
        let email_link_key = match self.welcome_path {
            WelcomePath::ExistingAccount => T::WelcomeModalEmailSignInLink,
            WelcomePath::Offline | WelcomePath::CreateAccount => T::WelcomeModalEmailLink,
        };
        let email_link_leaf = LinkLeaf::new(
            &mut self.engine,
            t(lang, email_link_key),
            false,
            true,
            hover == Some(Hit::ContinueWithEmail),
            focused == Some(Hit::ContinueWithEmail),
        );

        let header = col("Welcome header")
            .gap(WELCOME_BLOCK_GAP)
            .child(
                row("Welcome brand row")
                    .items_center()
                    .gap(WELCOME_LOGO_GAP)
                    .child(leaf("Koru logo", LogoLeaf).decorative())
                    .child(
                        text(t(lang, T::WelcomeModalTitle))
                            .named("Welcome title")
                            .role(roles::SUBTITLE)
                            .emphasis(true)
                            .id(KEY_TITLE),
                    ),
            )
            .child(
                text(t(lang, T::WelcomeModalLead))
                    .named("Welcome lead")
                    .role(roles::UI)
                    .ink(Ink::Secondary)
                    .wrap()
                    .id(KEY_LEAD),
            );

        let prompt_row = row("Start prompt row")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .child(
                text(t(lang, T::WelcomeModalStartPrompt))
                    .named("Start prompt")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .fill(),
            )
            .child(
                leaf("Compare plans", compare_leaf)
                    .semantic()
                    .id(KEY_COMPARE_PLANS)
                    .hit(),
            );

        let mut offline_card = row("Continue offline option")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .pad_x(WELCOME_OPTION_PAD_X)
            .pad_y(WELCOME_OPTION_PAD_Y)
            .radius(Radius::L)
            .states(&ROW_STATES)
            .id(KEY_OFFLINE_CARD)
            .hit()
            .focusable()
            .child(
                col("Offline copy")
                    .fill()
                    .gap(WELCOME_OPTION_TEXT_GAP)
                    .child(
                        text(t(lang, T::WelcomeModalOfflineTitle))
                            .role(roles::ROW)
                            .emphasis(true),
                    )
                    .child(
                        text(t(lang, T::WelcomeModalOfflineBody))
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .wrap(),
                    ),
            )
            .child(
                text(t(lang, T::WelcomeModalFree))
                    .role(roles::META)
                    .emphasis(true),
            );
        if is_offline {
            offline_card = offline_card.child(leaf("Offline check", CheckCircleLeaf).decorative());
        }

        let price_key = if is_yearly {
            T::WelcomeModalPlusPriceYearly
        } else {
            T::WelcomeModalPlusPrice
        };
        let mut account_top = row("Account option header")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .child(
                col("Account copy")
                    .fill()
                    .gap(WELCOME_OPTION_TEXT_GAP)
                    .child(
                        row("Account title row")
                            .items_center()
                            .gap(Space::S2)
                            .child(
                                text(t(lang, T::WelcomeModalAccountTitle))
                                    .role(roles::ROW)
                                    .emphasis(true),
                            )
                            .child(leaf("Trial badge", trial_badge).decorative()),
                    )
                    .child(
                        text(t(lang, T::WelcomeModalAccountBody))
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .wrap(),
                    ),
            )
            .child(text(t(lang, price_key)).role(roles::META).emphasis(true));
        if is_create {
            account_top = account_top.child(leaf("Account check", CheckCircleLeaf).decorative());
        }

        let billing_track = row("Billing toggle track")
            .id(KEY_BILLING_TRACK)
            .items_center()
            .pad(Space::S1)
            .gap(Space::S1)
            .child(
                leaf("Monthly billing", monthly_segment)
                    .semantic()
                    .id(KEY_BILLING_MONTHLY)
                    .hit(),
            )
            .child(
                leaf("Yearly billing", yearly_segment)
                    .semantic()
                    .id(KEY_BILLING_YEARLY)
                    .hit(),
            );
        let mut billing_row = row("Billing interval row")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .child(billing_track);
        if is_yearly {
            billing_row = billing_row.child(spacer()).child(
                text(t(lang, T::WelcomeModalYearlyPerMonth))
                    .role(roles::META)
                    .ink(Ink::Secondary),
            );
        }

        let mut account_card = col("Create account option")
            .gap(WELCOME_BLOCK_GAP)
            .pad_x(WELCOME_OPTION_PAD_X)
            .pad_y(WELCOME_OPTION_PAD_Y)
            .radius(Radius::L)
            .states(&ROW_STATES)
            .id(KEY_ACCOUNT_CARD)
            .hit()
            .focusable()
            .child(account_top);
        if is_create {
            account_card = account_card.child(billing_row);
        }

        let mut existing_card = row("Existing account option")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .pad_x(WELCOME_OPTION_PAD_X)
            .pad_y(WELCOME_OPTION_PAD_Y)
            .radius(Radius::L)
            .states(&ROW_STATES)
            .id(KEY_EXISTING_CARD)
            .hit()
            .focusable()
            .child(
                col("Existing account copy")
                    .fill()
                    .gap(WELCOME_OPTION_TEXT_GAP)
                    .child(
                        text(t(lang, T::WelcomeModalExistingTitle))
                            .role(roles::ROW)
                            .emphasis(true),
                    )
                    .child(
                        text(t(lang, T::WelcomeModalExistingBody))
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .wrap(),
                    ),
            );
        if is_existing {
            existing_card =
                existing_card.child(leaf("Existing account check", CheckCircleLeaf).decorative());
        }

        let choices = col("Welcome choices")
            .gap(WELCOME_BLOCK_GAP)
            .child(prompt_row)
            .child(offline_card)
            .child(account_card)
            .child(existing_card);

        let mut footer = col("Welcome footer").gap(WELCOME_BLOCK_GAP);
        if let Some(error) = self.error.clone() {
            footer = footer.child(
                text(error)
                    .named("Welcome status")
                    .role(roles::META)
                    .ink(Ink::Notice)
                    .wrap()
                    .id(KEY_STATUS),
            );
        }
        if is_offline {
            footer = footer.child(
                leaf(
                    "Continue offline",
                    ButtonLeaf {
                        label: t(lang, T::Continue).to_owned(),
                        width: 0.0,
                        style: ButtonStyle::Primary,
                        hovered: hover == Some(Hit::Primary),
                        focused: focused == Some(Hit::Primary),
                        disabled: false,
                    },
                )
                .semantic()
                .id(KEY_CONTINUE)
                .hit(),
            );
        } else {
            let lead_key = if is_existing {
                T::WelcomeModalSignInLeadExisting
            } else {
                T::WelcomeModalSignInLeadPlus
            };
            let providers = row("Sign-in providers")
                .gap(WELCOME_PROVIDER_GAP)
                .child(
                    leaf(
                        "Google",
                        ProviderButtonLeaf {
                            kind: ProviderKind::Google,
                            label: t(lang, T::WelcomeModalGoogle).to_owned(),
                            hovered: hover == Some(Hit::Secondary),
                            focused: focused == Some(Hit::Secondary),
                        },
                    )
                    .semantic()
                    .fill()
                    .id(KEY_GOOGLE)
                    .hit(),
                )
                .child(
                    leaf(
                        "Apple",
                        ProviderButtonLeaf {
                            kind: ProviderKind::Apple,
                            label: t(lang, T::WelcomeModalApple).to_owned(),
                            hovered: hover == Some(Hit::Apple),
                            focused: focused == Some(Hit::Apple),
                        },
                    )
                    .semantic()
                    .fill()
                    .id(KEY_APPLE)
                    .hit(),
                );
            footer = footer
                .child(
                    text(t(lang, lead_key))
                        .role(roles::META)
                        .ink(Ink::Secondary),
                )
                .child(providers)
                .child(
                    leaf("Continue with email", email_link_leaf)
                        .semantic()
                        .align_self(Align::Centre)
                        .id(KEY_EMAIL_LINK)
                        .hit(),
                );
        }

        let footnote_key = match self.welcome_path {
            WelcomePath::Offline => T::WelcomeModalFootOffline,
            WelcomePath::CreateAccount if is_yearly => T::WelcomeModalFootPlusYearly,
            WelcomePath::CreateAccount => T::WelcomeModalFootPlus,
            WelcomePath::ExistingAccount => T::WelcomeModalFootExisting,
        };
        footer = footer.child(
            text(t(lang, footnote_key))
                .named("Welcome footnote")
                .role(roles::META)
                .ink(Ink::Secondary)
                .wrap()
                .align_self(Align::Centre),
        );

        if self.recovery_offered() {
            footer = footer.child(
                text(t(lang, T::OnboardingRecoveryLink))
                    .named("Recovery link")
                    .role(roles::META)
                    .ink(Ink::Accent)
                    .wrap()
                    .align_self(Align::Centre)
                    .radius(Radius::M)
                    .states(&ROW_STATES)
                    .id(KEY_RECOVERY)
                    .hit()
                    .focusable(),
            );
        }

        let card = col(CARD)
            .id(KEY_CARD)
            .w(width)
            .max_w(WELCOME_CARD_MAX_W)
            .min_h(WELCOME_MODAL_MIN_H)
            .pad(WELCOME_CARD_PAD)
            .gap(WELCOME_SECTION_GAP)
            .child(header)
            .child(choices)
            .child(spacer())
            .child(footer);

        col(WINDOW)
            .items_center()
            .pad_x(WELCOME_CARD_MARGIN)
            .child(spacer())
            .child(card)
            .child(spacer())
    }

    pub(super) fn welcome_modal_frame(&mut self) -> (El, Frame) {
        let tree = self.welcome_modal_tree();
        let controls = self.controls();
        let interaction = Interaction {
            hot: self.hover.and_then(key_for_hit),
            pressed: None,
            focused: self
                .keyboard_focus
                .then(|| controls.get(self.focus).copied().and_then(key_for_hit))
                .flatten(),
        };
        let area = self.setup_area();
        let frame = tree.layout(
            &mut self.engine,
            &mut self.setup_cache,
            area,
            FrameCx::root().with_interaction(interaction),
        );
        (tree, frame)
    }

    pub(super) fn welcome_modal_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let (_, frame) = self.welcome_modal_frame();
        let key = frame.hit_at(x, y)?;
        hit_for_key(key)
    }

    pub(super) fn draw_welcome_modal(&mut self, canvas: &mut Scene, root: Affine) {
        let (tree, frame) = self.welcome_modal_frame();
        let Some(card) = frame.node(KEY_CARD).map(|node| node.rect) else {
            return;
        };
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            card.x,
            card.y,
            card.w,
            card.h,
            &scene::Panel::popover(WELCOME_CARD_RADIUS.value),
        );
        let p = theme::p();
        if let Some(track) = frame.node(KEY_BILLING_TRACK).map(|node| node.rect) {
            scene::fill(
                canvas,
                root,
                p.code_bg,
                &scene::rrect(
                    track.x,
                    track.y,
                    track.w,
                    track.h,
                    WELCOME_SEGMENT_TRACK_RADIUS.value,
                ),
            );
        }
        frame.paint(&tree, canvas, root, &mut self.engine);
        for (key, selected) in [
            (KEY_OFFLINE_CARD, self.welcome_path == WelcomePath::Offline),
            (
                KEY_ACCOUNT_CARD,
                self.welcome_path == WelcomePath::CreateAccount,
            ),
            (
                KEY_EXISTING_CARD,
                self.welcome_path == WelcomePath::ExistingAccount,
            ),
        ] {
            if let Some(rect) = frame.node(key).map(|node| node.rect) {
                let stroke = if selected { p.text } else { p.panel_border };
                scene::stroke(
                    canvas,
                    root,
                    stroke,
                    WELCOME_OPTION_BORDER_W,
                    &scene::rrect(
                        rect.x,
                        rect.y,
                        rect.w,
                        rect.h,
                        WELCOME_OPTION_RADIUS.value,
                    ),
                );
            }
        }
    }
}
