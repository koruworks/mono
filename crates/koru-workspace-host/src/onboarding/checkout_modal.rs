// SPDX-License-Identifier: MPL-2.0

//! **Checkout progress and recovery modals** (between sign-in and profile
//! selection/setup):
//! - `Screen::CheckoutPending`: *"Finish checkout in your browser"* with a
//!   spinner, plan summary card, *Reopen checkout page*, and *Cancel checkout*.
//! - `Screen::CheckoutIncomplete`: *"Checkout was not completed"* with a
//!   Monthly / Yearly billing toggle, *Retry checkout*, and *Continue offline*
//!   (keeping the person signed in).

use super::setup::leaves::{ButtonLeaf, ButtonStyle};
use super::welcome_modal::leaves::{
    BillingSegmentLeaf, CardIconLeaf, LinkLeaf, SecondaryFullButtonLeaf, SpinnerLeaf,
};
use super::welcome_modal::tokens::*;
use super::*;

use koru_draw::el::{
    Align, El, Frame, FrameCx, Interaction, Key as ElKey, Length, Spacing, col, leaf, row, spacer,
    text,
};
use koru_draw::theme::{Ink, roles};
use koru_i18n::{Key as T, t};

const SURFACE: &str = "checkout-modal";

pub(super) const KEY_CARD: ElKey = ElKey::surface(SURFACE, "card");
pub(super) const KEY_TITLE: ElKey = ElKey::surface(SURFACE, "title");
pub(super) const KEY_BODY: ElKey = ElKey::surface(SURFACE, "body");
pub(super) const KEY_SUMMARY: ElKey = ElKey::surface(SURFACE, "summary");
pub(super) const KEY_BILLING_TRACK: ElKey = ElKey::surface(SURFACE, "billing-track");
pub(super) const KEY_BILLING_MONTHLY: ElKey = ElKey::surface(SURFACE, "billing-monthly");
pub(super) const KEY_BILLING_YEARLY: ElKey = ElKey::surface(SURFACE, "billing-yearly");
pub(super) const KEY_REOPEN: ElKey = ElKey::surface(SURFACE, "reopen");
pub(super) const KEY_CANCEL: ElKey = ElKey::surface(SURFACE, "cancel");
pub(super) const KEY_RETRY: ElKey = ElKey::surface(SURFACE, "retry");
pub(super) const KEY_OFFLINE: ElKey = ElKey::surface(SURFACE, "offline");
pub(super) const KEY_BACK: ElKey = ElKey::surface(SURFACE, "back");

const WINDOW: &str = "Checkout modal window";
const CARD: &str = "Checkout modal card";

fn hit_for_key(key: ElKey) -> Option<Hit> {
    Some(match key {
        k if k == KEY_BILLING_MONTHLY => Hit::BillingMonthly,
        k if k == KEY_BILLING_YEARLY => Hit::BillingYearly,
        k if k == KEY_REOPEN => Hit::ReopenCheckout,
        k if k == KEY_CANCEL => Hit::CancelCheckout,
        k if k == KEY_RETRY => Hit::RetryCheckout,
        k if k == KEY_OFFLINE => Hit::ContinueOffline,
        k if k == KEY_BACK => Hit::Back,
        _ => return None,
    })
}

pub(super) fn key_for_hit(hit: Hit) -> Option<ElKey> {
    Some(match hit {
        Hit::BillingMonthly => KEY_BILLING_MONTHLY,
        Hit::BillingYearly => KEY_BILLING_YEARLY,
        Hit::ReopenCheckout => KEY_REOPEN,
        Hit::CancelCheckout => KEY_CANCEL,
        Hit::RetryCheckout => KEY_RETRY,
        Hit::ContinueOffline => KEY_OFFLINE,
        Hit::Back => KEY_BACK,
        _ => return None,
    })
}

impl OnboardingView {
    pub fn show_checkout_pending(&mut self) {
        self.screen = Screen::CheckoutPending;
        self.keyboard_focus = false;
        self.focus = 0;
        self.error = None;
    }

    pub fn show_checkout_incomplete(&mut self) {
        self.screen = Screen::CheckoutIncomplete;
        self.keyboard_focus = false;
        self.focus = 0;
        self.error = None;
    }

    pub(crate) fn is_checkout_pending(&self) -> bool {
        self.screen == Screen::CheckoutPending
    }

    #[cfg(test)]
    pub(crate) fn checkout_plan(&self) -> &'static str {
        self.welcome_billing.plan_code()
    }

    pub(super) fn checkout_modal_controls(&self) -> Vec<Hit> {
        match self.screen {
            Screen::CheckoutPending => vec![Hit::ReopenCheckout, Hit::CancelCheckout],
            Screen::CheckoutIncomplete => vec![
                Hit::Back,
                Hit::BillingMonthly,
                Hit::BillingYearly,
                Hit::RetryCheckout,
                Hit::ContinueOffline,
            ],
            _ => Vec::new(),
        }
    }

    pub(super) fn activate_checkout_modal(&mut self, hit: Hit) {
        match hit {
            Hit::Back => {
                if self.workspace_modal && self.welcome_signed_in {
                    self.show_welcome_signed_in();
                } else {
                    self.modal_account_flow = false;
                    self.show_entry();
                }
            }
            Hit::BillingMonthly => {
                self.welcome_billing = WelcomeBillingInterval::Monthly;
            }
            Hit::BillingYearly => {
                self.welcome_billing = WelcomeBillingInterval::Yearly;
            }
            Hit::ReopenCheckout => {
                self.pending = Some(Action::StartCheckout {
                    plan: self.welcome_billing.plan_code(),
                });
            }
            Hit::CancelCheckout => {
                self.show_checkout_incomplete();
                self.pending = Some(Action::CancelCheckout);
            }
            Hit::RetryCheckout => {
                self.show_checkout_pending();
                self.pending = Some(Action::StartCheckout {
                    plan: self.welcome_billing.plan_code(),
                });
            }
            Hit::ContinueOffline => {
                // Keep the signed-in session in the agent so the person can
                // start their trial later from Settings. When the unified
                // welcome form already holds both names, create and open the
                // space right away; otherwise return to the signed-in form with
                // Cloud toggled off.
                if self.workspace_modal {
                    self.welcome_cloud_enabled = false;
                    self.welcome_path = WelcomePath::Offline;
                    if self.setup_complete() {
                        self.finish_welcome_profile_creation(true);
                    } else {
                        self.show_welcome_signed_in();
                    }
                } else {
                    self.show_setup();
                }
            }
            _ => {}
        }
    }

    fn checkout_modal_tree(&mut self) -> El {
        let lang = self.language;
        let pending = self.screen == Screen::CheckoutPending;
        let is_yearly = self.welcome_billing == WelcomeBillingInterval::Yearly;
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
            "checkout modal width",
        );

        let price_key = if is_yearly {
            T::WelcomeModalPlusPriceYearly
        } else {
            T::WelcomeModalPlusPrice
        };
        let terms_key = if is_yearly {
            T::CheckoutTrialTermsYearly
        } else {
            T::CheckoutTrialTermsMonthly
        };

        let mut plan_col = col("Plan identity")
            .fill()
            .gap(WELCOME_OPTION_TEXT_GAP)
            .child(
                text(t(lang, T::CheckoutPlanName))
                    .role(roles::ROW)
                    .emphasis(true),
            );
        if let Some(email) = self
            .account_email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            plan_col = plan_col.child(
                text(email.to_owned())
                    .role(roles::META)
                    .ink(Ink::Secondary),
            );
        }

        let summary_top = row("Summary top")
            .items_center()
            .gap(WELCOME_ROW_GAP)
            .child(plan_col)
            .child(text(t(lang, price_key)).role(roles::ROW).emphasis(true));

        let mut summary = col("Checkout summary")
            .id(KEY_SUMMARY)
            .pad_x(WELCOME_OPTION_PAD_X)
            .pad_y(WELCOME_OPTION_PAD_Y)
            .gap(WELCOME_BLOCK_GAP)
            .child(summary_top);

        if !pending {
            let monthly_segment = BillingSegmentLeaf::new(
                &mut self.engine,
                t(lang, T::WelcomeModalBillingMonthly),
                None,
                !is_yearly,
                hover == Some(Hit::BillingMonthly),
                focused == Some(Hit::BillingMonthly),
                false,
            );
            let yearly_segment = BillingSegmentLeaf::new(
                &mut self.engine,
                t(lang, T::WelcomeModalBillingYearly),
                Some(t(lang, T::WelcomeModalSave16)),
                is_yearly,
                hover == Some(Hit::BillingYearly),
                focused == Some(Hit::BillingYearly),
                false,
            );
            let billing_track = row("Billing toggle track")
                .id(KEY_BILLING_TRACK)
                .items_center()
                .gap(WELCOME_SEGMENT_GAP)
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
            summary = summary.child(row("Billing toggle row").child(billing_track));
        }

        summary = summary.child(
            text(t(lang, terms_key))
                .role(roles::META)
                .ink(Ink::Secondary)
                .wrap(),
        );

        let top_row = if pending {
            let angle = self.blink_origin.elapsed().as_secs_f64() * 5.0;
            leaf("Checkout spinner", SpinnerLeaf { angle_rad: angle })
                .decorative()
                .align_self(Align::Start)
        } else {
            let back_link = LinkLeaf::new(
                &mut self.engine,
                t(lang, T::Back),
                false,
                false,
                hover == Some(Hit::Back),
                focused == Some(Hit::Back),
            );
            row("Checkout top row")
                .items_center()
                .child(leaf("Checkout card icon", CardIconLeaf).decorative())
                .child(spacer())
                .child(leaf("Back", back_link).semantic().id(KEY_BACK).hit())
        };

        let header = col("Checkout header")
            .gap(WELCOME_BLOCK_GAP)
            .child(top_row)
            .child(
                text(t(
                    lang,
                    if pending {
                        T::CheckoutPendingTitle
                    } else {
                        T::CheckoutIncompleteTitle
                    },
                ))
                .named("Checkout title")
                .role(roles::SUBTITLE)
                .emphasis(true)
                .wrap()
                .id(KEY_TITLE),
            )
            .child(
                text(t(
                    lang,
                    if pending {
                        T::CheckoutPendingBody
                    } else {
                        T::CheckoutIncompleteBody
                    },
                ))
                .named("Checkout body")
                .role(roles::UI)
                .ink(Ink::Secondary)
                .wrap()
                .id(KEY_BODY),
            );

        let mut actions = col("Checkout actions").gap(WELCOME_BLOCK_GAP);
        if pending {
            let cancel_link = LinkLeaf::new(
                &mut self.engine,
                t(lang, T::CheckoutCancelLink),
                false,
                true,
                hover == Some(Hit::CancelCheckout),
                focused == Some(Hit::CancelCheckout),
            );
            actions = actions
                .child(
                    leaf(
                        "Reopen checkout page",
                        SecondaryFullButtonLeaf {
                            label: t(lang, T::CheckoutReopenButton).to_owned(),
                            external_icon: false,
                            hovered: hover == Some(Hit::ReopenCheckout),
                            focused: focused == Some(Hit::ReopenCheckout),
                        },
                    )
                    .semantic()
                    .id(KEY_REOPEN)
                    .hit(),
                )
                .child(
                    leaf("Cancel checkout", cancel_link)
                        .semantic()
                        .align_self(Align::Centre)
                        .id(KEY_CANCEL)
                        .hit(),
                );
        } else {
            actions = actions
                .child(
                    leaf(
                        "Retry checkout",
                        ButtonLeaf {
                            label: t(lang, T::CheckoutRetryButton).to_owned(),
                            width: 0.0,
                            style: ButtonStyle::Primary,
                            hovered: hover == Some(Hit::RetryCheckout),
                            focused: focused == Some(Hit::RetryCheckout),
                            disabled: false,
                        },
                    )
                    .semantic()
                    .id(KEY_RETRY)
                    .hit(),
                )
                .child(
                    leaf(
                        "Continue offline",
                        SecondaryFullButtonLeaf {
                            label: t(lang, T::CheckoutContinueOffline).to_owned(),
                            external_icon: false,
                            hovered: hover == Some(Hit::ContinueOffline),
                            focused: focused == Some(Hit::ContinueOffline),
                        },
                    )
                    .semantic()
                    .id(KEY_OFFLINE)
                    .hit(),
                )
                .child(
                    text(t(lang, T::CheckoutIncompleteFoot))
                        .named("Checkout footnote")
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .wrap()
                        .align_self(Align::Centre),
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
            .child(summary)
            .child(spacer())
            .child(actions);

        col(WINDOW)
            .items_center()
            .pad_x(WELCOME_CARD_MARGIN)
            .child(spacer())
            .child(card)
            .child(spacer())
    }

    pub(super) fn checkout_modal_frame(&mut self) -> (El, Frame) {
        let tree = self.checkout_modal_tree();
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

    pub(super) fn checkout_modal_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let (_, frame) = self.checkout_modal_frame();
        let key = frame.hit_at(x, y)?;
        hit_for_key(key)
    }

    pub(super) fn draw_checkout_modal(&mut self, canvas: &mut Scene, root: Affine) {
        let (tree, frame) = self.checkout_modal_frame();
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
        frame.paint(&tree, canvas, root, &mut self.engine);
        if let Some(summary) = frame.node(KEY_SUMMARY).map(|node| node.rect) {
            scene::stroke(
                canvas,
                root,
                p.panel_border,
                1.0,
                &scene::rrect(
                    summary.x,
                    summary.y,
                    summary.w,
                    summary.h,
                    WELCOME_OPTION_RADIUS.value,
                ),
            );
        }
    }
}
