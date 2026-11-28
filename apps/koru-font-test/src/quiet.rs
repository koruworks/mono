// SPDX-License-Identifier: MPL-2.0

//! Quiet.md's Feishu typography on the unmodified upstream glyph painting path.
//! Metrics: https://github.com/s1dashu/quiet-md/blob/main/assets/themes/feishu.css
//! Blocks remain independent PlainEditors; this is a visual comparison fixture,
//! not a rich-text document editor. Click a block to edit it; changes are ephemeral.

use crate::{access_ids::next_node_id, text::Editor};
use accesskit::{Node, NodeId, Role, TreeUpdate};
use std::time::Instant;
use ui_events::{
    keyboard::{Key, KeyboardEvent, NamedKey},
    pointer::PointerEvent,
};
use vello::{
    Scene,
    kurbo::{Affine, Line, Rect, RoundedRect, Stroke},
    peniko::{Fill, color::AlphaColor, color::Srgb},
};
use winit::event::{MouseScrollDelta, WindowEvent};

pub const DEFAULT_TEXT_COLOR: [u8; 3] = [31, 35, 41];

/// Parley 0.11.1 stores each cluster's byte offset in a `u16`, so a single
/// shaped run past 65,535 bytes wraps `text_offset` modulo 65,536 and panics
/// in `LayoutAccessibility::build_nodes` on multi-byte UTF-8 characters.
/// Keep each block comfortably under that ceiling.
pub const MAX_BLOCK_BYTES: usize = 32_768;

pub fn split_paste_chunks(text: &str, max_bytes: usize) -> Vec<String> {
    let max_bytes = max_bytes.max(4);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut chunks = Vec::new();
    for line in normalized.split('\n') {
        if line.len() <= max_bytes {
            chunks.push(line.to_owned());
            continue;
        }
        let mut rest = line;
        while rest.len() > max_bytes {
            let mut cut = max_bytes;
            while cut > 0 && !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            if let Some(ws) = rest[..cut].rfind(char::is_whitespace)
                && ws > 0
            {
                cut = ws;
            }
            if cut == 0 {
                cut = rest.chars().next().map_or(0, char::len_utf8);
            }
            chunks.push(rest[..cut].to_owned());
            rest = rest[cut..].trim_start_matches(' ');
        }
        if !rest.is_empty() {
            chunks.push(rest.to_owned());
        }
    }
    chunks
}

pub fn parse_hex_color(input: &str) -> anyhow::Result<[u8; 3]> {
    let trimmed = input.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    let bytes = hex.as_bytes();
    let parse_nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    match bytes.len() {
        3 => {
            let r = parse_nibble(bytes[0]);
            let g = parse_nibble(bytes[1]);
            let b = parse_nibble(bytes[2]);
            match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Ok([r * 17, g * 17, b * 17]),
                _ => anyhow::bail!("Invalid hex color {input:?}. Expected #RRGGBB or #RGB."),
            }
        }
        6 => {
            let pair = |hi: u8, lo: u8| -> Option<u8> {
                Some((parse_nibble(hi)? << 4) | parse_nibble(lo)?)
            };
            match (
                pair(bytes[0], bytes[1]),
                pair(bytes[2], bytes[3]),
                pair(bytes[4], bytes[5]),
            ) {
                (Some(r), Some(g), Some(b)) => Ok([r, g, b]),
                _ => anyhow::bail!("Invalid hex color {input:?}. Expected #RRGGBB or #RGB."),
            }
        }
        _ => anyhow::bail!("Invalid hex color {input:?}. Expected #RRGGBB or #RGB."),
    }
}

pub fn format_hex_color([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn color_of([r, g, b]: [u8; 3]) -> AlphaColor<Srgb> {
    AlphaColor::from_rgb8(r, g, b)
}

struct ColorEdit {
    previous: [u8; 3],
    input: String,
}

struct Block {
    editor: Editor,
    font_size: f32,
    line_height: f32,
    weight: f32,
    gap_before: f32,
    indent: f32,
    marker: Option<&'static str>,
    id: NodeId,
    height: f32,
}

pub struct Page {
    fonts: parley::FontContext,
    blocks: Vec<Block>,
    active: usize,
    revision: u64,
    width: f32,
    height: f32,
    scale: f64,
    scroll: f32,
    content_height: f32,
    heading_weight: f32,
    hint: bool,
    aa: vello::AaConfig,
    half_pixel: bool,
    font_index: usize,
    text_color: [u8; 3],
    color_edit: Option<ColorEdit>,
    pub coverage: crate::coverage::Coverage,
}

impl Page {
    pub fn new(fonts: &parley::FontContext) -> Self {
        let mut blocks = Vec::new();
        let mut add = |text, size, line, weight, gap_before, marker: Option<&'static str>| {
            let mut editor = Editor::new(text, fonts);
            editor.configure(size, line, weight);
            blocks.push(Block {
                editor,
                font_size: size,
                line_height: line,
                weight,
                gap_before,
                indent: if marker.is_some() { 24.0 } else { 0.0 },
                marker,
                id: next_node_id(),
                height: line,
            });
        };
        add("Images, files and covers", 34.0, 55.25, 600.0, 0.0, None);
        add(
            "Sometimes an image says more than a paragraph. Add screenshots, documents and other files to your notes, then use icons and covers to make frequently used pages easy to recognize.",
            16.0,
            26.0,
            400.0,
            20.75,
            None,
        );
        add("Images", 26.0, 36.4, 600.0, 26.0, None);
        // Adjacent headings use the screenshot's compact spacing rather than
        // summing both headings' top and bottom margins.
        add("Add an image", 22.0, 30.8, 600.0, 18.0, None);
        add(
            "Choose Image from the slash menu and select a file.",
            16.0,
            26.0,
            400.0,
            8.0,
            Some("1."),
        );
        add(
            "Drag an image from File Explorer into the document.",
            16.0,
            26.0,
            400.0,
            8.0,
            Some("2."),
        );
        add(
            "Paste an image from the clipboard with Ctrl+V. Win+Shift+S captures a selection to the clipboard.",
            16.0,
            26.0,
            400.0,
            8.0,
            Some("3."),
        );
        add(
            "JPG, PNG, GIF, WebP and BMP are supported, up to 20 MB per image. The file is copied into your documents folder, so moving the original does not break the note.",
            16.0,
            26.0,
            400.0,
            8.0,
            None,
        );
        for sample in [
            "Alphabet: ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "Alphabet: abcdefghijklmnopqrstuvwxyz",
            "Digits: 0123456789 — 1,234.56 / 1.234,56",
            "Curves: a e g o c s b d p q B C D G O Q S",
            "Diagonals: A K M N V W X Y Z k v w x y z",
            "Similar shapes: Il1| O0 rn m vv w cl d",
            "German: Ä Ö Ü ä ö ü ß ẞ — Grüße aus Köln: zwölf süße Brötchen für draußen.",
            "Spanish: Á É Í Ó Ú Ü Ñ á é í ó ú ü ñ ¡ ¿ — ¡Qué alegría! ¿El pingüino tomó café esta mañana?",
            "Portuguese: À Á Â Ã Ç É Ê Í Ó Ô Õ Ú à á â ã ç é ê í ó ô õ ú — À noite, João põe açúcar no café. Você lê português?",
            "Punctuation: . , : ; ! ? ' \" ‘ ’ “ ” … - – — _ / \\ ( ) [ ] { }",
            "Symbols: @ # % & * + = < > € $ £ ° § © ® ™",
            "Precomposed / combining: ä a\u{0308} | ñ n\u{0303} | ç c\u{0327} | é e\u{0301} | ã a\u{0303} | ô o\u{0302}",
        ] {
            add(sample, 16.0, 26.0, 400.0, 16.0, None);
        }
        let mut page = Self {
            fonts: fonts.clone(),
            blocks,
            active: 1,
            revision: 1,
            width: 1028.0,
            height: 760.0,
            scale: 1.0,
            scroll: 0.0,
            content_height: 0.0,
            heading_weight: 600.0,
            hint: true,
            aa: vello::AaConfig::Area,
            half_pixel: false,
            font_index: 0,
            text_color: DEFAULT_TEXT_COLOR,
            color_edit: None,
            coverage: crate::coverage::Coverage::default(),
        };
        page.reflow();
        page
    }

    fn reflow(&mut self) {
        let page_width = (self.width - 64.0).clamp(100.0, 820.0);
        let left = ((self.width - page_width) * 0.5).max(24.0);
        let mut y = 74.0;
        for block in &mut self.blocks {
            y += block.gap_before;
            let shift = if self.half_pixel {
                (0.5 / self.scale) as f32
            } else {
                0.0
            };
            block.editor.origin = (left + block.indent + shift, y - self.scroll + shift);
            block.editor.scale = self.scale;
            block.editor.hint = self.hint;
            block
                .editor
                .editor()
                .set_width(Some(page_width - block.indent));
            block.height = block.editor.height();
            y += block.height;
        }
        self.content_height = y + 48.0;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.scale = scale;
        self.width = (width as f64 / scale) as f32;
        self.height = (height as f64 / scale) as f32;
        self.reflow();
    }

    pub fn generation(&self) -> u64 {
        self.revision
    }

    pub fn antialiasing(&self) -> vello::AaConfig {
        self.aa
    }

    pub fn editing_color(&self) -> bool {
        self.color_edit.is_some()
    }

    pub fn set_text_color(&mut self, rgb: [u8; 3]) {
        self.text_color = rgb;
        let color = color_of(rgb);
        for block in &mut self.blocks {
            block.editor.set_color(color);
        }
        self.reflow();
    }

    fn apply_color_preview(&mut self, rgb: [u8; 3]) {
        self.text_color = rgb;
        let color = color_of(rgb);
        for block in &mut self.blocks {
            block.editor.set_color(color);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    fn color_pill_rect(&self) -> Rect {
        let w = 112.0_f64;
        let x1 = (self.width as f64 - 16.0).max(w + 16.0);
        Rect::new(x1 - w, 4.0, x1, 26.0)
    }

    fn begin_color_edit(&mut self) {
        self.color_edit = Some(ColorEdit {
            previous: self.text_color,
            input: String::new(),
        });
        self.revision = self.revision.wrapping_add(1);
    }

    fn cancel_color_edit(&mut self) {
        if let Some(edit) = self.color_edit.take() {
            self.apply_color_preview(edit.previous);
        }
    }

    fn commit_color_edit(&mut self) -> bool {
        let Some(edit) = self.color_edit.as_ref() else {
            return false;
        };
        if edit.input.is_empty() {
            self.cancel_color_edit();
            return true;
        }
        if let Ok(rgb) = parse_hex_color(&edit.input) {
            self.color_edit = None;
            self.apply_color_preview(rgb);
            return true;
        }
        false
    }

    pub fn window_title(&self) -> String {
        let color_part = match &self.color_edit {
            Some(edit) => format!("Color #{} (Enter apply, Esc cancel)", edit.input),
            None => format!("F2 {}", format_hex_color(self.text_color)),
        };
        format!(
            "{color_part} | F3/F4 font {}/{} {} | F9 gamma {:.2} | F10 cutoff {:.0}% | F11 reset | F5 weight {} | F6 hint {} | F7 {:?} | F8 offset {}px",
            self.font_index + 1,
            crate::fonts::CHOICES.len(),
            crate::fonts::CHOICES[self.font_index].label,
            self.coverage.gamma,
            self.coverage.cutoff * 100.0,
            self.heading_weight,
            if self.hint { "on" } else { "off" },
            self.aa,
            if self.half_pixel { "0.5" } else { "0" }
        )
    }

    pub fn set_font(&mut self, index: usize) {
        self.font_index = index;
        for block in &mut self.blocks {
            block.editor.set_font(crate::fonts::CHOICES[index].stack);
        }
        self.reflow();
    }
    pub fn cursor_reset(&mut self) {
        self.blocks[self.active].editor.cursor_reset();
        self.revision += 1;
    }
    pub fn disable_blink(&mut self) {
        self.blocks[self.active].editor.disable_blink();
    }
    pub fn cursor_blink(&mut self) {
        self.blocks[self.active].editor.cursor_blink();
        self.revision += 1;
    }
    pub fn next_blink_time(&self) -> Option<Instant> {
        self.blocks[self.active].editor.next_blink_time()
    }

    pub fn ime_cursor_area(&mut self) -> parley::BoundingBox {
        let block = &mut self.blocks[self.active];
        let area = block.editor.editor().ime_cursor_area();
        let (x, y) = block.editor.origin;
        parley::BoundingBox::new(
            (area.x0 + x as f64) * self.scale,
            (area.y0 + y as f64) * self.scale,
            (area.x1 + x as f64) * self.scale,
            (area.y1 + y as f64) * self.scale,
        )
    }

    pub fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let chunks = split_paste_chunks(text, MAX_BLOCK_BYTES);
        if chunks.is_empty() {
            return;
        }
        let existing_len = self.blocks[self.active].editor.raw_text_len();
        if chunks.len() == 1 && existing_len + chunks[0].len() <= MAX_BLOCK_BYTES {
            self.blocks[self.active].editor.insert_text(&chunks[0]);
            self.reflow();
            return;
        }
        let template_size = self.blocks[self.active].font_size;
        let template_line = self.blocks[self.active].line_height;
        let template_weight = self.blocks[self.active].weight;
        let template_indent = self.blocks[self.active].indent;
        let font_stack = crate::fonts::CHOICES[self.font_index].stack;
        let ink = color_of(self.text_color);

        let mut iter = chunks.into_iter();
        let mut insert_at = self.active + 1;
        if let Some(first) = iter.next() {
            if existing_len + first.len() <= MAX_BLOCK_BYTES {
                self.blocks[self.active].editor.insert_text(&first);
            } else {
                let mut ed = Editor::new(&first, &self.fonts);
                ed.set_font(font_stack);
                ed.configure(template_size, template_line, template_weight);
                ed.set_color(ink);
                self.blocks.insert(
                    insert_at,
                    Block {
                        editor: ed,
                        font_size: template_size,
                        line_height: template_line,
                        weight: template_weight,
                        gap_before: 12.0,
                        indent: template_indent,
                        marker: None,
                        id: next_node_id(),
                        height: template_line,
                    },
                );
                self.active = insert_at;
                insert_at += 1;
            }
        }
        for chunk in iter {
            let mut ed = Editor::new(&chunk, &self.fonts);
            ed.set_font(font_stack);
            ed.configure(template_size, template_line, template_weight);
            ed.set_color(ink);
            self.blocks.insert(
                insert_at,
                Block {
                    editor: ed,
                    font_size: template_size,
                    line_height: template_line,
                    weight: template_weight,
                    gap_before: 12.0,
                    indent: template_indent,
                    marker: None,
                    id: next_node_id(),
                    height: template_line,
                },
            );
            self.active = insert_at;
            insert_at += 1;
        }
        self.reflow();
    }

    fn paste_into_color_edit(&mut self, text: &str) {
        let mut changed = false;
        let preview = {
            let Some(edit) = self.color_edit.as_mut() else {
                return;
            };
            let trimmed = text.trim();
            let cleaned: String = trimmed
                .strip_prefix('#')
                .unwrap_or(trimmed)
                .chars()
                .filter(char::is_ascii_hexdigit)
                .take(6)
                .map(|c| c.to_ascii_uppercase())
                .collect();
            if !cleaned.is_empty() {
                edit.input = cleaned;
                changed = true;
            }
            if edit.input.len() == 6 {
                parse_hex_color(&edit.input).ok()
            } else {
                None
            }
        };
        if let Some(rgb) = preview {
            self.apply_color_preview(rgb);
        } else if changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn handle_keyboard_event(&mut self, event: &KeyboardEvent) {
        let action_mod = if cfg!(target_os = "macos") {
            event.modifiers.meta()
        } else {
            event.modifiers.ctrl()
        };
        if event.key == Key::Named(NamedKey::F2) {
            if event.state.is_down() {
                if self.color_edit.is_some() {
                    self.cancel_color_edit();
                } else {
                    self.begin_color_edit();
                }
            }
            return;
        }
        if self.color_edit.is_some() {
            if event.state.is_down() {
                match &event.key {
                    Key::Character(c) if action_mod && c.eq_ignore_ascii_case("v") => {
                        #[cfg(any(
                            target_os = "windows",
                            target_os = "macos",
                            target_os = "linux"
                        ))]
                        if let Ok(cb) = clipboard_rs::ClipboardContext::new()
                            && let Ok(text) = clipboard_rs::Clipboard::get_text(&cb)
                        {
                            self.paste_into_color_edit(&text);
                        }
                    }
                    Key::Named(NamedKey::Escape) => self.cancel_color_edit(),
                    Key::Named(NamedKey::Enter) => {
                        self.commit_color_edit();
                    }
                    Key::Named(NamedKey::Backspace) => {
                        let preview = {
                            let edit = self.color_edit.as_mut().unwrap();
                            edit.input.pop();
                            if edit.input.len() == 6 {
                                parse_hex_color(&edit.input).ok()
                            } else {
                                Some(edit.previous)
                            }
                        };
                        if let Some(rgb) = preview {
                            self.apply_color_preview(rgb);
                        } else {
                            self.revision = self.revision.wrapping_add(1);
                        }
                    }
                    Key::Character(text) if !action_mod => {
                        let mut changed = false;
                        let preview = {
                            let edit = self.color_edit.as_mut().unwrap();
                            for ch in text.chars() {
                                if ch == '#' && edit.input.is_empty() {
                                    continue;
                                }
                                if ch.is_ascii_hexdigit() && edit.input.len() < 6 {
                                    edit.input.push(ch.to_ascii_uppercase());
                                    changed = true;
                                }
                            }
                            if edit.input.len() == 6 {
                                parse_hex_color(&edit.input).ok()
                            } else {
                                None
                            }
                        };
                        if let Some(rgb) = preview {
                            self.apply_color_preview(rgb);
                        } else if changed {
                            self.revision = self.revision.wrapping_add(1);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        if event.state.is_down()
            && action_mod
            && matches!(&event.key, Key::Character(c) if c.eq_ignore_ascii_case("v"))
        {
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            if let Ok(cb) = clipboard_rs::ClipboardContext::new()
                && let Ok(text) = clipboard_rs::Clipboard::get_text(&cb)
            {
                self.paste_text(&text);
            }
            return;
        }
        let control = matches!(
            event.key,
            Key::Named(
                NamedKey::F3
                    | NamedKey::F4
                    | NamedKey::F5
                    | NamedKey::F6
                    | NamedKey::F7
                    | NamedKey::F8
                    | NamedKey::F9
                    | NamedKey::F10
                    | NamedKey::F11
            )
        );
        if control {
            if event.state.is_down() {
                match event.key {
                    Key::Named(NamedKey::F3 | NamedKey::F4) => {
                        let count = crate::fonts::CHOICES.len();
                        let step = if event.key == Key::Named(NamedKey::F3) {
                            count - 1
                        } else {
                            1
                        };
                        self.set_font((self.font_index + step) % count);
                        return;
                    }
                    Key::Named(NamedKey::F5) => {
                        self.heading_weight = if self.heading_weight == 600.0 {
                            500.0
                        } else {
                            600.0
                        };
                        for i in [0, 2, 3] {
                            self.blocks[i].editor.set_weight(self.heading_weight);
                        }
                    }
                    Key::Named(NamedKey::F6) => self.hint = !self.hint,
                    Key::Named(NamedKey::F7) => {
                        self.aa = match self.aa {
                            vello::AaConfig::Area => vello::AaConfig::Msaa8,
                            vello::AaConfig::Msaa8 => vello::AaConfig::Msaa16,
                            vello::AaConfig::Msaa16 => vello::AaConfig::Area,
                        }
                    }
                    Key::Named(NamedKey::F8) => self.half_pixel = !self.half_pixel,
                    Key::Named(NamedKey::F9) => self.coverage.next_gamma(),
                    Key::Named(NamedKey::F10) => self.coverage.next_cutoff(),
                    Key::Named(NamedKey::F11) => {
                        self.coverage = crate::coverage::Coverage::default()
                    }
                    _ => unreachable!(),
                }
                self.reflow();
            }
            return;
        }
        self.blocks[self.active].editor.handle_keyboard_event(event);
        self.reflow();
    }

    pub fn handle_pointer_event(&mut self, event: &PointerEvent) {
        if let PointerEvent::Down(down) = event {
            let pos = (down.state.position.x, down.state.position.y);
            if self.color_pill_rect().contains(pos) {
                if self.color_edit.is_none() {
                    self.begin_color_edit();
                }
                return;
            }
            if self.color_edit.is_some() && !self.commit_color_edit() {
                self.cancel_color_edit();
            }
            let y = down.state.position.y as f32;
            if let Some(index) = self
                .blocks
                .iter()
                .position(|b| y >= b.editor.origin.1 && y < b.editor.origin.1 + b.height)
            {
                if index != self.active {
                    self.blocks[self.active].editor.disable_blink();
                    self.blocks[self.active].editor.cursor_blink();
                    self.active = index;
                }
            }
        }
        if self.color_edit.is_some() {
            return;
        }
        self.blocks[self.active].editor.handle_pointer_event(event);
        self.revision += 1;
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * 52.0,
                MouseScrollDelta::PixelDelta(p) => (p.y / self.scale) as f32,
            };
            self.scroll =
                (self.scroll - dy).clamp(0.0, (self.content_height - self.height).max(0.0));
            self.reflow();
        } else if !self.editing_color() {
            let composing = matches!(event, WindowEvent::Ime(_));
            self.blocks[self.active].editor.handle_event(event);
            if composing {
                self.reflow();
            }
        }
    }

    pub fn handle_accesskit_action_request(&mut self, req: &accesskit::ActionRequest) {
        if let Some(index) = self.blocks.iter().position(|b| b.id == req.target_node) {
            self.active = index;
        }
        self.blocks[self.active]
            .editor
            .handle_accesskit_action_request(req);
        self.reflow();
    }

    pub fn accessibility(&mut self, update: &mut TreeUpdate, node: &mut Node) {
        node.set_role(Role::Document);
        node.set_label("Images, files and covers — Quiet.md typography comparison");
        node.set_transform(accesskit::Affine::scale(self.scale));
        node.set_children(self.blocks.iter().map(|b| b.id).collect::<Vec<_>>());
        for block in &mut self.blocks {
            let mut child = Node::new(Role::TextInput);
            block.editor.accessibility(update, &mut child);
            update.nodes.push((block.id, child));
        }
        update.focus = self.blocks[self.active].id;
    }

    pub fn draw(&mut self, scene: &mut Scene) -> u64 {
        for block in &mut self.blocks {
            block.editor.draw(scene);
            if let Some(marker) = block.marker {
                let (x, y) = block.editor.origin;
                block.editor.label(
                    scene,
                    marker,
                    x - 23.0,
                    y,
                    16.0,
                    AlphaColor::from_rgb8(51, 112, 255),
                );
            }
        }
        // Quiet.md's breadcrumb and collapsed outline control.
        let ink = color_of(self.text_color);
        let pill = self.color_pill_rect();
        let editing = self.color_edit.is_some();
        let pill_text = match &self.color_edit {
            Some(edit) => format!("#{}_", edit.input),
            None => format_hex_color(self.text_color),
        };
        let label = &mut self.blocks[0].editor;
        label.label(
            scene,
            "My documents",
            16.0,
            6.0,
            13.0,
            AlphaColor::from_rgb8(115, 116, 119),
        );
        label.label(
            scene,
            "›",
            111.0,
            6.0,
            16.0,
            AlphaColor::from_rgb8(115, 116, 119),
        );
        let pill_shape = RoundedRect::from_rect(pill, 5.0);
        let border_color = if editing {
            AlphaColor::from_rgb8(51, 112, 255)
        } else {
            AlphaColor::from_rgb8(216, 218, 222)
        };
        scene.fill(
            Fill::NonZero,
            Affine::scale(self.scale),
            AlphaColor::from_rgb8(247, 248, 250),
            None,
            &pill_shape,
        );
        scene.stroke(
            &Stroke::new(if editing { 1.5 } else { 1.0 }),
            Affine::scale(self.scale),
            border_color,
            None,
            &pill_shape,
        );
        let swatch = RoundedRect::from_rect(
            Rect::new(pill.x0 + 8.0, pill.y0 + 6.0, pill.x0 + 18.0, pill.y0 + 16.0),
            2.0,
        );
        scene.fill(Fill::NonZero, Affine::scale(self.scale), ink, None, &swatch);
        scene.stroke(
            &Stroke::new(0.75),
            Affine::scale(self.scale),
            AlphaColor::from_rgba8(0, 0, 0, 50),
            None,
            &swatch,
        );
        label.label(
            scene,
            &pill_text,
            pill.x0 as f32 + 24.0,
            2.0,
            12.0,
            ink,
        );
        label.label(
            scene,
            "Images, files and covers",
            124.0,
            6.0,
            13.0,
            ink,
        );
        for (dy, length) in [(0.0, 13.0), (5.0, 13.0), (10.0, 8.0)] {
            scene.stroke(
                &Stroke::new(1.3),
                Affine::scale(self.scale),
                ink,
                None,
                &Line::new((14.0, 114.0 + dy), (14.0 + length, 114.0 + dy)),
            );
        }
        self.revision
    }
}

#[cfg(test)]
mod tests;
