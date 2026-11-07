// SPDX-License-Identifier: MPL-2.0

//! What sits on the chat's text: the right-click menu of a reply's selection, and
//! the **Add to chat** pill over that selection (koru-ai D88).
//!
//! **A quote is the composer's.** Adding a reply's selection to the chat —
//! `Ctrl+L`, the pill, or the menu — puts it in the message as a quote chip
//! at the caret, with the caret on a new line under it for the comment
//! ([`crate::composer::ChatComposer::insert_quote`]). Several interleave
//! with their comments in order. At send each chip is a Markdown blockquote
//! before the text that follows it, so the wire contract and the durable chat
//! model need no quote-only variant; a queued, refused or recalled message
//! keeps its quotes because it keeps its composer document. The person's own
//! card shows a sent quote with the bar a reply's blockquote has
//! ([`quote_bars`]).

use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::{scene, theme, theme::role};
use koru_ui::{Anchor, Menu, MenuItem};
use parley::editing::Cursor;
use parley::layout::Affinity;
use std::borrow::Cow;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::composer::{Recalled, recalled_rows};
use crate::{ChatSurface, Focus};

/// The *Add to chat* pill: its height, its gap to the selected line, the
/// inset of its caption, and its key cap's height, inset and gap.
const ADD_H: f32 = 26.0;
const ADD_GAP: f32 = 6.0;
const ADD_PAD_X: f32 = 10.0;
const CAP_H: f32 = 18.0;
const CAP_PAD_X: f32 = 5.0;
const CAP_GAP: f32 = 8.0;
/// How far the pill keeps from the transcript's edges.
const ADD_MARGIN: f32 = 4.0;

/// The bar a quoted line wears in the person's own card: the one a reply's
/// blockquote is drawn with (`koru_markdown`).
pub(crate) const QUOTE_BAR: &str = "\u{2502} ";

#[derive(Copy, Clone)]
pub(crate) enum ResponseMenuItem {
    Copy,
    /// *Add to chat*, its caption already in the surface's language.
    AddToChat(&'static str),
}

impl MenuItem for ResponseMenuItem {
    fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(match self {
            Self::Copy => "Copy",
            Self::AddToChat(label) => label,
        })
    }

    fn icon(&self) -> Option<char> {
        Some(match self {
            Self::Copy => icons::COPY,
            Self::AddToChat(_) => icons::QUOTES,
        })
    }

    fn shortcut(&self) -> Option<String> {
        match self {
            Self::Copy => Some(koru_keys::label_of(koru_keys::ShortcutId::Copy).to_owned()),
            Self::AddToChat(_) => koru_keys::show_hint(
                koru_keys::ShortcutId::AddSelectionToChat,
                koru_keys::Section::Menu,
            ),
        }
    }
}

/// A sent message as the person's own card shows it: each blockquote's
/// lines behind [`QUOTE_BAR`], and the blank lines that set a quote off from
/// its comment dropped, so a quote sits right over what was said about it,
/// as it did in the composer. A message with no quote is returned as it is.
pub(crate) fn quote_bars(markdown: &str) -> String {
    if !markdown.lines().any(|line| line.starts_with('>')) {
        return markdown.to_owned();
    }
    let rows = recalled_rows(markdown);
    let last = rows.len().saturating_sub(1);
    rows.into_iter()
        .enumerate()
        .filter(|(index, row)| !(*index == last && *row == Recalled::Line("")))
        .map(|(_, row)| match row {
            Recalled::Line(line) => line.to_owned(),
            Recalled::Quote(passage) => passage
                .split('\n')
                .map(|line| format!("{QUOTE_BAR}{line}").trim_end().to_owned())
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The **Add to chat** pill over a reply's selection (D88): shown when a
/// selection is finished — a drag let go, a double-click, a Shift+arrow —
/// and gone when the selection is, on a scroll, on Escape, on any press
/// elsewhere, and once it has been used.
#[derive(Clone, Debug, Default)]
pub(crate) struct AddPill {
    shown: bool,
    hovered: bool,
    /// Where it was last drawn: what a press and the pointer are tested
    /// against, so what is hit is what was seen.
    rect: Option<Rect>,
}

impl AddPill {
    pub(crate) fn shown(&self) -> bool {
        self.shown
    }

    /// Where it is on screen now, if it is.
    pub(crate) fn rect(&self) -> Option<Rect> {
        self.rect.filter(|_| self.shown)
    }

    /// Hide it; whether that changed anything.
    pub(crate) fn hide(&mut self) -> bool {
        let was = self.shown;
        self.shown = false;
        self.hovered = false;
        self.rect = None;
        was
    }
}

impl ChatSurface {
    pub(crate) fn copy_response_selection(&mut self) -> bool {
        self.response_menu = None;
        self.response_menu_anchor = None;
        let Some(text) = self.selected_response_text() else {
            return true;
        };
        koru_ui::clipboard::put_plain(&text);
        self.show_copy_feedback("Copied");
        true
    }

    // ----- Add to chat ------------------------------------------------

    /// **Add the reply's selection to the message** (D88): a quote chip at
    /// the composer's caret, the caret on a new line under it, the keyboard
    /// in the composer. The selection, the pill and the menu go. `false`
    /// when nothing is selected.
    pub(crate) fn add_selection_to_chat(&mut self) -> bool {
        self.response_menu = None;
        self.response_menu_anchor = None;
        self.add_pill.hide();
        let Some(passage) = self.selected_response_text() else {
            return false;
        };
        if !self.composer.insert_quote(&passage) {
            return false;
        }
        self.response_selection = None;
        self.focus = Focus::Composer;
        self.composer.set_focused(true);
        true
    }

    /// A selection gesture ended: the pill shows if it left text selected.
    pub(crate) fn show_add_pill(&mut self) {
        let selected = self.selected_response_text().is_some();
        if selected {
            self.add_pill.shown = true;
        } else {
            self.add_pill.hide();
        }
    }

    /// **The chat takes `Ctrl+L` while *Add to chat* is offered** — a reply
    /// has a selection and the pill is up — wherever the keyboard is in this
    /// surface, the composer included (the user's call, 2026-11-03): the
    /// composer refuses alignment in a message anyway, so nothing is lost
    /// there, and only a document editor outside the chat keeps the chord
    /// as Align left (`koru_keys::ShortcutId::AddSelectionToChat`).
    pub(crate) fn add_to_chat_key_is_ours(&self) -> bool {
        self.add_pill.shown() && self.selected_response_text().is_some()
    }

    pub(crate) fn add_pill_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hovered = self.add_pill.rect().is_some_and(|rect| rect.contains(x, y));
        let changed = self.add_pill.hovered != hovered;
        self.add_pill.hovered = hovered;
        changed
    }

    /// Where the selection starts, on screen: the first selected line's top
    /// and bottom and its first character's left edge.
    fn selection_start(&self) -> Option<(f32, f32, f32)> {
        let selected = self.response_selection.as_ref()?;
        if selected.anchor == selected.focus {
            return None;
        }
        for (message, block) in self.response_fragments() {
            let Some(text) = self.response_text(message, block) else {
                continue;
            };
            let Some(range) = self.response_selection_range(message, block, text.len()) else {
                continue;
            };
            let (layout, origin) = self.response_layout(message, block)?;
            let caret = Cursor::from_byte_index(layout, range.start, Affinity::Downstream)
                .geometry(layout, 1.0);
            return Some((
                origin.0 + caret.x0 as f32,
                origin.1 + caret.y0 as f32,
                origin.1 + caret.y1 as f32,
            ));
        }
        None
    }

    /// Draw the pill while it shows: the popover's surface, hairline and
    /// lift, fully round; *Add to chat* and the chord in a key cap. Just
    /// above the selection's first line, below it where there is no room,
    /// always inside the transcript.
    pub(crate) fn draw_add_pill(&mut self, canvas: &mut Scene, root: Affine) {
        self.add_pill.rect = None;
        // Under anything that floats over the transcript it is neither drawn
        // nor pressed: the menu, a picker, a confirmation.
        let covered = self.response_menu.is_some()
            || self.showing_setup()
            || self.chat_model_open
            || self.tool_mode_open
            || self.approval_mode_open
            || self.tool_access_open
            || self.confirm.is_some()
            || self.composer.picker_open();
        if !self.add_pill.shown || covered {
            return;
        }
        let Some((left, top, bottom)) = self.selection_start() else {
            return;
        };
        let area = self.conversation_hit_rect;
        let label_text = koru_i18n::t(self.lang, koru_i18n::Key::AiAddToChat);
        let label = self.engine.simple_layout(label_text, role::meta());
        let hint = koru_keys::show_hint(
            koru_keys::ShortcutId::AddSelectionToChat,
            koru_keys::Section::Menu,
        );
        let cap = hint
            .as_deref()
            .map(|hint| self.engine.simple_layout(hint, role::hint()));
        let cap_w = cap
            .as_ref()
            .map_or(0.0, |cap| cap.width() + CAP_PAD_X * 2.0);
        let w = ADD_PAD_X * 2.0 + label.width() + if cap.is_some() { CAP_GAP + cap_w } else { 0.0 };
        let x = left.clamp(
            area.x + ADD_MARGIN,
            (area.x + area.w - w - ADD_MARGIN).max(area.x + ADD_MARGIN),
        );
        let above = top - ADD_GAP - ADD_H;
        let y = if above >= area.y + ADD_MARGIN {
            above
        } else {
            (bottom + ADD_GAP).max(area.y + ADD_MARGIN)
        };
        if y + ADD_H > area.y + area.h - ADD_MARGIN {
            return;
        }
        let rect = Rect::new(x, y, w, ADD_H);
        self.add_pill.rect = Some(rect);
        let _record = koru_draw::inspect::scope(
            "Add to chat",
            "part",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |record| record.property("name", label_text, "koru_i18n::Key::AiAddToChat"),
        );
        let p = theme::p();
        scene::floating_panel(
            canvas,
            root,
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            &scene::Panel::popover(ADD_H * 0.5),
        );
        if self.add_pill.hovered {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, ADD_H * 0.5),
            );
        }
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            rect.x + ADD_PAD_X,
            rect.y + (rect.h - label.height()) * 0.5,
            Some(p.text),
        );
        if let Some(cap) = cap {
            let cap_x = rect.x + ADD_PAD_X + label.width() + CAP_GAP;
            let cap_y = rect.y + (rect.h - CAP_H) * 0.5;
            scene::fill(
                canvas,
                root,
                p.chip_bg,
                &scene::rrect(cap_x, cap_y, cap_w, CAP_H, 4.0),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &cap,
                cap_x + CAP_PAD_X,
                cap_y + (CAP_H - cap.height()) * 0.5,
                Some(p.text_dim),
            );
        }
    }

    // ----- the reply's menu and the document pill -------------------------

    pub(crate) fn response_menu_mouse_move(
        &mut self,
        x: f32,
        y: f32,
        old_cursor: CursorIcon,
    ) -> Option<bool> {
        let viewport = self.menu_viewport();
        let menu = self.response_menu.as_mut()?;
        let next = menu.item_at(viewport, x, y);
        let changed = menu.hover != next;
        menu.hover = next;
        Some(changed || old_cursor != self.cursor_icon())
    }

    /// A press while the reply's menu is open is the menu's: a row, or a
    /// press outside that closes it.
    pub(crate) fn response_menu_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let menu = self.response_menu.as_ref()?;
        if let Some(item) = menu.item_at(self.menu_viewport(), x, y) {
            return Some(self.activate_response_menu_item(item));
        }
        self.response_menu = None;
        self.response_menu_anchor = None;
        Some(true)
    }

    /// The keys of the reply's menu, `Ctrl+L` over a selection, and Escape
    /// for the *Add to chat* pill. `None` when none of them is in play.
    pub(crate) fn quote_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if self.response_menu.is_none() {
            if koru_keys::registry::get(koru_keys::ShortcutId::AddSelectionToChat)
                .chord
                .matches(key, text, mods)
                && self.add_to_chat_key_is_ours()
            {
                return Some(self.add_selection_to_chat());
            }
            if matches!(key, Key::Named(NamedKey::Escape)) && self.add_pill.shown() {
                self.add_pill.hide();
                return Some(true);
            }
            return None;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.response_menu = None;
            self.response_menu_anchor = None;
            return Some(true);
        }
        if matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown)) {
            let delta = if matches!(key, Key::Named(NamedKey::ArrowUp)) {
                -1
            } else {
                1
            };
            if let Some(menu) = &mut self.response_menu {
                menu.step(&[true, true], delta);
            }
            return Some(true);
        }
        if matches!(key, Key::Named(NamedKey::Enter)) {
            let selected = self.response_menu.as_ref().and_then(|menu| menu.hover);
            return Some(self.activate_response_menu_item(selected.unwrap_or(0)));
        }
        Some(false)
    }

    /// Open the shared popover menu only when the press lands in the selected
    /// response text. Hosts call this from their ordinary `View::context_menu`
    /// path, so the context pane and a chat tab behave alike.
    pub fn context_menu(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if !self.response_selection_contains(x, y) {
            self.response_menu_anchor = None;
            return self.response_menu.take().is_some();
        }
        let add = koru_i18n::t(self.lang, koru_i18n::Key::AiAddToChat);
        let mut menu = Menu::aligned(
            &mut self.engine,
            (x, y),
            vec![ResponseMenuItem::Copy, ResponseMenuItem::AddToChat(add)],
            Anchor::Cursor,
        );
        menu.step(&[true, true], 1);
        self.response_menu = Some(menu);
        self.response_menu_anchor = Some(Rect::new(x, y, 1.0, 1.0));
        self.add_pill.hide();
        true
    }

    pub(crate) fn activate_response_menu_item(&mut self, item: usize) -> bool {
        match item {
            0 => self.copy_response_selection(),
            1 => {
                self.add_selection_to_chat();
                true
            }
            _ => false,
        }
    }
}

/// **Seams for a host's own suite** (D90): a host drives *Add to chat*
/// through its real key and pointer path — the window's routing, not this
/// surface's — and needs a reply on screen and a way to read the composer.
/// Only with `no-client`, which only a host's dev-dependencies turn on.
#[cfg(any(test, feature = "no-client"))]
impl ChatSurface {
    /// Put one finished reply on screen, as a loaded chat would, ready to
    /// be laid out by the host's next frame.
    #[doc(hidden)]
    pub fn show_reply_for_host_test(&mut self, reply: &str) {
        use koru_ai_core::{
            AiProviderId, Chat, ChatId, ChatRole, Message, MessageBlock, MessageBlockId,
            MessageBlockKind, MessageId, MessageState, ModelSelection, ReasoningChoice, Timestamp,
        };
        let now = Timestamp::from_millis(1);
        let chat_id = ChatId::new();
        let message_id = MessageId::new();
        self.transcript = Some(koru_ai_wire::ChatTranscript {
            sources: Vec::new(),
            chat: Chat {
                id: chat_id,
                title: "Chat".into(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).expect("a known provider"),
                    requested_model: "gpt-test".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                profile_id: None,
                created_at: now,
                updated_at: now,
            },
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            notices: Vec::new(),
            messages: vec![Message {
                id: message_id,
                chat_id,
                ordinal: 0,
                role: ChatRole::Assistant,
                state: MessageState::Complete,
                markdown: reply.into(),
                origin_message_id: None,
                provider: None,
                requested_model: None,
                resolved_model: None,
                error_class: None,
                created_at: now,
                completed_at: Some(now),
            }],
            blocks: vec![MessageBlock {
                id: MessageBlockId::new(),
                message_id,
                ordinal: 0,
                kind: MessageBlockKind::Paragraph,
                markdown: reply.into(),
                plain_text: reply.into(),
            }],
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            reasoning_control: None,
            attachments: Vec::new(),
        });
        self.validated_ready = true;
        self.setup_open = false;
    }

    /// Where byte `byte` of that reply sits on screen, half way down its
    /// line, as of the last frame. `None` before one has laid it out.
    #[doc(hidden)]
    pub fn reply_point_for_host_test(&self, byte: usize) -> Option<(f32, f32)> {
        let (layout, origin) = self.response_layout(0, Some(0))?;
        let caret =
            Cursor::from_byte_index(layout, byte, Affinity::Downstream).geometry(layout, 1.0);
        Some((
            origin.0 + caret.x0 as f32,
            origin.1 + (caret.y0 + caret.y1) as f32 * 0.5,
        ))
    }

    /// What the composer holds, quotes as their `> ` lines.
    #[doc(hidden)]
    pub fn composer_text_for_host_test(&self) -> String {
        self.composer.text()
    }
}

#[cfg(test)]
mod tests;
