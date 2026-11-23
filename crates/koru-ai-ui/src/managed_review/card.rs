// SPDX-License-Identifier: MPL-2.0
//! The review card under the pointer and the keyboard: hover and tooltips
//! from what was drawn, the result's words selectable as a chat reply's are
//! (a drag, a double click, `Ctrl+C`, `Ctrl+L`), and the reply actions a
//! chat reply has — copy the whole result, save it as a document.
use super::*;
use parley::editing::Selection;

/// The words as last laid out, at the width they were laid out for.
pub(crate) struct WordsLayout {
    pub text: String,
    pub width: f32,
    pub layout: std::rc::Rc<Layout<Brush>>,
}

impl ChatSurface {
    /// What the card shows: the selected result, or — while the next one
    /// loads after a skip or an arrow — the one it showed last, so the card
    /// changes its content in place instead of emptying and growing back.
    pub(crate) fn review_displayed(&self) -> Option<&wire::ReviewDetail> {
        self.review.selected.as_ref().or(self.review.shown.as_ref())
    }

    /// The result's words laid out at `width`, reused while neither changes.
    pub(super) fn review_words_layout(
        &mut self,
        text: &str,
        width: f32,
    ) -> std::rc::Rc<Layout<Brush>> {
        if let Some(words) = &self.review.words
            && words.text == text
            && (words.width - width).abs() < 0.5
        {
            return words.layout.clone();
        }
        let layout = std::rc::Rc::new(chat_text_layout(&mut self.engine, text, width));
        self.review.words = Some(WordsLayout {
            text: text.to_owned(),
            width,
            layout: layout.clone(),
        });
        layout
    }

    /// The byte of the words under `(x, y)`, when the point is on them and
    /// inside the card's visible body.
    fn review_text_index(&self, x: f32, y: f32) -> Option<(usize, Selection)> {
        let at = self.review.words_at?;
        let body = self.review.body?;
        if !at.contains(x, y) || !body.contains(x, y) {
            return None;
        }
        let words = self.review.words.as_ref()?;
        let selection = Selection::from_point(&words.layout, x - at.x, y - at.y);
        Some((selection.focus().index(), selection))
    }

    pub(super) fn review_selection_range(&self) -> Option<std::ops::Range<usize>> {
        let (anchor, focus) = self.review.selection?;
        let len = self.review.words.as_ref()?.text.len();
        let range = anchor.min(focus).min(len)..anchor.max(focus).min(len);
        (!range.is_empty()).then_some(range)
    }

    /// The selected part of the result's words.
    pub(crate) fn review_selected_text(&self) -> Option<String> {
        let range = self.review_selection_range()?;
        let words = self.review.words.as_ref()?;
        words
            .text
            .get(range)
            .filter(|piece| !piece.is_empty())
            .map(str::to_owned)
    }

    pub(crate) fn clear_review_selection(&mut self) -> bool {
        self.review.selecting = false;
        let had = self.review.selection.take().is_some();
        // The pill offered for these words goes with them.
        if had && self.selected_response_text().is_none() {
            self.add_pill.hide();
        }
        had
    }

    /// Where the selection starts, on screen: its first line's top and
    /// bottom and its first character's left edge, for the *Add to chat*
    /// pill over it.
    pub(crate) fn review_selection_start(&self) -> Option<(f32, f32, f32)> {
        use parley::editing::Cursor;
        use parley::layout::Affinity;
        let range = self.review_selection_range()?;
        let words = self.review.words.as_ref()?;
        let at = self.review.words_at?;
        let caret = Cursor::from_byte_index(&words.layout, range.start, Affinity::Downstream)
            .geometry(&words.layout, 1.0);
        let top = at.y + caret.y0 as f32;
        // A first line scrolled out of the card's body offers nothing.
        if !self
            .review
            .body
            .is_some_and(|body| body.contains(at.x, top.max(body.y)))
        {
            return None;
        }
        Some((at.x + caret.x0 as f32, top, at.y + caret.y1 as f32))
    }

    /// A press in the result's words starts a selection there.
    pub(super) fn review_text_press(&mut self, x: f32, y: f32) -> bool {
        let Some((index, _)) = self.review_text_index(x, y) else {
            return false;
        };
        self.review.selection = Some((index, index));
        self.review.selecting = true;
        true
    }

    /// A double click in the result's words selects the word under it.
    pub(crate) fn review_double_click(&mut self, x: f32, y: f32) -> Option<bool> {
        if !self.review.open {
            return None;
        }
        let at = self.review.words_at?;
        let (_, _) = self.review_text_index(x, y)?;
        let words = self.review.words.as_ref()?;
        let word = Selection::word_from_point(&words.layout, x - at.x, y - at.y);
        self.review.selection = Some((word.anchor().index(), word.focus().index()));
        self.review.selecting = false;
        self.show_add_pill();
        Some(true)
    }

    /// The pointer moved: a selection being dragged follows it, and the
    /// control under it changes its hover. `true` when either repaints.
    pub(crate) fn review_pointer(&mut self, old: (f32, f32), x: f32, y: f32) -> bool {
        if !self.review.open {
            return false;
        }
        let mut changed = false;
        if self.review.selecting
            && let Some((index, _)) = self.review_text_index(x, y)
            && let Some((anchor, focus)) = self.review.selection
            && focus != index
        {
            self.review.selection = Some((anchor, index));
            changed = true;
        }
        changed || self.review_target_at(old.0, old.1) != self.review_target_at(x, y)
    }

    /// A drag in the words ended: *Add to chat* is offered over what it
    /// selected, as over a reply's selection.
    pub(crate) fn review_release(&mut self) -> bool {
        let released = std::mem::take(&mut self.review.selecting);
        if released && self.review_selected_text().is_some() {
            self.show_add_pill();
        }
        released
    }

    /// The control drawn under `(x, y)`, enabled or not.
    pub(super) fn review_target_at(&self, x: f32, y: f32) -> Option<Part> {
        self.review
            .targets
            .iter()
            .find(|(_, rect, _)| rect.contains(x, y))
            .map(|(part, _, _)| *part)
    }

    /// The tooltip of the card's icon buttons and of Skip: the name, and
    /// the shortcut on its second line where there is one.
    pub(crate) fn review_tooltip_at(&self, x: f32, y: f32) -> Option<TooltipContent> {
        let (part, rect, _) = *self
            .review
            .targets
            .iter()
            .find(|(_, rect, _)| rect.contains(x, y))?;
        let (words, shortcut) = match part {
            Part::Previous => (
                Words::BotsReviewPrevious,
                Some(koru_keys::ShortcutId::ReviewPrevious),
            ),
            Part::Next => (
                Words::BotsReviewNext,
                Some(koru_keys::ShortcutId::ReviewNext),
            ),
            Part::Close => (Words::ViewerClose, None),
            Part::Copy => (Words::AiReplyCopy, None),
            Part::Save => (Words::AiReplySave, None),
            Part::Skip => (
                Words::AiQuestionsSkip,
                Some(koru_keys::ShortcutId::ReviewSkip),
            ),
            Part::More | Part::Retry => return None,
        };
        Some(
            TooltipContent::new(
                0x7265_7669_6577_0000 | part as u64,
                rect,
                t(self.lang, words),
            )
            .with_optional_shortcut(
                shortcut.and_then(|id| koru_keys::show_hint(id, koru_keys::Section::Tooltip)),
            ),
        )
    }

    /// Copy the whole result. A result the engine sends in parts is read
    /// to its end first; the parts land in [`State::copying`].
    pub(super) fn copy_review(&mut self, bot: BotId) -> bool {
        let Some(detail) = self.review_displayed() else {
            return false;
        };
        if detail.offset == 0 && detail.next.is_none() {
            koru_ui::clipboard::put_plain(&detail.text);
            self.show_copy_feedback("Copied");
            return true;
        }
        let occurrence = detail.item.occurrence;
        if self.review.pending.is_some() && !self.review.failed {
            return false;
        }
        self.review.copying = Some((occurrence, String::new()));
        self.request_review(wire::Command::Review {
            bot,
            occurrence,
            offset: 0,
        })
    }

    /// One part of a result being copied arrived: read on, or copy.
    pub(super) fn copied_part(
        &mut self,
        bot: BotId,
        occurrence: Uuid,
        detail: wire::ReviewDetail,
    ) -> bool {
        let Some((_, mut words)) = self.review.copying.take() else {
            return false;
        };
        words.push_str(&detail.text);
        match detail.next {
            Some(next) if next > detail.offset => {
                self.review.copying = Some((occurrence, words));
                self.request_review(wire::Command::Review {
                    bot,
                    occurrence,
                    offset: next,
                })
            }
            _ => {
                koru_ui::clipboard::put_plain(&words);
                self.show_copy_feedback("Copied");
                true
            }
        }
    }

    /// Save opens the chat's own name prompt, named after the Routine.
    pub(super) fn ask_review_save_name(&mut self) -> bool {
        let Some(detail) = self.review_displayed() else {
            return false;
        };
        let (reply, name) = (detail.item.reply, detail.item.name.clone());
        self.selected_destination = 0;
        self.save_name.set_text(&mut self.engine, &name);
        self.destination_open = Some(MessageId::from_uuid(reply));
        self.focus = Focus::SaveName;
        true
    }

    /// The name prompt's Save for a Routine result: the engine's copy of
    /// the whole result becomes a page in the Bot's workspace. An uncertain
    /// answer keeps the request, so saving again under the same name finds
    /// the page instead of making a second one. `None` when `message` is
    /// not the result on the card.
    pub(crate) fn save_review_result(&mut self, message: MessageId) -> Option<bool> {
        let (bot, _, _) = self.review_subject()?;
        let detail = self.review_displayed()?;
        if detail.item.reply != message.as_uuid() {
            return None;
        }
        let occurrence = detail.item.occurrence;
        let typed = self.save_name.value().trim().to_owned();
        let title = if typed.is_empty() {
            detail.item.name.clone()
        } else {
            typed
        };
        let command = match &self.review.saving {
            Some(
                kept @ wire::Command::SaveReview {
                    occurrence: kept_occurrence,
                    title: kept_title,
                    ..
                },
            ) if *kept_occurrence == occurrence && *kept_title == title => kept.clone(),
            _ => wire::Command::SaveReview {
                id: Uuid::new_v4(),
                bot,
                occurrence,
                title,
                document: Uuid::new_v4(),
            },
        };
        self.destination_open = None;
        self.focus = Focus::Composer;
        self.review.saving = Some(command.clone());
        // Another card request is in flight: the save follows it.
        if self.review.pending.is_some() && !self.review.failed {
            self.review.save_waiting = true;
            return Some(true);
        }
        Some(self.request_review(command))
    }
}
