// SPDX-License-Identifier: MPL-2.0

//! Ephemeral quote context staged above the chat composer.
//!
//! Quotes remain UI state until send. At that boundary [`compose_markdown`]
//! turns them into ordinary blockquotes, so the wire contract and durable chat
//! model need no quote-only variant.

use std::borrow::Cow;
use std::time::Instant;

use koru_ai_core::{ChatId, DocumentId};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme, theme::role};
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::tooltip::{Tooltip, TooltipContent};
use koru_ui::{Anchor, Menu, MenuItem, Typed};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::{ChatSurface, Focus};

pub(crate) const PILL_H: f32 = 28.0;
const PILL_GAP: f32 = 6.0;
const BAND_GAP: f32 = 8.0;
const PILL_RADIUS: f32 = 7.0;
const CLOSE_W: f32 = 18.0;
const ICON_SIZE: f32 = 14.0;
const EDITOR_W: f32 = 280.0;
const EDITOR_H: f32 = 86.0;
const EDITOR_PAD: f32 = 10.0;
const EDITOR_INPUT_H: f32 = 34.0;

#[derive(Copy, Clone)]
pub(crate) enum ResponseMenuItem {
    Copy,
    AddComment,
}

impl MenuItem for ResponseMenuItem {
    fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(match self {
            Self::Copy => "Copy",
            Self::AddComment => "Add comment",
        })
    }

    fn icon(&self) -> Option<char> {
        Some(match self {
            Self::Copy => icons::COPY,
            Self::AddComment => icons::QUOTES,
        })
    }

    fn shortcut(&self) -> Option<String> {
        match self {
            Self::Copy => Some(koru_keys::label_of(koru_keys::ShortcutId::Copy).to_owned()),
            Self::AddComment => None,
        }
    }
}

/// A staged question captured eagerly from ephemeral response selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingQuote {
    pub quote: String,
    pub comment: String,
}

/// Ephemeral document context staged above the chat composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextDocumentPill {
    pub document_id: DocumentId,
    pub title: String,
}

impl ContextDocumentPill {
    fn label(&self) -> String {
        elide(&self.title, 32)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PillKind {
    Quote,
}

impl PillKind {
    const fn glyph(self) -> char {
        match self {
            Self::Quote => icons::QUOTES,
        }
    }
}

impl PendingQuote {
    const fn kind(&self) -> PillKind {
        PillKind::Quote
    }

    fn label(&self) -> &str {
        if self.comment.trim().is_empty() {
            &self.quote
        } else {
            &self.comment
        }
    }
}

/// Compile staged quotes into the next ordinary user message.
pub(crate) fn compose_markdown(quotes: &[PendingQuote], typed: &str) -> String {
    let typed = typed.trim();
    if quotes.is_empty() {
        return typed.to_owned();
    }

    let mut sections = Vec::with_capacity(quotes.len() * 2 + usize::from(!typed.is_empty()));
    for quote in quotes {
        let quoted = quote
            .quote
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(quoted);
        if !quote.comment.trim().is_empty() {
            sections.push(quote.comment.trim().to_owned());
        }
    }
    if !typed.is_empty() {
        sections.push(typed.to_owned());
    }
    sections.join("\n\n")
}

/// The strip disappears from layout completely when it carries no pills.
pub(crate) fn pill_band_height(
    context_document: Option<&ContextDocumentPill>,
    quotes: &[PendingQuote],
) -> f32 {
    if context_document.is_none() && quotes.is_empty() {
        0.0
    } else {
        PILL_H + BAND_GAP
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PillPart {
    Body,
    Close,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PillTarget {
    ContextDocument,
    Quote(usize),
}

#[derive(Clone, Debug)]
struct LaidOutPill {
    target: Option<PillTarget>,
    body: Rect,
    close: Option<Rect>,
    label: String,
    hidden: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PillStrip {
    pub(crate) hovered: Option<(PillTarget, PillPart)>,
    tooltip: Tooltip,
}

impl PillStrip {
    fn layout(
        engine: &mut TextEngine,
        context_document: Option<&ContextDocumentPill>,
        quotes: &[PendingQuote],
        band: Rect,
    ) -> Vec<LaidOutPill> {
        let doc_count = usize::from(context_document.is_some());
        let total_pills = doc_count + quotes.len();
        if total_pills == 0 || band.w <= 0.0 {
            return Vec::new();
        }
        let quote_icon_w = engine.icon_layout(icons::QUOTES, ICON_SIZE).width();
        let doc_icon_w = engine.icon_layout(icons::DOCUMENT, ICON_SIZE).width();
        let mut pills = Vec::new();
        let mut x = band.x;
        if let Some(doc) = context_document {
            let label = doc.label();
            let text_w = engine.simple_layout(&label, role::ui()).width();
            let width = theme::PILL_PAD_X * 2.0 + doc_icon_w + 6.0 + text_w + CLOSE_W;
            let body = Rect::new(x, band.y, width, PILL_H);
            pills.push(LaidOutPill {
                target: Some(PillTarget::ContextDocument),
                body,
                close: Some(Rect::new(
                    body.x + body.w - CLOSE_W,
                    body.y,
                    CLOSE_W,
                    body.h,
                )),
                label,
                hidden: 0,
            });
            x += width + PILL_GAP;
        }
        for (index, quote) in quotes.iter().enumerate() {
            let label = elide(quote.label(), 32);
            let text_w = engine.simple_layout(&label, role::ui()).width();
            let width = theme::PILL_PAD_X * 2.0 + quote_icon_w + 6.0 + text_w + CLOSE_W;
            let body = Rect::new(x, band.y, width, PILL_H);
            pills.push(LaidOutPill {
                target: Some(PillTarget::Quote(index)),
                body,
                close: Some(Rect::new(
                    body.x + body.w - CLOSE_W,
                    body.y,
                    CLOSE_W,
                    body.h,
                )),
                label,
                hidden: 0,
            });
            x += width + PILL_GAP;
        }
        if pills
            .last()
            .is_some_and(|pill| pill.body.x + pill.body.w <= band.x + band.w)
        {
            return pills;
        }

        while let Some(last) = pills.last() {
            let hidden = total_pills - pills.len() + 1;
            let label = format!("+{hidden}");
            let width = engine.simple_layout(&label, role::ui()).width() + theme::PILL_PAD_X * 2.0;
            let overflow_x = if pills.len() == 1 {
                band.x
            } else {
                pills[pills.len() - 2].body.x + pills[pills.len() - 2].body.w + PILL_GAP
            };
            let _ = last;
            if overflow_x + width <= band.x + band.w || pills.len() == 1 {
                pills.pop();
                pills.push(LaidOutPill {
                    target: None,
                    body: Rect::new(overflow_x, band.y, width.min(band.w), PILL_H),
                    close: None,
                    label,
                    hidden,
                });
                return pills;
            }
            pills.pop();
        }
        Vec::new()
    }

    pub(crate) fn hit(
        &self,
        engine: &mut TextEngine,
        context_document: Option<&ContextDocumentPill>,
        quotes: &[PendingQuote],
        band: Rect,
        x: f32,
        y: f32,
    ) -> Option<(PillTarget, PillPart, Rect)> {
        Self::layout(engine, context_document, quotes, band)
            .into_iter()
            .find_map(|pill| {
                let target = pill.target?;
                if pill.close.is_some_and(|close| close.contains(x, y)) {
                    return Some((target, PillPart::Close, pill.close.expect("checked")));
                }
                pill.body
                    .contains(x, y)
                    .then_some((target, PillPart::Body, pill.body))
            })
    }

    pub(crate) fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        context_document: Option<&ContextDocumentPill>,
        quotes: &[PendingQuote],
        band: Rect,
        x: f32,
        y: f32,
        now: Instant,
    ) -> bool {
        let hit = self.hit(engine, context_document, quotes, band, x, y);
        let next = hit.map(|(target, part, _)| (target, part));
        let changed = self.hovered != next;
        self.hovered = next;
        let content = hit.map(|(target, part, rect)| {
            let (key_part, label) = match (target, part) {
                (PillTarget::ContextDocument, PillPart::Body) => (
                    0,
                    context_document
                        .map(|d| d.title.clone())
                        .unwrap_or_default(),
                ),
                (PillTarget::ContextDocument, PillPart::Close) => (1, "Remove document".to_owned()),
                (PillTarget::Quote(index), PillPart::Body) => (0, quotes[index].quote.clone()),
                (PillTarget::Quote(_), PillPart::Close) => (1, "Remove quote".to_owned()),
            };
            let id = match target {
                PillTarget::ContextDocument => 0,
                PillTarget::Quote(index) => 1 + index as u64,
            };
            TooltipContent::new(10_000 + id * 2 + key_part, rect, label)
        });
        self.tooltip.set_hover(content, now) || changed
    }

    pub(crate) fn maintenance_deadline(&self) -> Option<Instant> {
        self.tooltip.maintenance_deadline()
    }

    pub(crate) fn maintain(&mut self, now: Instant) -> bool {
        self.tooltip.maintain(now)
    }

    pub(crate) fn animating(&self, now: Instant) -> bool {
        self.tooltip.animating(now)
    }

    pub(crate) fn clear_tooltip(&mut self) {
        self.hovered = None;
        self.tooltip.clear();
    }

    pub(crate) fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        context_document: Option<&ContextDocumentPill>,
        quotes: &[PendingQuote],
        band: Rect,
        viewport: Rect,
        now: Instant,
    ) {
        let p = theme::p();
        for pill in Self::layout(engine, context_document, quotes, band) {
            scene::fill(
                canvas,
                root,
                p.chip_bg,
                &scene::rrect(
                    pill.body.x,
                    pill.body.y,
                    pill.body.w,
                    pill.body.h,
                    PILL_RADIUS,
                ),
            );
            if let Some(target) = pill.target {
                let glyph = match target {
                    PillTarget::ContextDocument => icons::DOCUMENT,
                    PillTarget::Quote(index) => quotes[index].kind().glyph(),
                };
                let icon = engine.icon_layout(glyph, ICON_SIZE);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &icon,
                    pill.body.x + theme::PILL_PAD_X,
                    pill.body.y + (pill.body.h - icon.height()) * 0.5,
                    Some(p.text_dim),
                );
                let label = engine.simple_layout(&pill.label, role::ui());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &label,
                    pill.body.x + theme::PILL_PAD_X + icon.width() + 6.0,
                    pill.body.y + (pill.body.h - label.height()) * 0.5,
                    Some(p.text),
                );
                if let Some(close_rect) = pill.close {
                    let close = engine.simple_layout("\u{00d7}", role::ui());
                    scene::draw_layout_first_line(
                        canvas,
                        root,
                        &close,
                        close_rect.x + (close_rect.w - close.width()) * 0.5,
                        close_rect.y + (close_rect.h - close.height()) * 0.5,
                        Some(p.text_dim),
                    );
                }
            } else {
                debug_assert!(pill.hidden > 0);
                let label = engine.simple_layout(&pill.label, role::ui());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &label,
                    pill.body.x + (pill.body.w - label.width()) * 0.5,
                    pill.body.y + (pill.body.h - label.height()) * 0.5,
                    Some(p.text_dim),
                );
            }
        }
        self.tooltip.draw(canvas, root, engine, viewport, now);
    }
}

pub(crate) struct CommentEditor {
    quote: String,
    input: TextInput,
    anchor: Rect,
    editing: Option<usize>,
}

impl CommentEditor {
    pub(crate) fn new(
        engine: &mut TextEngine,
        quote: String,
        comment: &str,
        anchor: Rect,
        editing: Option<usize>,
    ) -> Self {
        let mut input = TextInput::with_text(engine, role::ui(), comment);
        if editing.is_some() {
            input.select_all(engine);
        }
        Self {
            quote,
            input,
            anchor,
            editing,
        }
    }

    pub(crate) fn quote(&self) -> &str {
        &self.quote
    }

    pub(crate) fn comment(&self) -> String {
        self.input.value()
    }

    pub(crate) fn editing(&self) -> Option<usize> {
        self.editing
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    pub(crate) fn rebind(&mut self, engine: &mut TextEngine) {
        let value = self.input.raw_text().to_owned();
        self.input = TextInput::with_text(engine, role::ui(), &value);
        self.input.move_to_end(engine);
    }

    pub(crate) fn panel_rect(&self, bounds: Rect) -> Rect {
        let width = EDITOR_W.min((bounds.w - 16.0).max(120.0));
        let mut x = self.anchor.x + self.anchor.w * 0.5 - width * 0.5;
        x = x.clamp(
            bounds.x + 8.0,
            (bounds.x + bounds.w - width - 8.0).max(bounds.x + 8.0),
        );
        let below = self.anchor.y + self.anchor.h + 8.0;
        let y = if below + EDITOR_H <= bounds.y + bounds.h - 8.0 {
            below
        } else {
            (self.anchor.y - EDITOR_H - 8.0).max(bounds.y + 8.0)
        };
        Rect::new(x, y, width, EDITOR_H)
    }

    fn input_rect(&self, bounds: Rect) -> Rect {
        let panel = self.panel_rect(bounds);
        Rect::new(
            panel.x + EDITOR_PAD,
            panel.y + panel.h - EDITOR_PAD - EDITOR_INPUT_H,
            panel.w - EDITOR_PAD * 2.0,
            EDITOR_INPUT_H,
        )
    }

    pub(crate) fn click(&mut self, engine: &mut TextEngine, bounds: Rect, x: f32, y: f32) -> bool {
        self.input.click(engine, self.input_rect(bounds), x, y)
    }

    pub(crate) fn drag(&mut self, engine: &mut TextEngine, bounds: Rect, x: f32) -> bool {
        self.input.drag(engine, self.input_rect(bounds), x)
    }

    pub(crate) fn release(&mut self) {
        self.input.release();
    }

    pub(crate) fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.input.key(engine, key, text, mods) == Typed::Handled
    }

    pub(crate) fn draw(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        bounds: Rect,
    ) {
        let panel = self.panel_rect(bounds);
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let icon = engine.icon_layout(icons::QUOTES, ICON_SIZE);
        scene::draw_layout_first_line(
            canvas,
            root,
            &icon,
            panel.x + EDITOR_PAD,
            panel.y + EDITOR_PAD,
            Some(theme::p().text_dim),
        );
        let quote = engine.simple_layout(&elide(&self.quote, 44), role::meta());
        scene::draw_layout_first_line(
            canvas,
            root,
            &quote,
            panel.x + EDITOR_PAD + icon.width() + 6.0,
            panel.y + EDITOR_PAD,
            Some(theme::p().text_dim),
        );
        self.input.draw(
            canvas,
            root,
            engine,
            self.input_rect(bounds),
            InputStyle::search(),
            Some("Add a comment\u{2026}"),
            true,
        );
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

    pub(crate) fn quote_content_mouse_move(
        &mut self,
        x: f32,
        y: f32,
        old_cursor: CursorIcon,
    ) -> (Option<bool>, bool) {
        if self.comment_editor.is_some() {
            let bounds = self.conversation_rect();
            let dragged = self
                .comment_editor
                .as_mut()
                .is_some_and(|editor| editor.drag(&mut self.engine, bounds, x));
            return (Some(dragged || old_cursor != self.cursor_icon()), false);
        }
        let changed = if self.showing_setup() {
            self.pill_strip.clear_tooltip();
            false
        } else {
            let band = self.pill_band_rect();
            let context_document = self.context_document_pill();
            self.pill_strip.mouse_move(
                &mut self.engine,
                context_document.as_ref(),
                &self.pending_quotes,
                band,
                x,
                y,
                Instant::now(),
            )
        };
        (None, changed)
    }

    pub(crate) fn quote_overlay_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if self.comment_editor.is_some() {
            let bounds = self.conversation_rect();
            let panel = self
                .comment_editor
                .as_ref()
                .expect("checked above")
                .panel_rect(bounds);
            if panel.contains(x, y) {
                if let Some(editor) = &mut self.comment_editor {
                    editor.click(&mut self.engine, bounds, x, y);
                }
                return Some(true);
            }
            self.commit_comment_editor(false);
        }
        if let Some(menu) = &self.response_menu {
            if let Some(item) = menu.item_at(self.menu_viewport(), x, y) {
                return Some(self.activate_response_menu_item(item));
            }
            self.response_menu = None;
            self.response_menu_anchor = None;
            return Some(true);
        }
        None
    }

    pub(crate) fn pill_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let band = self.pill_band_rect();
        let context_document = self.context_document_pill();
        let Some((target, part, anchor)) = self.pill_strip.hit(
            &mut self.engine,
            context_document.as_ref(),
            &self.pending_quotes,
            band,
            x,
            y,
        ) else {
            return false;
        };
        self.pill_strip.clear_tooltip();
        match target {
            PillTarget::ContextDocument => {
                if part == PillPart::Close {
                    self.remove_context_document();
                }
                true
            }
            PillTarget::Quote(index) => {
                match part {
                    PillPart::Body => self.edit_pending_quote(index, anchor),
                    PillPart::Close => self.remove_pending_quote(index),
                }
                true
            }
        }
    }

    pub(crate) fn quote_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if self.comment_editor.is_some() {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.comment_editor = None;
                if self.pending_quotes.is_empty() {
                    self.staged_chat = None;
                }
                return Some(true);
            }
            if matches!(key, Key::Named(NamedKey::Enter))
                && !koru_keys::is_chord(mods)
                && !mods.alt_key()
            {
                self.commit_comment_editor(true);
                return Some(true);
            }
            return Some(
                self.comment_editor
                    .as_mut()
                    .is_some_and(|editor| editor.key(&mut self.engine, key, text, mods)),
            );
        }
        if self.response_menu.is_none() {
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

    pub(crate) fn pill_band_rect(&mut self) -> Rect {
        let composer = self.composer_rect();
        let context_document = self.context_document_pill();
        let height = pill_band_height(context_document.as_ref(), &self.pending_quotes);
        Rect::new(composer.x, composer.y - height, composer.w, height)
    }

    /// Open the shared popover menu only when the press lands in the selected
    /// response text. Hosts call this from their ordinary `View::context_menu`
    /// path, so the standalone window and workspace pane behave alike.
    pub fn context_menu(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(handled) = self.chats_pane_context_menu(x, y) {
            return handled;
        }
        if !self.response_selection_contains(x, y) {
            self.response_menu_anchor = None;
            return self.response_menu.take().is_some();
        }
        let mut menu = Menu::aligned(
            &mut self.engine,
            (x, y),
            vec![ResponseMenuItem::Copy, ResponseMenuItem::AddComment],
            Anchor::Cursor,
        );
        menu.step(&[true, true], 1);
        self.response_menu = Some(menu);
        self.response_menu_anchor = Some(Rect::new(x, y, 1.0, 1.0));
        true
    }

    pub(crate) fn activate_response_menu_item(&mut self, item: usize) -> bool {
        match item {
            0 => self.copy_response_selection(),
            1 => {
                let anchor = self
                    .response_menu_anchor
                    .take()
                    .unwrap_or_else(|| Rect::new(self.pointer.0, self.pointer.1, 1.0, 1.0));
                self.response_menu = None;
                let Some(quote) = self.selected_response_text() else {
                    return true;
                };
                self.response_selection = None;
                self.staged_chat = self
                    .transcript
                    .as_ref()
                    .map(|transcript| transcript.chat.id);
                self.comment_editor = Some(CommentEditor::new(
                    &mut self.engine,
                    quote,
                    "",
                    anchor,
                    None,
                ));
                self.focus = Focus::None;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn commit_comment_editor(&mut self, allow_empty_creation: bool) {
        let Some(editor) = self.comment_editor.take() else {
            return;
        };
        if editor.editing().is_none() && editor.is_empty() && !allow_empty_creation {
            if self.pending_quotes.is_empty() {
                self.staged_chat = None;
            }
            return;
        }
        let pending = PendingQuote {
            quote: editor.quote().to_owned(),
            comment: editor.comment(),
        };
        if let Some(index) = editor.editing() {
            if let Some(existing) = self.pending_quotes.get_mut(index) {
                *existing = pending;
            }
        } else {
            self.pending_quotes.push(pending);
        }
        self.staged_chat = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id);
    }

    pub(crate) fn edit_pending_quote(&mut self, index: usize, anchor: Rect) {
        let Some(pending) = self.pending_quotes.get(index).cloned() else {
            return;
        };
        self.comment_editor = Some(CommentEditor::new(
            &mut self.engine,
            pending.quote,
            &pending.comment,
            anchor,
            Some(index),
        ));
        self.focus = Focus::None;
    }

    pub(crate) fn remove_pending_quote(&mut self, index: usize) {
        if index < self.pending_quotes.len() {
            self.pending_quotes.remove(index);
        }
        if self.pending_quotes.is_empty() {
            self.staged_chat = None;
        }
    }

    pub(crate) fn retain_staged_quotes_for(&mut self, chat_id: ChatId) {
        if self.staged_chat.is_some_and(|staged| staged != chat_id) {
            self.pending_quotes.clear();
            self.comment_editor = None;
            self.staged_chat = None;
            self.pill_strip.clear_tooltip();
        }
    }
}

fn elide(value: &str, max_scalars: usize) -> String {
    let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = one_line.chars();
    let head = chars.by_ref().take(max_scalars).collect::<String>();
    if chars.next().is_some() {
        format!("{head}\u{2026}")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::time::Duration;

    use koru_ai_core::{
        AiProviderId, Chat, ChatId, ChatRole, DocumentId, MessageBlockId, MessageId, MessageState,
        ModelSelection, ReasoningChoice,
    };
    use koru_ai_wire::ChatTranscript;
    use koru_ui::MenuItem;
    use parley::layout::Affinity;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    use crate::reader::{Command, Reader, Update};
    use crate::{ChatSurface, ResponseMenuItem, ResponsePosition, ResponseSelection, SurfaceMode};

    fn quote(quote: &str, comment: &str) -> PendingQuote {
        PendingQuote {
            quote: quote.into(),
            comment: comment.into(),
        }
    }

    fn transcript() -> ChatTranscript {
        let now = koru_ai_core::Timestamp::from_millis(1);
        let chat_id = ChatId::new();
        ChatTranscript {
            sources: Vec::new(),
            chat: Chat {
                id: chat_id,
                title: "Test chat".into(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                    requested_model: "gpt-test".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                created_at: now,
                updated_at: now,
            },
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            reasoning_control: None,
            attachments: Vec::new(),
        }
    }

    fn assistant_selection_surface(text: &str) -> ChatSurface {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let mut transcript = transcript();
        let message_id = MessageId::new();
        transcript.messages.push(koru_ai_core::Message {
            id: message_id,
            chat_id: transcript.chat.id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: text.into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: koru_ai_core::Timestamp::from_millis(1),
            completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
        });
        transcript.blocks.push(koru_ai_core::MessageBlock {
            id: MessageBlockId::new(),
            message_id,
            ordinal: 0,
            kind: koru_ai_core::MessageBlockKind::Paragraph,
            markdown: text.into(),
            plain_text: text.into(),
        });
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.rebuild_messages(400.0);
        surface.response_selection = Some(ResponseSelection {
            anchor: ResponsePosition {
                message: 0,
                block: Some(0),
                byte: 0,
                affinity: Affinity::Downstream,
            },
            focus: ResponsePosition {
                message: 0,
                block: Some(0),
                byte: text.len(),
                affinity: Affinity::Upstream,
            },
            dragging: false,
            pressed_link: None,
        });
        surface
    }

    #[test]
    fn markdown_composition_covers_each_section_shape() {
        assert_eq!(compose_markdown(&[], "  typed only  "), "typed only");
        assert_eq!(
            compose_markdown(&[quote("first passage", "why?")], ""),
            "> first passage\n\nwhy?"
        );
        assert_eq!(
            compose_markdown(&[quote("first passage", "")], ""),
            "> first passage"
        );
        assert_eq!(
            compose_markdown(
                &[quote("first\nsecond", "question one"), quote("other", "")],
                "typed"
            ),
            "> first\n> second\n\nquestion one\n\n> other\n\ntyped"
        );
        assert_eq!(
            compose_markdown(&[quote("> literal *marks* and `code`", "")], "  "),
            "> > literal *marks* and `code`"
        );
        assert!(!compose_markdown(&[quote("sendable", "")], " ").is_empty());
    }

    #[test]
    fn empty_strip_has_no_layout_height() {
        assert_eq!(pill_band_height(None, &[]), 0.0);
        assert_eq!(
            pill_band_height(None, &[quote("one", "")]),
            PILL_H + BAND_GAP
        );
        assert_eq!(
            pill_band_height(None, &[quote("one", ""), quote("two", "")]),
            PILL_H + BAND_GAP
        );
    }

    #[test]
    fn context_document_alone_has_band_height() {
        let doc = ContextDocumentPill {
            document_id: DocumentId::new(),
            title: "Launch plan".into(),
        };
        assert_eq!(pill_band_height(Some(&doc), &[]), PILL_H + BAND_GAP);
    }

    #[test]
    fn context_document_pill_lays_out_before_quotes() {
        let mut engine = TextEngine::new();
        let doc = ContextDocumentPill {
            document_id: DocumentId::new(),
            title: "Launch plan".into(),
        };
        let quotes = [quote("first", "")];
        let layout = PillStrip::layout(
            &mut engine,
            Some(&doc),
            &quotes,
            Rect::new(0.0, 0.0, 600.0, PILL_H),
        );
        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].target, Some(PillTarget::ContextDocument));
        assert_eq!(layout[1].target, Some(PillTarget::Quote(0)));
    }

    #[test]
    fn context_document_pill_close_target_is_detected() {
        let mut engine = TextEngine::new();
        let doc = ContextDocumentPill {
            document_id: DocumentId::new(),
            title: "Launch plan".into(),
        };
        let strip = PillStrip::default();
        let band = Rect::new(0.0, 0.0, 600.0, PILL_H);
        let hit = strip
            .hit(
                &mut engine,
                Some(&doc),
                &[],
                band,
                band.x + 20.0,
                band.y + PILL_H * 0.5,
            )
            .expect("the document pill is hit");
        assert_eq!(hit.0, PillTarget::ContextDocument);
        assert_eq!(hit.1, PillPart::Body);
    }

    #[test]
    fn narrow_strip_replaces_later_pills_with_an_overflow_count() {
        let mut engine = TextEngine::new();
        let quotes = [
            quote("one", "one"),
            quote("two", "two"),
            quote("three", "three"),
            quote("four", "four"),
        ];
        let layout = PillStrip::layout(
            &mut engine,
            None,
            &quotes,
            Rect::new(0.0, 0.0, 180.0, PILL_H),
        );
        let overflow = layout.last().expect("the strip has an overflow chip");
        assert!(overflow.target.is_none());
        assert!(overflow.hidden > 0);
        assert_eq!(overflow.label, format!("+{}", overflow.hidden));
    }

    #[test]
    fn enter_can_stage_a_bare_quote_without_a_comment() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let transcript = transcript();
        surface.staged_chat = Some(transcript.chat.id);
        surface.transcript = Some(transcript);
        surface.comment_editor = Some(CommentEditor::new(
            &mut surface.engine,
            "bare quote".into(),
            "",
            Rect::new(10.0, 10.0, 1.0, 1.0),
            None,
        ));

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert_eq!(surface.pending_quotes, vec![quote("bare quote", "")]);
    }

    #[test]
    fn add_comment_dispatch_captures_selection_before_clearing_it() {
        let mut surface = assistant_selection_surface("captured immediately");
        let expected = surface.selected_response_text().unwrap();
        surface.response_menu_anchor = Some(Rect::new(30.0, 40.0, 1.0, 1.0));

        assert!(surface.activate_response_menu_item(1));

        assert_eq!(
            surface.comment_editor.as_ref().map(CommentEditor::quote),
            Some(expected.as_str())
        );
        assert!(surface.response_selection.is_none());
        assert_eq!(
            ResponseMenuItem::AddComment.label(),
            Cow::Borrowed("Add comment")
        );
    }

    #[test]
    fn comment_editor_commits_cancels_revises_and_close_removes() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let transcript = transcript();
        let chat_id = transcript.chat.id;
        surface.transcript = Some(transcript);
        surface.validated_ready = true;
        surface.staged_chat = Some(chat_id);
        surface.comment_editor = Some(CommentEditor::new(
            &mut surface.engine,
            "first quote".into(),
            "",
            Rect::new(20.0, 20.0, 1.0, 1.0),
            None,
        ));
        assert!(surface.key(
            &Key::Character("w".into()),
            Some("why?"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert_eq!(surface.pending_quotes[0].comment, "why?");

        surface.comment_editor = Some(CommentEditor::new(
            &mut surface.engine,
            "discard me".into(),
            "",
            Rect::new(20.0, 20.0, 1.0, 1.0),
            None,
        ));
        assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(),));
        assert_eq!(surface.pending_quotes.len(), 1);

        surface.edit_pending_quote(0, Rect::new(20.0, 20.0, 40.0, 20.0));
        assert!(surface.key(
            &Key::Character("r".into()),
            Some("revised"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert_eq!(surface.pending_quotes[0].comment, "revised");

        surface.composer_rect();
        let band = surface.pill_band_rect();
        let (_, _, close) = (0..band.w as usize)
            .rev()
            .find_map(|offset| {
                let context_document = surface.context_document_pill();
                surface
                    .pill_strip
                    .hit(
                        &mut surface.engine,
                        context_document.as_ref(),
                        &surface.pending_quotes,
                        band,
                        band.x + offset as f32,
                        band.y + PILL_H * 0.5,
                    )
                    .filter(|(_, part, _)| *part == PillPart::Close)
            })
            .expect("the pill has a close target");
        assert!(surface.mouse_down(close.x + close.w * 0.5, close.y + close.h * 0.5));
        assert!(surface.pending_quotes.is_empty());
        assert!(surface.staged_chat.is_none());
    }

    #[test]
    fn send_compiles_all_pills_into_the_user_message_then_clears_staging() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        let transcript = transcript();
        surface.staged_chat = Some(transcript.chat.id);
        surface.reader = Some(reader);
        surface.transcript = Some(transcript);
        surface.pending_quotes = vec![
            PendingQuote {
                quote: "first\nline two".into(),
                comment: "why?".into(),
            },
            PendingQuote {
                quote: "second".into(),
                comment: String::new(),
            },
        ];
        surface.composer.set_text("typed question");
        let expected = compose_markdown(&surface.pending_quotes, "typed question");

        assert!(surface.send_composer());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Send(request) => assert_eq!(request.markdown, expected),
            _ => panic!("composer send must issue a turn"),
        }
        assert!(surface.pending_quotes.is_empty());
        assert!(surface.staged_chat.is_none());
        assert!(surface.comment_editor.is_none());
    }

    #[test]
    fn transcript_updates_keep_same_chat_pills_and_clear_different_chat_pills() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let current = transcript();
        let chat_id = current.chat.id;
        surface.transcript = Some(current.clone());
        surface.staged_chat = Some(chat_id);
        surface.pending_quotes.push(PendingQuote {
            quote: "keep me".into(),
            comment: "question".into(),
        });
        surface.comment_editor = Some(CommentEditor::new(
            &mut surface.engine,
            "draft".into(),
            "",
            Rect::new(10.0, 10.0, 1.0, 1.0),
            None,
        ));

        surface.apply_update(Update::Transcript(current));
        assert_eq!(surface.pending_quotes.len(), 1);
        assert!(surface.comment_editor.is_some());

        surface.apply_update(Update::Transcript(transcript()));
        assert!(surface.pending_quotes.is_empty());
        assert!(surface.comment_editor.is_none());
        assert!(surface.staged_chat.is_none());
    }
}
