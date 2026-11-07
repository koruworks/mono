// SPDX-License-Identifier: MPL-2.0

//! The composer's element layout. The editor is a mutable embedded document,
//! so its host paints at the rectangle resolved by this frame.

use std::cell::RefCell;

use koru_draw::el::{
    Action, El, Frame, FrameCx, Key, Length, Role, TextCache, boxed, col, row, spacer,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::tokens::Space;

use crate::{BUTTON_H, COMPOSER_PAD_TOP, COMPOSER_TEXT_GAP};

pub(crate) const PLUS: Key = Key::surface("KoruComposer", "add-context");
pub(crate) const SEND: Key = Key::surface("KoruComposer", "send");

pub(crate) struct Shell {
    pub(crate) tree: El,
    pub(crate) frame: Frame,
}

impl Shell {
    pub(crate) fn text(&self) -> Rect {
        self.frame.rect_of("ComposerText").expect("composer text")
    }

    pub(crate) fn toolbar(&self) -> Rect {
        self.frame
            .rect_of("ComposerToolbar")
            .expect("composer toolbar")
    }

    pub(crate) fn plus(&self) -> Rect {
        self.frame
            .rect_of("ComposerAddContext")
            .expect("composer plus")
    }

    pub(crate) fn send(&self) -> Rect {
        self.frame.rect_of("ComposerSend").expect("composer send")
    }
}

thread_local! {
    static LAYOUT: RefCell<Option<(TextEngine, TextCache)>> = const { RefCell::new(None) };
}

pub(crate) fn layout(
    rect: Rect,
    band_height: f32,
    text_height: f32,
    plus_name: &str,
    send_name: &str,
) -> Shell {
    let text_region = COMPOSER_PAD_TOP + text_height + COMPOSER_TEXT_GAP;
    let tree = col("KoruComposer").child(
        col("ComposerBox")
            .pad_bottom(Space::S2)
            .child(
                boxed("ComposerContextBand").h(Length::Runtime(band_height, "measured chip band")),
            )
            .child(boxed("ComposerText").h(Length::Runtime(text_region, "editor lines and insets")))
            .child(
                row("ComposerToolbar")
                    .h(Length::Runtime(BUTTON_H, "action button height"))
                    .pad_x(Space::S4)
                    .child(
                        boxed("ComposerAddContext")
                            .w(Length::Runtime(BUTTON_H, "action button height"))
                            .h(Length::Runtime(BUTTON_H, "action button height"))
                            .id(PLUS)
                            .hit()
                            .focusable()
                            .action(Action::Click)
                            .semantics(Role::Button)
                            .name_text(plus_name),
                    )
                    .child(spacer())
                    .child(
                        boxed("ComposerSend")
                            .w(Length::Runtime(BUTTON_H, "action button height"))
                            .h(Length::Runtime(BUTTON_H, "action button height"))
                            .id(SEND)
                            .hit()
                            .focusable()
                            .action(Action::Click)
                            .semantics(Role::Button)
                            .name_text(send_name),
                    ),
            ),
    );
    let frame = LAYOUT.with_borrow_mut(|slot| {
        let (engine, cache) = slot.get_or_insert_with(|| (TextEngine::new(), TextCache::default()));
        tree.layout(engine, cache, rect, FrameCx::root())
    });
    Shell { tree, frame }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_parts_share_frame_geometry_and_ancestry() {
        let rect = Rect::new(10.0, 20.0, 360.0, 130.0);
        let shell = layout(rect, 46.0, 28.0, "Add context", "Send");
        assert!(
            shell.frame.diagnostics.is_empty(),
            "{:?}",
            shell.frame.diagnostics
        );
        assert_eq!(
            shell
                .frame
                .hit_at(shell.plus().x + 1.0, shell.plus().y + 1.0),
            Some(PLUS)
        );
        assert_eq!(
            shell
                .frame
                .hit_at(shell.send().x + 1.0, shell.send().y + 1.0),
            Some(SEND)
        );
        let text = shell.text();
        let toolbar = shell.toolbar();
        assert_eq!(text.y + text.h, toolbar.y);
        assert_eq!(shell.send().x + shell.send().w, rect.x + rect.w - 16.0);
        let text_node = shell.frame.named("ComposerText").next().unwrap();
        let box_node = shell.frame.named("ComposerBox").next().unwrap();
        let mut parent = text_node.parent;
        assert_eq!(
            parent.map(|at| shell.frame.nodes[at].name),
            Some(box_node.name)
        );
        parent = shell.frame.nodes[parent.unwrap()].parent;
        assert_eq!(
            parent.map(|at| shell.frame.nodes[at].name),
            Some("KoruComposer")
        );
    }
}
