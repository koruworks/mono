// SPDX-License-Identifier: MPL-2.0

//! A sent chat file opened over its Koru window. Preparation and saving run on
//! workers. Plain text uses Koru's renderer on every desktop platform.

use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use koru_ai_ui::{AttachmentOpenRequest, AttachmentPreview};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme, theme::role};
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_window::OpenAs;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Fill;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};

const MARGIN: f32 = 36.0;
const MAX_W: f32 = 920.0;
const MAX_H: f32 = 760.0;
const HEADER_H: f32 = 52.0;
const FOOTER_H: f32 = 34.0;
const BODY_PAD: f32 = 12.0;
const LINE_H: f32 = 22.0;
const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

struct Prepared {
    path: PathBuf,
    lines: Option<Vec<String>>,
    truncated: bool,
}

pub(super) struct FilePreview {
    request: AttachmentOpenRequest,
    prepared: Receiver<Result<Prepared, String>>,
    path: Option<PathBuf>,
    lines: Option<Vec<String>>,
    truncated: bool,
    scroll: usize,
    error: Option<String>,
    saving: Option<Receiver<Result<(), String>>>,
    opening: Option<Receiver<Result<(), String>>>,
    status: Option<String>,
}

impl FilePreview {
    pub(super) fn open(request: AttachmentOpenRequest) -> Self {
        let (sender, prepared) = mpsc::channel();
        let mut error = None;
        if !cfg!(test) {
            let work = request.clone();
            if let Err(failure) = std::thread::Builder::new()
                .name("koru-ai-preview".into())
                .spawn(move || {
                    let _ = sender.send(prepare(&work));
                })
            {
                error = Some(failure.to_string());
            }
        }
        Self {
            request,
            prepared,
            path: None,
            lines: None,
            truncated: false,
            scroll: 0,
            error,
            saving: None,
            opening: None,
            status: None,
        }
    }

    pub(super) fn maintenance_deadline(&self) -> Option<Instant> {
        (self.path.is_none() && self.error.is_none()
            || self.saving.is_some()
            || self.opening.is_some())
        .then(|| Instant::now() + Duration::from_millis(50))
    }

    pub(super) fn maintain(&mut self) -> bool {
        let mut changed = false;
        if let Ok(result) = self.prepared.try_recv() {
            changed = true;
            match result {
                Ok(prepared) => {
                    self.path = Some(prepared.path);
                    self.lines = prepared.lines;
                    self.truncated = prepared.truncated;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(saving) = &self.saving
            && let Ok(result) = saving.try_recv()
        {
            self.status = Some(match result {
                Ok(()) => "Saved a copy.".into(),
                Err(error) => format!("Couldn't save the file: {error}"),
            });
            self.saving = None;
            changed = true;
        }
        if let Some(opening) = &self.opening
            && let Ok(result) = opening.try_recv()
        {
            self.status = Some(match result {
                Ok(()) => "Opened in the default app.".into(),
                Err(error) => format!("Couldn't open the file: {error}"),
            });
            self.opening = None;
            changed = true;
        }
        changed
    }

    pub(super) fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        pointer: (f32, f32),
    ) {
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );
        let panel = panel_rect(viewport);
        let _inspection = koru_draw::inspect::scope(
            "ChatFilePreview",
            "component",
            root,
            scene::rect(panel.x, panel.y, panel.w, panel.h),
            |record| record.property("file", &self.request.name, "attachment name"),
        );
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(12.0),
        );
        let title = engine.elided(&self.request.name, role::row(), (panel.w - 320.0).max(1.0));
        let title = engine.simple_layout(&title, role::row());
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            panel.x + 18.0,
            panel.y + (HEADER_H - title.height()) * 0.5,
            Some(p.text),
        );
        if self.path.is_some() {
            let save = save_rect(viewport);
            ActionButton::new("Save as…", ActionStyle::Secondary, None).draw(
                canvas,
                root,
                engine,
                save,
                save.contains(pointer.0, pointer.1),
            );
            if self.request.preview == AttachmentPreview::Unsupported {
                let open = open_rect(viewport);
                ActionButton::new("Open externally", ActionStyle::Secondary, None).draw(
                    canvas,
                    root,
                    engine,
                    open,
                    open.contains(pointer.0, pointer.1),
                );
            }
        }
        let close = close_rect(viewport);
        IconButton::new(icons::WINDOW_CLOSE, close)
            .with_state(IconButtonState {
                hovered: close.contains(pointer.0, pointer.1),
                active: false,
                disabled: false,
            })
            .draw(canvas, root, engine);
        let body = body_rect(viewport);
        scene::fill(
            canvas,
            root,
            p.code_bg,
            &scene::rrect(body.x, body.y, body.w, body.h, 6.0),
        );
        if let Some(lines) = &self.lines {
            canvas.push_clip_layer(
                Fill::NonZero,
                root,
                &scene::rect(body.x, body.y, body.w, body.h),
            );
            for (index, line) in lines
                .iter()
                .skip(self.scroll)
                .take(visible_lines(viewport))
                .enumerate()
            {
                let line = engine.elided(line, role::row(), (body.w - 28.0).max(1.0));
                let layout = engine.simple_layout(&line, role::row());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &layout,
                    body.x + 14.0,
                    body.y + 10.0 + index as f32 * LINE_H,
                    Some(p.text),
                );
            }
            canvas.pop_layer();
        } else {
            let message = if let Some(error) = &self.error {
                error.as_str()
            } else if self.path.is_none() {
                "Loading file…"
            } else {
                "Preview is unavailable for this file type."
            };
            let line = engine.simple_layout(message, role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &line,
                body.x + 18.0,
                body.y + 20.0,
                Some(p.text_dim),
            );
        }
        let footer = self.status.clone().or_else(|| {
            if self.truncated {
                Some("Preview shows the first 512 KB. Save As keeps the full file.".into())
            } else {
                self.lines.as_ref().map(|lines| {
                    if lines.is_empty() {
                        "Empty file".into()
                    } else {
                        format!(
                            "Lines {}–{} of {} · Scroll to read",
                            self.scroll + 1,
                            (self.scroll + visible_lines(viewport)).min(lines.len()),
                            lines.len()
                        )
                    }
                })
            }
        });
        if let Some(footer) = footer {
            let line = engine.simple_layout(&footer, role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &line,
                panel.x + 18.0,
                panel.y + panel.h - line.height() - 10.0,
                Some(p.text_dim),
            );
        }
    }

    /// Returns `false` only when the preview was closed.
    pub(super) fn press(&mut self, x: f32, y: f32, viewport: (f32, f32)) -> bool {
        if close_rect(viewport).contains(x, y) || !panel_rect(viewport).contains(x, y) {
            return false;
        }
        if save_rect(viewport).contains(x, y) {
            self.save_as();
        } else if self.request.preview == AttachmentPreview::Unsupported
            && open_rect(viewport).contains(x, y)
        {
            self.open_externally();
        }
        true
    }

    pub(super) fn scroll_by(&mut self, delta: MouseScrollDelta, viewport: (f32, f32)) {
        let amount = match delta {
            MouseScrollDelta::LineDelta(_, y) => -(y * 3.0) as i32,
            MouseScrollDelta::PixelDelta(position) => -(position.y / f64::from(LINE_H)) as i32,
        };
        self.scroll_lines(amount, viewport);
    }

    pub(super) fn key(&mut self, key: &Key, viewport: (f32, f32)) {
        let page = visible_lines(viewport) as i32;
        let amount = match key {
            Key::Named(NamedKey::ArrowDown) => 1,
            Key::Named(NamedKey::ArrowUp) => -1,
            Key::Named(NamedKey::PageDown) => page,
            Key::Named(NamedKey::PageUp) => -page,
            Key::Named(NamedKey::Home) => -i32::MAX,
            Key::Named(NamedKey::End) => i32::MAX,
            _ => 0,
        };
        self.scroll_lines(amount, viewport);
    }

    fn scroll_lines(&mut self, amount: i32, viewport: (f32, f32)) {
        let max = self.lines.as_ref().map_or(0, |lines| {
            lines.len().saturating_sub(visible_lines(viewport))
        });
        self.scroll = self.scroll.saturating_add_signed(amount as isize).min(max);
    }

    fn save_as(&mut self) {
        let Some(source) = self.path.clone() else {
            return;
        };
        let chosen = koru_workspace_host::dialogs::pick_save_file(
            "Save chat attachment",
            &self.request.name,
            &[],
            None,
            None,
        );
        let Some(destination) = chosen else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-ai-save-as".into())
            .spawn(move || {
                let result = std::fs::copy(&source, &destination)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })
        {
            self.status = Some(format!("Couldn't save the file: {error}"));
            return;
        }
        self.saving = Some(receiver);
        self.status = Some("Saving…".into());
    }

    fn open_externally(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-ai-open-external".into())
            .spawn(move || {
                let result = koru_window::open_uri(path.to_string_lossy().as_ref())
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })
        {
            self.status = Some(format!("Couldn't open the file: {error}"));
            return;
        }
        self.opening = Some(receiver);
        self.status = Some("Opening…".into());
    }
}

fn prepare(request: &AttachmentOpenRequest) -> Result<Prepared, String> {
    let path = if let Some(source) = &request.reference_path {
        std::fs::metadata(source).map_err(|error| error.to_string())?;
        source.clone()
    } else {
        let mode = if request.preview == AttachmentPreview::Text {
            OpenAs::Text
        } else {
            OpenAs::AsIs
        };
        let path = koru_window::open_bytes::temporary_open_path(
            &request.id.to_string(),
            &request.name,
            mode,
        );
        let parent = path.parent().ok_or("No preview directory")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let root = crate::search::gateway_root();
        let client = match root {
            Some(root) => koru_api_client::Client::open_at_with_scopes(
                koru_channel::segment(),
                root,
                &["ai.chat"],
            ),
            None => {
                koru_api_client::Client::ensure_with_scopes(koru_channel::segment(), &["ai.chat"])
            }
        }
        .map_err(|error| error.to_string())?;
        let bytes = client
            .ai()
            .attachment_bytes(request.chat, request.id)
            .map_err(|error| error.message)?;
        std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
        path
    };
    let (lines, truncated) = if request.preview == AttachmentPreview::Text {
        let mut file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_PREVIEW_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let truncated = bytes.len() as u64 > MAX_PREVIEW_BYTES;
        bytes.truncate(MAX_PREVIEW_BYTES as usize);
        let text = String::from_utf8_lossy(&bytes);
        (Some(text.lines().map(str::to_owned).collect()), truncated)
    } else {
        (None, false)
    };
    Ok(Prepared {
        path,
        lines,
        truncated,
    })
}

fn panel_rect(viewport: (f32, f32)) -> Rect {
    let w = (viewport.0 - MARGIN * 2.0).min(MAX_W).max(360.0);
    let h = (viewport.1 - MARGIN * 2.0).min(MAX_H).max(240.0);
    Rect::new((viewport.0 - w) * 0.5, (viewport.1 - h) * 0.5, w, h)
}

fn body_rect(viewport: (f32, f32)) -> Rect {
    let panel = panel_rect(viewport);
    Rect::new(
        panel.x + BODY_PAD,
        panel.y + HEADER_H,
        panel.w - BODY_PAD * 2.0,
        panel.h - HEADER_H - FOOTER_H,
    )
}

fn visible_lines(viewport: (f32, f32)) -> usize {
    ((body_rect(viewport).h - 20.0) / LINE_H).floor().max(1.0) as usize
}

fn save_rect(viewport: (f32, f32)) -> Rect {
    let panel = panel_rect(viewport);
    Rect::new(panel.x + panel.w - 154.0, panel.y + 10.0, 96.0, 32.0)
}

fn open_rect(viewport: (f32, f32)) -> Rect {
    let panel = panel_rect(viewport);
    Rect::new(panel.x + panel.w - 298.0, panel.y + 10.0, 132.0, 32.0)
}

fn close_rect(viewport: (f32, f32)) -> Rect {
    let panel = panel_rect(viewport);
    Rect::new(panel.x + panel.w - 46.0, panel.y + 10.0, 32.0, 32.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_preview_scroll_stays_within_content() {
        let request = AttachmentOpenRequest {
            chat: koru_ai_ui::ChatId::new(),
            id: koru_ai_core::ChatAttachmentId::new(),
            name: "notes.txt".into(),
            reference_path: None,
            preview: AttachmentPreview::Text,
        };
        let mut preview = FilePreview::open(request);
        preview.lines = Some((0..100).map(|n| n.to_string()).collect());
        let viewport = (1200.0, 900.0);
        preview.scroll_lines(i32::MAX, viewport);
        assert_eq!(preview.scroll, 100 - visible_lines(viewport));
        preview.scroll_lines(-i32::MAX, viewport);
        assert_eq!(preview.scroll, 0);
    }

    #[test]
    fn preview_fits_with_a_separate_toolbar() {
        let panel = panel_rect((1200.0, 900.0));
        let body = body_rect((1200.0, 900.0));
        assert!(body.y >= panel.y + HEADER_H);
        assert!(body.x >= panel.x && body.x + body.w <= panel.x + panel.w);
        assert!(body.y + body.h <= panel.y + panel.h);
    }
}
