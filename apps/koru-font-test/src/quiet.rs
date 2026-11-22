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
    kurbo::{Affine, Line, Stroke},
    peniko::color::AlphaColor,
};
use winit::event::{MouseScrollDelta, WindowEvent};

struct Block {
    editor: Editor,
    gap_before: f32,
    indent: f32,
    marker: Option<&'static str>,
    id: NodeId,
    height: f32,
}

pub struct Page {
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

    pub fn window_title(&self) -> String {
        format!(
            "F3/F4 font {}/{} {} | F9 gamma {:.2} | F10 cutoff {:.0}% | F11 reset | F5 weight {} | F6 hint {} | F7 {:?} | F8 offset {}px",
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

    pub fn handle_keyboard_event(&mut self, event: &KeyboardEvent) {
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
        } else {
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
        label.label(
            scene,
            "Images, files and covers",
            124.0,
            6.0,
            13.0,
            AlphaColor::from_rgb8(31, 35, 41),
        );
        for (dy, length) in [(0.0, 13.0), (5.0, 13.0), (10.0, 8.0)] {
            scene.stroke(
                &Stroke::new(1.3),
                Affine::scale(self.scale),
                AlphaColor::from_rgb8(31, 35, 41),
                None,
                &Line::new((14.0, 114.0 + dy), (14.0 + length, 114.0 + dy)),
            );
        }
        self.revision
    }
}
