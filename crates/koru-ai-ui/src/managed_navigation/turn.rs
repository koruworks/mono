// SPDX-License-Identifier: MPL-2.0

//! The work, answer and exchange share the bounds of their originating turn.
//! The existing painters run inside that measured parent, rather than in
//! unrelated transcript-wide passes that flatten ownership in the inspector.
use super::*;

impl ChatSurface {
    pub(crate) fn conversation_turn_rect(&self, index: usize, area: Rect) -> Rect {
        let message = &self.message_layouts[index];
        let mut top = message.top;
        let mut bottom = top + self.shown_message_height(message);
        if let Some(id) = message.id {
            for (start, end) in self
                .work_layouts
                .iter()
                .filter(|work| work.message == id)
                .map(|work| (work.top, work.top + work.height))
                .chain(
                    self.notice_layouts
                        .iter()
                        .filter(|notice| notice.message == id)
                        .map(|notice| (notice.top, notice.top + notice.layout.height())),
                )
                .chain(self.navigation_turn_bounds(id))
            {
                top = top.min(start);
                bottom = bottom.max(end);
            }
        }
        Rect::new(
            area.x,
            area.y - self.conversation_scroll + top,
            area.w,
            bottom - top,
        )
    }

    pub(crate) fn draw_turn_work(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        area: Rect,
        message: Option<MessageId>,
    ) {
        let focused = self.work_focus();
        let belongs = |id| match message {
            Some(expected) => id == expected,
            None => !self
                .message_layouts
                .iter()
                .any(|message| message.id == Some(id)),
        };
        let now_ms = koru_ai_core::Timestamp::now().millis();
        let sweep = (!self.reduced_motion).then(|| chat_status::sweep_phase(now_ms));
        let images = &self.images;
        let mut picture = |canvas: &mut Scene, root: Affine, key: &str, rect: Rect| {
            if let Some(loaded) = images.get(key).and_then(|image| image.ready.as_ref()) {
                draw_image_fitted(canvas, root, loaded, rect);
            }
        };
        for notice in self
            .notice_layouts
            .iter()
            .filter(|notice| belongs(notice.message))
        {
            let y = area.y - self.conversation_scroll + notice.top;
            if y + notice.layout.height() >= area.y && y <= area.y + area.h {
                scene::draw_layout(
                    canvas,
                    root,
                    &notice.layout,
                    area.x + PAD,
                    y,
                    Some(theme::p().text_dim),
                );
            }
        }
        for work in self
            .work_layouts
            .iter()
            .filter(|work| belongs(work.message) && work.height > 0.0)
        {
            let pad = if work.bot {
                crate::bot_presentation::PROSE_PAD
            } else {
                PAD
            };
            let at = Rect::new(
                area.x + pad,
                area.y - self.conversation_scroll + work.top,
                (area.w - pad * 2.0).max(1.0),
                work.height,
            );
            if at.y + at.h < area.y || at.y > area.y + area.h {
                continue;
            }
            let _work = koru_draw::inspect::scope(
                "Turn work",
                "part",
                root,
                scene::rect(at.x, at.y, at.w, at.h),
                |_| {},
            );
            let bot = self.working_bot_for(work.message);
            work_view::draw(
                canvas,
                root,
                &mut self.engine,
                self.lang,
                work,
                at,
                self.work_hover,
                focused,
                now_ms,
                sweep,
                bot.as_ref(),
                &mut picture,
            );
        }
    }
}
