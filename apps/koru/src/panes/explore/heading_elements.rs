// SPDX-License-Identifier: MPL-2.0

//! The navigator heading's title and action share their element allocation.

use super::*;
use koru_draw::el::{
    Action, El, Frame, FrameCx, Interaction, Key, Role, boxed, icon, row, spacer, text,
};
use koru_draw::theme::{IconSize, Ink, Radius, Space, metrics, roles};

pub(super) const TITLE: Key = Key::surface("explore", "heading");
pub(super) const ACTIONS: Key = Key::surface("explore", "actions");

impl ExploreView {
    fn heading_tree(&self, width: f32) -> El {
        let mut heading = row("Navigator heading")
            .pad_start(theme::Tagged::new(
                "NAV_TITLE_PAD",
                Space::Px(leading_x(0.0)),
            ))
            .gap(Space::S2)
            .child(
                text(self.mode.label())
                    .id(TITLE)
                    .role(roles::PANE_TITLE)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate()
                    .semantics(Role::Heading)
                    .name_text(self.mode.label()),
            );
        if width >= metrics::RAIL_SQUARE.value + Space::S3.px(theme::Profile::Desktop)
            && (!actions::available(self.mode).is_empty() || self.mode == NavigatorMode::Chats)
        {
            let (glyph, name) = if self.mode == NavigatorMode::Chats {
                (icons::NOTE_PENCIL, koru_i18n::Key::ChatsNew)
            } else {
                (icons::DOTS_THREE, koru_i18n::Key::A11yMoreActions)
            };
            heading = heading.child(
                boxed("Navigator actions")
                    .id(ACTIONS)
                    .w(metrics::RAIL_SQUARE)
                    .h(metrics::RAIL_SQUARE)
                    .items_center()
                    .hit()
                    .focusable()
                    .semantics(Role::Button)
                    .name(name)
                    .action(Action::Click)
                    .radius(Radius::M)
                    .states(&crate::panes::sidebar::rail_elements::RAIL_SQUARE_STATES)
                    .child(spacer())
                    .child(icon(glyph, IconSize::Caption).decorative())
                    .child(spacer()),
            );
        }
        heading
    }

    pub(crate) fn with_heading_frame<R>(&self, area: Rect, read: impl FnOnce(&Frame) -> R) -> R {
        let band = self.heading_band(area);
        let tree = self.heading_tree(band.w);
        crate::input_engine::with(|engine| {
            let frame = tree.layout(
                engine,
                &mut self.heading_layout.borrow_mut(),
                band,
                FrameCx::root(),
            );
            read(&frame)
        })
    }

    pub(super) fn heading_part_rect(&self, area: Rect, key: Key) -> Rect {
        let zero = Rect::new(area.x + area.w, self.heading_band(area).y, 0.0, 0.0);
        if area.w <= 0.0 {
            return zero;
        }
        self.with_heading_frame(area, |frame| frame.node(key).map_or(zero, |node| node.rect))
    }

    pub(super) fn paint_heading_elements(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
    ) {
        if area.w <= 0.0 {
            return;
        }
        let band = self.heading_band(area);
        let tree = self.heading_tree(band.w);
        let interaction = Interaction {
            hot: self.actions_hover.then_some(ACTIONS),
            ..Interaction::default()
        };
        let frame = tree.layout(
            engine,
            &mut self.heading_layout.borrow_mut(),
            band,
            FrameCx::root().with_interaction(interaction),
        );
        frame.paint(&tree, scene, root, engine);
    }
}
