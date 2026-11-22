// SPDX-License-Identifier: MPL-2.0

//! Dismissible composer notices. Hiding a notice never changes request state.
use super::*;
use koru_draw::el::{Action, FrameCx, Key as ElementKey, Length, Role, TextCache, boxed, row};
use koru_draw::theme::tokens::{Profile, Space};
use koru_ui::icon_button::{self, Metrics};

const DISMISS: ElementKey = ElementKey::surface("ChatNotice", "dismiss");
const METRICS: Metrics = Metrics::COMPACT;

#[derive(Default)]
pub(super) struct Footer {
    pub(super) dismissed: Option<(Option<ChatId>, String)>,
    close: Option<(String, Rect)>,
    cache: TextCache,
}

impl ChatSurface {
    fn footer_dismissible(&self) -> bool {
        !self.stop_armed(Instant::now()) && self.footer_line().is_some()
    }

    pub(super) fn footer_dismiss_room(&self) -> f32 {
        if self.footer_dismissible() {
            METRICS.control_width() + Space::S2.px(Profile::Desktop)
        } else {
            0.0
        }
    }

    pub(super) fn footer_height(&self, text_height: f32) -> f32 {
        if self.footer_dismissible() {
            text_height.max(METRICS.control_height())
        } else {
            text_height
        }
    }

    pub(super) fn footer_dismiss_rect(&self) -> Option<Rect> {
        let (line, rect) = self.footer.close.as_ref()?;
        (self.footer_dismissible() && self.footer_line() == Some(line.as_str())).then_some(*rect)
    }

    pub(super) fn dismiss_footer(&mut self) -> bool {
        if !self.footer_dismissible() {
            return false;
        }
        self.footer.dismissed = Some((
            self.current_chat_id(),
            self.footer_line().unwrap().to_owned(),
        ));
        self.footer.close = None;
        self.stored_open_link = None;
        self.focus = Focus::Composer;
        true
    }

    pub(super) fn footer_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if self.focus == Focus::Composer
            && mods == ModifiersState::SHIFT
            && matches!(key, Key::Named(NamedKey::Tab))
            && self.footer_dismissible()
            && !self.composer.picker_open()
        {
            self.focus = Focus::DismissNotice;
            return Some(true);
        }
        if self.focus != Focus::DismissNotice {
            return None;
        }
        if mods.is_empty() && matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) {
            self.dismiss_footer();
            self.focus = Focus::Composer;
            return Some(true);
        }
        self.focus = Focus::Composer;
        if matches!(key, Key::Named(NamedKey::Tab | NamedKey::Escape)) {
            return Some(true);
        }
        None
    }

    pub(super) fn draw_footer(&mut self, canvas: &mut Scene, root: Affine, composer: Rect) {
        self.stored_open_link = None;
        self.footer.close = None;
        let content = self.content_rect();
        let Some((words, open)) = self.footer_layouts(content.w) else {
            return;
        };
        let height = self.footer_height(words.height());
        let rect = Rect::new(
            content.x,
            composer.y - COMPOSER_GAP - self.queue_band_h() - height,
            content.w,
            height,
        );
        let mut tree = row("ChatNotice")
            .gap(Space::S2)
            .child(boxed("NoticeMessage").fill());
        if self.footer_dismissible() {
            tree = tree.child(
                boxed("DismissNotice")
                    .w(Length::Runtime(
                        METRICS.control_width(),
                        "compact icon control",
                    ))
                    .h(Length::Runtime(
                        METRICS.control_height(),
                        "compact icon control",
                    ))
                    .id(DISMISS)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .semantics(Role::Button)
                    .name_text(koru_i18n::t(self.lang, koru_i18n::Key::AiDismissNotice)),
            );
        }
        let frame = tree.layout(
            &mut self.engine,
            &mut self.footer.cache,
            rect,
            FrameCx::root(),
        );
        let palette = theme::p();
        frame.paint_host(&tree, canvas, root, |node, canvas, root| match node.name {
            "NoticeMessage" => {
                scene::draw_layout(
                    canvas,
                    root,
                    &words,
                    node.rect.x,
                    node.rect.y,
                    Some(palette.text_dim),
                );
                if let Some((open, dx, dy)) = &open {
                    let (x, y) = (node.rect.x + dx, node.rect.y + dy);
                    let hit = Rect::new(x - 3.0, y - 2.0, open.width() + 6.0, open.height() + 4.0);
                    if hit.contains(self.pointer.0, self.pointer.1) {
                        scene::fill(
                            canvas,
                            root,
                            palette.hover_tint,
                            &scene::rrect(hit.x, hit.y, hit.w, hit.h, 4.0),
                        );
                    }
                    scene::draw_layout_first_line(canvas, root, open, x, y, Some(palette.accent));
                    self.stored_open_link = Some(hit);
                }
            }
            "DismissNotice" => {
                icon_button::draw_with(
                    canvas,
                    root,
                    &mut self.engine,
                    node.rect,
                    icons::WINDOW_CLOSE,
                    IconButtonState {
                        hovered: node.rect.contains(self.pointer.0, self.pointer.1),
                        ..Default::default()
                    },
                    METRICS,
                );
                if self.focus == Focus::DismissNotice {
                    scene::stroke(
                        canvas,
                        root,
                        palette.focus_ring,
                        1.0,
                        &scene::rrect(
                            node.rect.x,
                            node.rect.y,
                            node.rect.w,
                            node.rect.h,
                            METRICS.radius,
                        ),
                    );
                }
                self.footer.close = Some((self.footer_line().unwrap().to_owned(), node.rect));
            }
            _ => {}
        });
    }
}

#[cfg(test)]
mod tests;
