// SPDX-License-Identifier: MPL-2.0

//! **Browser sign-in waiting modal** (`Screen::Browser`): centered card with
//! `[Koru] • • • [Google | Apple | Koru]`, *Continue in your browser*,
//! provider-specific copy, an animated *Waiting for …* pill, *Open the browser
//! again*, *Cancel*, and *Browser did not open? Copy the sign-in link*.

use super::welcome_modal::leaves::{
    BrowserConnectBadgesLeaf, CenteredTextLeaf, LinkLeaf, SecondaryFullButtonLeaf, WaitingPillLeaf,
};
use super::welcome_modal::tokens::*;
use super::*;

use koru_draw::el::{
    Align, El, Frame, FrameCx, Interaction, Key as ElKey, Length, Spacing, col, leaf, row, spacer,
    text,
};
use koru_draw::theme::{Ink, Space, roles};
use koru_i18n::{Key as T, t};

const SURFACE: &str = "browser-modal";

pub(super) const KEY_CARD: ElKey = ElKey::surface(SURFACE, "card");
pub(super) const KEY_TITLE: ElKey = ElKey::surface(SURFACE, "title");
pub(super) const KEY_BODY: ElKey = ElKey::surface(SURFACE, "body");
pub(super) const KEY_REOPEN: ElKey = ElKey::surface(SURFACE, "reopen");
pub(super) const KEY_CANCEL: ElKey = ElKey::surface(SURFACE, "cancel");
pub(super) const KEY_COPY: ElKey = ElKey::surface(SURFACE, "copy");

const WINDOW: &str = "Browser sign-in window";
const CARD: &str = "Browser sign-in card";

/// How long *Sign-in link copied* replaces *Copy the sign-in link* after a click.
const LINK_COPIED_FLASH: std::time::Duration = std::time::Duration::from_millis(2500);

fn hit_for_key(key: ElKey) -> Option<Hit> {
    Some(match key {
        k if k == KEY_REOPEN => Hit::ReopenBrowser,
        k if k == KEY_CANCEL => Hit::Secondary,
        k if k == KEY_COPY => Hit::CopyBrowserLink,
        _ => return None,
    })
}

pub(super) fn key_for_hit(hit: Hit) -> Option<ElKey> {
    Some(match hit {
        Hit::ReopenBrowser => KEY_REOPEN,
        Hit::Secondary => KEY_CANCEL,
        Hit::CopyBrowserLink => KEY_COPY,
        _ => return None,
    })
}

impl OnboardingView {
    pub(crate) fn is_browser_waiting(&self) -> bool {
        self.screen == Screen::Browser
    }

    pub(crate) fn mark_browser_link_copied(&mut self) {
        self.browser_link_copied_at = Some(std::time::Instant::now());
    }

    fn browser_link_recently_copied(&self) -> bool {
        self.browser_link_copied_at
            .is_some_and(|at| at.elapsed() < LINK_COPIED_FLASH)
    }

    pub(super) fn browser_modal_controls(&self) -> Vec<Hit> {
        vec![Hit::ReopenBrowser, Hit::Secondary, Hit::CopyBrowserLink]
    }

    pub(super) fn activate_browser_modal(&mut self, hit: Hit) {
        match hit {
            Hit::ReopenBrowser => {
                self.pending = Some(Action::ReopenBrowserSignin);
            }
            Hit::Secondary => self.cancel_browser_signin(),
            Hit::CopyBrowserLink => {
                self.pending = Some(Action::CopyBrowserSigninLink);
            }
            _ => {}
        }
    }

    fn browser_modal_tree(&mut self) -> El {
        let lang = self.language;
        let provider = self.browser_provider;
        let controls = self.controls();
        let focused = self
            .keyboard_focus
            .then(|| controls.get(self.focus).copied())
            .flatten();
        let hover = self.hover;
        let area = self.setup_area();
        let margin = Spacing::from(WELCOME_CARD_MARGIN).px(koru_draw::theme::Profile::Desktop);
        let card_w = WELCOME_CARD_MAX_W.value.min((area.w - margin * 2.0).max(280.0));
        let width = Length::Runtime(card_w, "browser sign-in modal width");
        let pad_px = Spacing::from(WELCOME_CARD_PAD).px(koru_draw::theme::Profile::Desktop);
        let body_max_w = BROWSER_BODY_MAX_W.value.min((card_w - pad_px * 2.0).max(220.0));

        let (body_key, waiting_key) = match provider {
            BrowserProvider::Google => (T::BrowserSigninBodyGoogle, T::BrowserSigninWaitingGoogle),
            BrowserProvider::Apple => (T::BrowserSigninBodyApple, T::BrowserSigninWaitingApple),
            BrowserProvider::Email => (T::BrowserSigninBodyEmail, T::BrowserSigninWaitingEmail),
        };

        let title_style = theme::TextStyle::new(
            theme::role::subtitle().metrics,
            theme::FontWeight::SemiBold,
        );
        let title_leaf = CenteredTextLeaf::new(
            &mut self.engine,
            t(lang, T::BrowserSigninTitle),
            title_style,
            false,
            body_max_w,
        );
        let body_leaf = CenteredTextLeaf::new(
            &mut self.engine,
            t(lang, body_key),
            theme::role::row(),
            true,
            body_max_w,
        );
        let angle = self.blink_origin.elapsed().as_secs_f64() * 5.0;
        let waiting_pill = WaitingPillLeaf::new(&mut self.engine, t(lang, waiting_key), angle);
        let cancel_link = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::BrowserSigninCancelLink),
            false,
            true,
            hover == Some(Hit::Secondary),
            focused == Some(Hit::Secondary),
        );
        let copy_label = t(
            lang,
            if self.browser_link_recently_copied() {
                T::BrowserSigninLinkCopied
            } else {
                T::BrowserSigninCopyLink
            },
        );
        let copy_link = LinkLeaf::new(
            &mut self.engine,
            copy_label,
            false,
            true,
            hover == Some(Hit::CopyBrowserLink),
            focused == Some(Hit::CopyBrowserLink),
        );

        let header = col("Browser sign-in header")
            .items_center()
            .gap(WELCOME_BLOCK_GAP)
            .child(
                leaf("Browser badges", BrowserConnectBadgesLeaf { provider })
                    .decorative()
                    .align_self(Align::Centre),
            )
            .child(spacer().h(Space::S1))
            .child(
                leaf("Browser title", title_leaf)
                    .decorative()
                    .id(KEY_TITLE)
                    .align_self(Align::Centre),
            )
            .child(
                leaf("Browser body", body_leaf)
                    .decorative()
                    .id(KEY_BODY)
                    .align_self(Align::Centre),
            );

        let status = leaf("Waiting pill", waiting_pill)
            .decorative()
            .align_self(Align::Centre);

        let actions = col("Browser sign-in actions")
            .w(Length::Runtime(body_max_w, "browser sign-in actions width"))
            .align_self(Align::Centre)
            .gap(Space::S2_5)
            .child(
                leaf(
                    "Open the browser again",
                    SecondaryFullButtonLeaf {
                        label: t(lang, T::BrowserSigninReopenButton).to_owned(),
                        external_icon: true,
                        hovered: hover == Some(Hit::ReopenBrowser),
                        focused: focused == Some(Hit::ReopenBrowser),
                    },
                )
                .semantic()
                .id(KEY_REOPEN)
                .hit(),
            )
            .child(
                leaf("Cancel", cancel_link)
                    .semantic()
                    .align_self(Align::Centre)
                    .id(KEY_CANCEL)
                    .hit(),
            );

        let footer = row("Browser sign-in footer")
            .items_center()
            .gap(BROWSER_FOOTER_GAP)
            .align_self(Align::Centre)
            .child(
                text(t(lang, T::BrowserSigninDidNotOpen))
                    .role(roles::META)
                    .ink(Ink::Secondary),
            )
            .child(
                leaf("Copy the sign-in link", copy_link)
                    .semantic()
                    .id(KEY_COPY)
                    .hit(),
            );

        let mut card = col(CARD)
            .id(KEY_CARD)
            .w(width)
            .max_w(WELCOME_CARD_MAX_W)
            .pad(WELCOME_CARD_PAD)
            .gap(Space::S4);
        if self.workspace_modal {
            card = card.min_h(WELCOME_MODAL_MIN_H);
        }
        card = card
            .child(spacer())
            .child(header)
            .child(status)
            .child(actions)
            .child(footer)
            .child(spacer());

        col(WINDOW)
            .items_center()
            .pad_x(WELCOME_CARD_MARGIN)
            .child(spacer())
            .child(card)
            .child(spacer())
    }

    pub(super) fn browser_modal_frame(&mut self) -> (El, Frame) {
        let tree = self.browser_modal_tree();
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

    pub(super) fn browser_modal_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let (_, frame) = self.browser_modal_frame();
        let key = frame.hit_at(x, y)?;
        hit_for_key(key)
    }

    pub(super) fn draw_browser_modal(&mut self, canvas: &mut Scene, root: Affine) {
        let (tree, frame) = self.browser_modal_frame();
        let Some(card) = frame.node(KEY_CARD).map(|node| node.rect) else {
            return;
        };
        let _panel = self.workspace_modal.then(|| {
            scene::floating_panel_scope(
                canvas,
                root,
                card.x,
                card.y,
                card.w,
                card.h,
                &scene::Panel::popover(WELCOME_CARD_RADIUS.value),
            )
        });
        frame.paint(&tree, canvas, root, &mut self.engine);
    }
}
