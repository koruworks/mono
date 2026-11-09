// SPDX-License-Identifier: MPL-2.0

//! The composer's element layout. The editor is a mutable embedded document,
//! so its host paints at the rectangle resolved by this frame.

use std::cell::RefCell;

use koru_draw::el::{
    Action, El, Frame, FrameCx, Key, Length, Role, TextCache, boxed, col, row, spacer,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::tokens::{Profile, Space};
use koru_ui::icon_button::Metrics;
use koru_ui::split_button::SplitButton;

use crate::{BUTTON_H, COMPOSER_PAD_TOP, COMPOSER_TEXT_GAP};

pub(crate) const PLUS: Key = Key::surface("KoruComposer", "add-context");
pub(crate) const SEND: Key = Key::surface("KoruComposer", "send");
pub(crate) const MODE: Key = Key::surface("KoruComposer", "mode");
pub(crate) const MICROPHONE: Key = Key::surface("KoruComposer", "microphone");
pub(crate) const MICROPHONE_CHOICE: Key = Key::surface("KoruComposer", "microphone-choice");

pub(crate) struct Shell {
    pub(crate) tree: El,
    pub(crate) frame: Frame,
}

impl Shell {
    pub(crate) fn text(&self) -> Rect {
        self.frame.rect_of("ComposerText").expect("composer text")
    }

    #[cfg(test)]
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

    pub(crate) fn mode(&self) -> Option<Rect> {
        self.frame.rect_of("ComposerModeChevron")
    }

    pub(crate) fn microphone(&self) -> Option<Rect> {
        self.frame.rect_of("ComposerMicrophone")
    }

    pub(crate) fn microphone_choice(&self) -> Option<Rect> {
        self.frame.rect_of("ComposerMicrophoneChoice")
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
    mode_name: Option<&str>,
    microphone_name: Option<&str>,
    microphone_choice_name: Option<&str>,
) -> Shell {
    let text_region = COMPOSER_PAD_TOP + text_height + COMPOSER_TEXT_GAP;
    let mut toolbar = row("ComposerToolbar")
        .h(Length::Runtime(BUTTON_H, "action button height"))
        .pad_x(Space::S5)
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
        .child(spacer());
    if let Some(name) = microphone_name {
        toolbar = toolbar.child(
            boxed("ComposerMicrophone")
                .w(Length::Runtime(BUTTON_H, "action button height"))
                .h(Length::Runtime(BUTTON_H, "action button height"))
                .id(MICROPHONE)
                .hit()
                .action(Action::Click)
                .semantics(Role::Button)
                .name_text(name),
        );
        if let Some(choice_name) = microphone_choice_name {
            toolbar = toolbar
                .child(boxed("ComposerMicrophoneChoiceGap").w(Length::Runtime(
                    Space::S0_5.px(Profile::Desktop),
                    "choice gap",
                )))
                .child(
                    boxed("ComposerMicrophoneChoice")
                        .w(Length::Runtime(
                            Metrics::COMPACT.control_width(),
                            "microphone choice width",
                        ))
                        .h(Length::Runtime(BUTTON_H, "action button height"))
                        .id(MICROPHONE_CHOICE)
                        .hit()
                        .action(Action::Click)
                        .semantics(Role::Button)
                        .name_text(choice_name),
                );
        }
        toolbar = toolbar.child(
            boxed("ComposerMicrophoneGap")
                .w(Length::Runtime(Space::S2.px(Profile::Desktop), "send gap")),
        );
    }
    let send = boxed("ComposerSend")
        .w(Length::Runtime(BUTTON_H, "action button height"))
        .h(Length::Runtime(BUTTON_H, "action button height"))
        .id(SEND)
        .hit()
        .focusable()
        .action(Action::Click)
        .semantics(Role::Button)
        .name_text(send_name);
    toolbar = if let Some(mode_name) = mode_name {
        toolbar.child(
            row("ComposerSplitButton")
                .w(Length::Runtime(SplitButton::W, "split button width"))
                .child(send)
                .child(
                    boxed("ComposerModeChevron")
                        .w(Length::Runtime(
                            SplitButton::MENU_W,
                            "mode menu target width",
                        ))
                        .h(Length::Runtime(BUTTON_H, "action button height"))
                        .id(MODE)
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .semantics(Role::Button)
                        .name_text(mode_name),
                ),
        )
    } else {
        toolbar.child(send)
    };
    let tree = col("KoruComposer").child(
        col("ComposerBox")
            .pad_bottom(Space::S3_5)
            .child(
                boxed("ComposerContextBand").h(Length::Runtime(band_height, "measured chip band")),
            )
            .child(boxed("ComposerText").h(Length::Runtime(text_region, "editor lines and insets")))
            .child(toolbar),
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
        let shell = layout(rect, 46.0, 28.0, "Add context", "Send", None, None, None);
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
        assert_eq!(shell.send().x + shell.send().w, rect.x + rect.w - 20.0);
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

    #[test]
    fn split_send_keeps_the_menu_target_after_the_action() {
        let shell = layout(
            Rect::new(10.0, 20.0, 360.0, 130.0),
            0.0,
            74.0,
            "Add context",
            "Send",
            Some("Approval mode"),
            None,
            None,
        );
        assert!(shell.frame.diagnostics.is_empty());
        let send = shell.send();
        let mode = shell.mode().expect("mode menu target");
        let group = shell.frame.rect_of("ComposerSplitButton").unwrap();
        assert_eq!(group.w, SplitButton::W);
        assert_eq!(send.x, group.x);
        assert_eq!(send.w, SplitButton::ACTION_W);
        assert_eq!(send.x + send.w, mode.x);
        assert_eq!(shell.frame.hit_at(mode.x + 1.0, mode.y + 1.0), Some(MODE));
    }

    #[test]
    fn microphone_and_chooser_have_their_own_space_before_send() {
        let shell = layout(
            Rect::new(10.0, 20.0, 360.0, 130.0),
            0.0,
            74.0,
            "Add context",
            "Send",
            Some("Approval mode"),
            Some("Dictate"),
            Some("Choose microphone"),
        );
        let chooser = shell.microphone_choice().unwrap();
        let microphone = shell.microphone().unwrap();
        let send = shell.send();
        assert!(microphone.x + microphone.w < chooser.x);
        assert!(chooser.x + chooser.w < send.x);
        assert_eq!(
            shell.frame.hit_at(microphone.x + 1.0, microphone.y + 1.0),
            Some(MICROPHONE)
        );
        assert_eq!(
            shell.frame.hit_at(chooser.x + 1.0, chooser.y + 1.0),
            Some(MICROPHONE_CHOICE)
        );
    }
}
