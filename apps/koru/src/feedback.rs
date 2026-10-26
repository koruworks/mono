// SPDX-License-Identifier: MPL-2.0

//! The Send feedback tab: a form, not a document.
//!
//! Nothing written here is stored in the workspace. Closing the tab stashes a
//! process-scoped draft so opening Feedback again in this session restores it.
//!
//! **Send does not wait.** Pressing it hands the report to [`outbox`] and
//! shows the thank-you page immediately; the POST finishes on a worker that
//! outlives this view. Success says nothing more, because the page already
//! did. Failure comes back as a toast, and the draft it carries restores the
//! form so the report is never lost to a dropped connection.

pub(crate) mod draft;
mod logs;
pub(crate) mod outbox;
mod submit;

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, icons, scene, theme};
use koru_ui::button::{button_width, row_from_right};
use koru_ui::{
    ActionButton, ActionStyle, ButtonRole, Confirm, InputStyle, TextArea, TextInput, draw_button,
};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Blob, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

pub use logs::zip_logs;
pub use submit::{Payload, SubmitError, compress_image, endpoint, validate};

#[cfg(test)]
mod tests;

const COLUMN_W: f32 = 560.0;
const PAD: f32 = 24.0;
const CARD_H: f32 = 88.0;
const CARD_GAP: f32 = 8.0;
const CHECK_H: f32 = 22.0;
const LABEL_H: f32 = 20.0;
const LABEL_GAP: f32 = 8.0;
const SECTION_GAP: f32 = 16.0;
const COMPOSER_MIN_LINES: usize = 5;
const COMPOSER_MAX_LINES: usize = 12;
const THUMB_W: f32 = 96.0;
const THUMB_H: f32 = 72.0;
const THUMB_GAP: f32 = 12.0;
const CLOSE_S: f32 = 18.0;
const KIND_EMOJI: theme::TextStyle = theme::TextStyle::new(
    theme::TextMetrics::new(18.0, 22.0),
    theme::FontWeight::Medium,
);
const MAX_ATTACHMENTS: usize = submit::MAX_SCREENSHOTS;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    Bug,
    Idea,
    /// First on the page and chosen when it opens: most of what people send
    /// is neither a fault nor a request.
    #[default]
    General,
}

impl Kind {
    /// The cards, left to right. Their positions come from this order alone.
    fn all() -> [Self; 3] {
        [Self::General, Self::Idea, Self::Bug]
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Idea => "idea",
            Self::General => "general",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Bug => "Something isn't working",
            Self::Idea => "I have an idea",
            Self::General => "General feedback",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Bug => "What were you trying to do? What happened instead?",
            Self::Idea => "What would make Koru better for you?",
            Self::General => "Tell us what's on your mind.",
        }
    }

    fn emoji(self) -> &'static str {
        match self {
            // Escapes, not literals: a Windows rewrite of this file once
            // turned the UTF-8 glyphs into Latin-1 mojibake on the cards.
            Self::Bug => "\u{1F41B}",
            Self::Idea => "\u{1F4A1}",
            Self::General => "\u{1F4AC}",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Frequency {
    Once,
    Sometimes,
    EveryTime,
}

impl Frequency {
    fn all() -> [Self; 3] {
        [Self::Once, Self::Sometimes, Self::EveryTime]
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Sometimes => "sometimes",
            Self::EveryTime => "every_time",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Once => "Once",
            Self::Sometimes => "Sometimes",
            Self::EveryTime => "Every time",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Importance {
    Nice,
    Significant,
    Regular,
    Blocking,
}

impl Importance {
    fn all() -> [Self; 4] {
        [Self::Nice, Self::Significant, Self::Regular, Self::Blocking]
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::Nice => "nice",
            Self::Significant => "significant",
            Self::Regular => "regular",
            Self::Blocking => "blocking",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Nice => "Nice to have",
            Self::Significant => "Would improve Koru significantly for me",
            Self::Regular => "I regularly need this",
            Self::Blocking => "This is preventing me from using Koru",
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Composer,
    Expected,
    Email,
    None,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Hit {
    Kind(Kind),
    Composer,
    Frequency(Frequency),
    Importance(Importance),
    Expected,
    AddScreenshot,
    Shot(usize),
    RemoveShot(usize),
    Logs,
    More,
    Contact,
    Email,
    Cancel,
    Send,
    /// The one control on the thank-you page.
    ThanksClose,
}

pub struct FeedbackView {
    engine: TextEngine,
    composer: TextArea,
    expected: TextInput,
    email: TextInput,
    kind: Kind,
    frequency: Option<Frequency>,
    importance: Option<Importance>,
    include_logs: bool,
    more_open: bool,
    contact: bool,
    attachments: Vec<Attachment>,
    thumbs: ImageCache,
    focus: Focus,
    viewport: (f32, f32),
    scroll: f32,
    pointer: (f32, f32),
    hover: Option<Hit>,
    error: Option<String>,
    /// The report is away and the thank-you page is up. The send itself is
    /// the outbox's problem from that moment on.
    thanks: bool,
    confirm: Option<Confirm>,
    wants_close: bool,
    wants_pick: bool,
    forget: bool,
    image_rx: Option<Receiver<Result<koru_clipboard::ImagePayload, koru_clipboard::Error>>>,
}

impl Default for FeedbackView {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackView {
    pub fn new() -> Self {
        let draft = draft::load();
        let mut engine = TextEngine::new();
        let mut composer = TextArea::new(&mut engine, theme::role::ui());
        if !draft.message.is_empty() {
            composer.set_text(&mut engine, &draft.message);
        }
        let expected = if draft.expected.is_empty() {
            TextInput::new(&mut engine, theme::role::row())
        } else {
            TextInput::with_text(&mut engine, theme::role::row(), &draft.expected)
        };
        let email = if draft.contact_email.is_empty() {
            TextInput::new(&mut engine, theme::role::row())
        } else {
            TextInput::with_text(&mut engine, theme::role::row(), &draft.contact_email)
        };
        let mut view = Self {
            engine,
            composer,
            expected,
            email,
            kind: draft.kind,
            frequency: draft.frequency,
            importance: draft.importance,
            include_logs: draft.include_logs,
            more_open: draft.more_open,
            contact: draft.contact,
            attachments: draft.attachments,
            thumbs: ImageCache::new(),
            focus: draft.focus,
            viewport: (900.0, 560.0),
            scroll: 0.0,
            pointer: (0.0, 0.0),
            hover: None,
            error: None,
            thanks: false,
            confirm: None,
            wants_close: false,
            wants_pick: false,
            forget: false,
            image_rx: None,
        };
        view.rebuild_thumbs();
        view
    }

    pub fn take_wants_close(&mut self) -> bool {
        std::mem::take(&mut self.wants_close)
    }

    pub fn take_wants_pick(&mut self) -> bool {
        std::mem::take(&mut self.wants_pick)
    }

    /// The Close button on the thank-you page.
    ///
    /// Laid out from the viewport rather than from [`Laid`]: the page is one
    /// centred block with one control, and threading it through the form's
    /// layout would make every field's geometry answer for a screen that has
    /// no fields.
    fn thanks_close(&mut self) -> Rect {
        let w = button_width(&mut self.engine, "Close").max(96.0);
        let x = (self.viewport.0 - w) * 0.5;
        let y = (self.viewport.1 * 0.5) + 28.0;
        Rect::new(x, y, w, 32.0)
    }

    pub fn attach(&mut self, shot: Attachment) {
        if self.attachments.len() >= MAX_ATTACHMENTS {
            self.error = Some("You can attach up to three images.".into());
            return;
        }
        self.attachments.push(shot);
        self.rebuild_thumbs();
        self.error = None;
        self.persist();
    }

    fn rebuild_thumbs(&mut self) {
        self.thumbs = ImageCache::new();
        for (i, shot) in self.attachments.iter().enumerate() {
            if let Some(brush) = decode_shot(&shot.bytes) {
                // Unscoped: these keys are attachment indices, not blob hashes,
                // and a screenshot in this dialog belongs to no workspace.
                self.thumbs
                    .insert(ImageCache::UNSCOPED, &i.to_string(), brush);
            }
        }
    }

    pub fn attach_bytes(&mut self, filename: &str, bytes: &[u8]) {
        match compress_image(bytes, filename) {
            Ok(shot) => self.attach(shot),
            Err(error) => self.error = Some(error),
        }
    }

    fn persist(&self) {
        if self.forget {
            return;
        }
        draft::save(self.snapshot());
    }

    fn snapshot(&self) -> draft::Draft {
        draft::Draft {
            kind: self.kind,
            message: self.composer.value(),
            expected: self.expected.value(),
            frequency: self.frequency,
            importance: self.importance,
            include_logs: self.include_logs,
            more_open: self.more_open,
            contact: self.contact,
            contact_email: self.email.value(),
            attachments: self.attachments.clone(),
            focus: self.focus,
        }
    }

    fn can_send(&self) -> bool {
        !self.thanks
            && self.composer.value().trim().len() >= 3
            && (!self.contact || submit::is_email(&self.email.value()))
    }

    fn column(&self) -> Rect {
        let w = COLUMN_W.min(self.viewport.0 - PAD * 2.0).max(280.0);
        let x = ((self.viewport.0 - w) * 0.5).max(PAD);
        Rect::new(x, PAD - self.scroll, w, 0.0)
    }

    fn send(&mut self) {
        if !self.can_send() {
            self.error = Some(if self.contact && !submit::is_email(&self.email.value()) {
                SubmitError::InvalidEmail.to_string()
            } else {
                SubmitError::Empty.to_string()
            });
            return;
        }
        let payload = Payload {
            kind: self.kind,
            message: self.composer.value().trim().to_string(),
            frequency: if self.kind == Kind::Bug {
                self.frequency
            } else {
                None
            },
            expected: if self.kind == Kind::Bug {
                let expected = self.expected.value();
                (!expected.is_empty()).then_some(expected)
            } else {
                None
            },
            importance: if self.kind == Kind::Idea {
                self.importance
            } else {
                None
            },
            contact: self
                .contact
                .then(|| self.email.value())
                .filter(|email| !email.is_empty()),
            screenshots: self.attachments.clone(),
            logs_zip: None,
        };
        if let Err(error) = validate(&payload) {
            self.error = Some(error.to_string());
            return;
        }
        self.error = None;
        // **The outbox takes the only copy of the draft.** Clearing the
        // process slot here is what stops a report that has been sent from
        // reappearing in the form; if both attempts fail, the shell puts this
        // very snapshot back and the toast says so.
        outbox::send(endpoint(), payload, self.snapshot(), self.include_logs);
        draft::clear();
        self.forget = true;
        self.thanks = true;
    }

    fn begin_image_paste(&mut self) {
        if self.image_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if koru_clipboard::spawn_image_read(koru_clipboard::ImageLimits::default(), move |result| {
            let _ = tx.send(result);
        })
        .is_ok()
        {
            self.image_rx = Some(rx);
        }
    }

    fn poll_images(&mut self) -> bool {
        let Some(rx) = &self.image_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(koru_clipboard::ImagePayload::Bitmap(png))) => {
                self.image_rx = None;
                self.attach_bytes("paste.png", &png.bytes);
                true
            }
            Ok(Ok(koru_clipboard::ImagePayload::Files(paths))) => {
                self.image_rx = None;
                for path in paths {
                    if let Ok(bytes) = std::fs::read(&path) {
                        let name = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("screenshot.png");
                        self.attach_bytes(name, &bytes);
                    }
                }
                true
            }
            Ok(Ok(koru_clipboard::ImagePayload::Empty)) | Ok(Err(_)) => {
                self.image_rx = None;
                false
            }
            Err(_) => false,
        }
    }

    fn ask_cancel(&mut self) {
        if self.composer.value().trim().is_empty() && self.attachments.is_empty() {
            self.persist();
            self.wants_close = true;
            return;
        }
        self.confirm = Some(Confirm::new(
            "Discard this report?",
            "Closing keeps a copy for this session. Cancel stays on the page.",
            "Close",
        ));
    }
}

/// Zip whatever recent log files exist, for an opt-in attachment.
///
/// **Called on the submit worker, never on the caller** — see
/// [`submit::spawn_prepared`]. Reading the log directory and deflating up to
/// two megabytes is not work the frame drawing the thank-you page can afford.
fn collect_logs_zip() -> Option<Vec<u8>> {
    let root = koru_channel::store_root()?.join(koru_log::LOG_DIR);
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().into_owned();
        if !name.contains(".log") {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            files.push((name, bytes));
        }
    }
    zip_logs(files).ok()
}

impl Drop for FeedbackView {
    fn drop(&mut self) {
        self.persist();
    }
}

impl View for FeedbackView {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if let Some(mut confirm) = self.confirm.take() {
            return match confirm.key(key) {
                // Two answers here; the third belongs to a dialog that
                // declared one (`Confirm::with_alternative`).
                koru_ui::confirm::Outcome::Keep | koru_ui::confirm::Outcome::Alternative => {
                    self.confirm = Some(confirm);
                    true
                }
                koru_ui::confirm::Outcome::Cancel => true,
                koru_ui::confirm::Outcome::Confirm => {
                    self.persist();
                    self.wants_close = true;
                    true
                }
            };
        }
        // The thank-you page has nothing to discard and one thing to do, so
        // Escape and Enter both mean Close rather than "are you sure".
        if self.thanks {
            if matches!(key, Key::Named(NamedKey::Escape | NamedKey::Enter)) {
                self.wants_close = true;
            }
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.ask_cancel();
            return true;
        }
        if koru_keys::is_chord(mods) && koru_ui::ctrl_letter(key, text) == Some('v') {
            self.begin_image_paste();
        }
        match self.focus {
            Focus::Composer => {
                let _ = self.composer.key(&mut self.engine, key, text, mods);
                self.persist();
                true
            }
            Focus::Expected => {
                let _ = self.expected.key(&mut self.engine, key, text, mods);
                self.persist();
                true
            }
            Focus::Email => {
                let _ = self.email.key(&mut self.engine, key, text, mods);
                self.persist();
                true
            }
            Focus::None => false,
        }
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        self.pointer = (x, y);
        if let Some(mut confirm) = self.confirm.take() {
            return match confirm.mouse_down(&mut self.engine, self.viewport, x, y) {
                // Two answers here; the third belongs to a dialog that
                // declared one (`Confirm::with_alternative`).
                koru_ui::confirm::Outcome::Keep | koru_ui::confirm::Outcome::Alternative => {
                    self.confirm = Some(confirm);
                    true
                }
                koru_ui::confirm::Outcome::Cancel => true,
                koru_ui::confirm::Outcome::Confirm => {
                    self.persist();
                    self.wants_close = true;
                    true
                }
            };
        }
        if self.thanks {
            if self.thanks_close().contains(x, y) {
                self.wants_close = true;
            }
            return true;
        }
        let Some(hit) = self.hit_at(x, y) else {
            self.focus = Focus::None;
            return false;
        };
        match hit {
            Hit::Kind(kind) => {
                self.kind = kind;
                self.persist();
            }
            Hit::Composer => {
                self.focus = Focus::Composer;
                let rect = self.layout().composer;
                self.composer.click(&mut self.engine, rect, x, y);
            }
            Hit::Expected => {
                self.focus = Focus::Expected;
                if let Some(rect) = self.layout().expected {
                    self.expected.click(&mut self.engine, rect, x, y);
                }
            }
            Hit::Email => {
                self.focus = Focus::Email;
                if let Some(rect) = self.layout().email {
                    self.email.click(&mut self.engine, rect, x, y);
                }
            }
            Hit::Frequency(value) => {
                self.frequency = Some(value);
                self.persist();
            }
            Hit::Importance(value) => {
                self.importance = Some(value);
                self.persist();
            }
            Hit::AddScreenshot => self.wants_pick = true,
            Hit::Shot(_) => {}
            Hit::RemoveShot(i) => {
                if i < self.attachments.len() {
                    self.attachments.remove(i);
                    self.rebuild_thumbs();
                    self.persist();
                }
            }
            Hit::Logs => {
                self.include_logs = !self.include_logs;
                self.persist();
            }
            Hit::More => {
                self.more_open = !self.more_open;
                self.persist();
            }
            Hit::Contact => {
                self.contact = !self.contact;
                if self.contact {
                    self.focus = Focus::Email;
                }
                self.persist();
            }
            Hit::Cancel => self.ask_cancel(),
            Hit::Send => self.send(),
            // The form's hit test never reports it; the thank-you page is
            // handled above, before any of this runs.
            Hit::ThanksClose => {}
        }
        true
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if self.thanks {
            let over = self.thanks_close().contains(x, y);
            let hover = over.then_some(Hit::ThanksClose);
            if hover != self.hover {
                self.hover = hover;
                return true;
            }
            return false;
        }
        let laid = self.layout();
        if self.composer.drag(&mut self.engine, laid.composer, x, y) {
            return true;
        }
        if let Some(expected) = laid.expected
            && self.expected.drag(&mut self.engine, expected, x)
        {
            return true;
        }
        if let Some(email) = laid.email
            && self.email.drag(&mut self.engine, email, x)
        {
            return true;
        }
        let hover = laid.hit(x, y);
        if hover != self.hover {
            self.hover = hover;
            return true;
        }
        false
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        self.composer.release();
        self.expected.release();
        self.email.release();
        false
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        let rect = self.layout().composer;
        if rect.contains(x, y) {
            self.composer.click(&mut self.engine, rect, x, y);
            return true;
        }
        false
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let line = theme::role::ui().metrics.line_height * 3.0;
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y * line,
            MouseScrollDelta::PixelDelta(p) => -p.y as f32,
        };
        let laid = self.layout();
        if laid.composer.contains(self.pointer.0, self.pointer.1) {
            self.composer.scroll_by(dy);
            return;
        }
        let max = (laid.content_h - self.viewport.1).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
    }

    fn cursor_icon(&self) -> CursorIcon {
        match self.hover {
            Some(
                Hit::Kind(_)
                | Hit::Frequency(_)
                | Hit::Importance(_)
                | Hit::AddScreenshot
                | Hit::Shot(_)
                | Hit::RemoveShot(_)
                | Hit::Logs
                | Hit::More
                | Hit::Contact
                | Hit::ThanksClose
                | Hit::Cancel
                | Hit::Send,
            ) => CursorIcon::Default,
            Some(Hit::Composer | Hit::Expected | Hit::Email) => CursorIcon::Text,
            None => CursorIcon::Default,
        }
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.engine.clear_pill_cache();
    }

    /// **The send is not on this clock.** A pasted image is still arriving for
    /// this view, but the report belongs to the outbox the moment Send is
    /// pressed, and the shell ticks that whether or not this tab is open.
    fn maintenance_deadline(&self) -> Option<Instant> {
        self.image_rx.is_some().then(Instant::now)
    }

    fn maintain(&mut self, _ws: &mut WorkspaceDoc, _now: Instant) -> bool {
        self.poll_images()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &Workspace,
        _images: &ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        let p = theme::p();
        let (vw, vh) = self.viewport;
        scene::fill(scene, root, p.bg, &scene::rect(0.0, 0.0, vw, vh));
        if self.thanks {
            self.paint_thanks(scene, root);
            return;
        }
        scene.push_clip_layer(Fill::NonZero, root, &scene::rect(0.0, 0.0, vw, vh));
        let laid = self.layout();
        self.paint(scene, root, &laid);
        scene.pop_layer();
        if let Some(geom) = koru_ui::scrollbar::ScrollbarGeom::for_content(
            Rect::new(0.0, 0.0, vw, vh),
            laid.content_h,
            self.scroll,
        ) {
            koru_ui::scrollbar::draw(scene, root, geom, false);
        }
        if let Some(confirm) = self.confirm.take() {
            confirm.draw(scene, root, &mut self.engine, (vw, vh));
            self.confirm = Some(confirm);
        }
    }
}

struct ShotGeom {
    thumb: Rect,
    name: Rect,
    close: Rect,
}

struct Laid {
    x: f32,
    header_y: f32,
    kind_label_y: f32,
    kinds: [Rect; 3],
    composer: Rect,
    freq_label_y: Option<f32>,
    freqs: Vec<(Frequency, Rect)>,
    expected_label_y: Option<f32>,
    expected: Option<Rect>,
    importance_label_y: Option<f32>,
    importances: Vec<(Importance, Rect)>,
    add_shot: Rect,
    paste_hint_x: f32,
    shots: Vec<ShotGeom>,
    logs: Rect,
    more: Rect,
    contact: Option<Rect>,
    email: Option<Rect>,
    error_y: Option<f32>,
    hint: Option<Rect>,
    cancel: Rect,
    send: Rect,
    content_h: f32,
}

impl Laid {
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        for (i, kind) in Kind::all().into_iter().enumerate() {
            if self.kinds[i].contains(x, y) {
                return Some(Hit::Kind(kind));
            }
        }
        if self.composer.contains(x, y) {
            return Some(Hit::Composer);
        }
        for (freq, rect) in &self.freqs {
            if rect.contains(x, y) {
                return Some(Hit::Frequency(*freq));
            }
        }
        if self.expected.is_some_and(|rect| rect.contains(x, y)) {
            return Some(Hit::Expected);
        }
        for (importance, rect) in &self.importances {
            if rect.contains(x, y) {
                return Some(Hit::Importance(*importance));
            }
        }
        if self.add_shot.contains(x, y) {
            return Some(Hit::AddScreenshot);
        }
        for (i, shot) in self.shots.iter().enumerate() {
            if shot.close.contains(x, y) {
                return Some(Hit::RemoveShot(i));
            }
            if shot.thumb.contains(x, y) || shot.name.contains(x, y) {
                return Some(Hit::Shot(i));
            }
        }
        if self.logs.contains(x, y) {
            return Some(Hit::Logs);
        }
        if self.more.contains(x, y) {
            return Some(Hit::More);
        }
        if self.contact.is_some_and(|rect| rect.contains(x, y)) {
            return Some(Hit::Contact);
        }
        if self.email.is_some_and(|rect| rect.contains(x, y)) {
            return Some(Hit::Email);
        }
        if self.cancel.contains(x, y) {
            return Some(Hit::Cancel);
        }
        if self.send.contains(x, y) {
            return Some(Hit::Send);
        }
        None
    }
}

impl FeedbackView {
    fn hit_at(&mut self, x: f32, y: f32) -> Option<Hit> {
        self.layout().hit(x, y)
    }

    fn layout(&mut self) -> Laid {
        let col = self.column();
        let x = col.x;
        let w = col.w;
        let right = x + w;
        let mut y = col.y;
        let header_y = y;
        y += 52.0 + 18.0;
        let kind_label_y = y;
        y += LABEL_H + LABEL_GAP;
        let card_w = (w - CARD_GAP * 2.0) / 3.0;
        let mut i = 0;
        let kinds = Kind::all().map(|_| {
            let rect = Rect::new(x + i as f32 * (card_w + CARD_GAP), y, card_w, CARD_H);
            i += 1;
            rect
        });
        y += CARD_H + 14.0;
        let line = theme::role::ui().metrics.line_height;
        let composer_h = self.composer.desired_height(
            &mut self.engine,
            w,
            line,
            COMPOSER_MIN_LINES,
            COMPOSER_MAX_LINES,
        );
        let composer = Rect::new(x, y, w, composer_h);
        y += composer_h + 12.0;

        let mut freq_label_y = None;
        let mut freqs = Vec::new();
        let mut expected_label_y = None;
        let mut expected = None;
        let mut importance_label_y = None;
        let mut importances = Vec::new();
        match self.kind {
            Kind::Bug => {
                freq_label_y = Some(y);
                y += LABEL_H + LABEL_GAP;
                freqs = Frequency::all()
                    .into_iter()
                    .zip(koru_ui::chip::wrap(
                        &mut self.engine,
                        x,
                        y,
                        right,
                        Frequency::all().map(|freq| freq.label()),
                    ))
                    .collect();
                y = koru_ui::chip::bottom(&freqs.iter().map(|(_, r)| *r).collect::<Vec<_>>(), y)
                    + 12.0;
                expected_label_y = Some(y);
                y += LABEL_H + LABEL_GAP;
                expected = Some(Rect::new(x, y, w, 32.0));
                y += 32.0 + 12.0;
            }
            Kind::Idea => {
                importance_label_y = Some(y);
                y += LABEL_H + LABEL_GAP;
                for importance in Importance::all() {
                    importances.push((importance, Rect::new(x, y, w, 24.0)));
                    y += 26.0;
                }
                y += 8.0;
            }
            Kind::General => {}
        }

        let add = ActionButton::new("+ Add screenshot", ActionStyle::Ghost, None);
        let add_w = add.width(&mut self.engine).max(140.0);
        let add_shot = Rect::new(x, y, add_w, ActionButton::H);
        let paste_hint_x = x + add_w + 8.0;
        y += ActionButton::H + 8.0;
        let mut shots = Vec::new();
        for (i, _) in self.attachments.iter().enumerate() {
            let thumb = Rect::new(x + i as f32 * (THUMB_W + THUMB_GAP), y, THUMB_W, THUMB_H);
            let name = Rect::new(
                thumb.x,
                thumb.y + thumb.h + 4.0,
                thumb.w,
                theme::role::hint().metrics.line_height,
            );
            let close = Rect::new(
                thumb.x + thumb.w - CLOSE_S - 4.0,
                thumb.y + 4.0,
                CLOSE_S,
                CLOSE_S,
            );
            shots.push(ShotGeom { thumb, name, close });
        }
        if !shots.is_empty() {
            y += THUMB_H + 4.0 + theme::role::hint().metrics.line_height + 12.0;
        }

        let logs = Rect::new(x, y, w, CHECK_H + 16.0);
        y += CHECK_H + 16.0 + 10.0;
        let more = Rect::new(x, y, 160.0, 22.0);
        y += 26.0;
        let mut contact = None;
        let mut email = None;
        if self.more_open {
            contact = Some(Rect::new(x, y, w, CHECK_H));
            y += CHECK_H + 8.0;
            if self.contact {
                email = Some(Rect::new(x, y, w, 32.0));
                y += 40.0;
            }
        }

        let mut error_y = None;
        if self.error.is_some() {
            error_y = Some(y);
            y += LABEL_H + 8.0;
        }

        y += SECTION_GAP;
        let send_label = "Send";
        let cancel_w = button_width(&mut self.engine, "Cancel");
        let send_w = button_width(&mut self.engine, send_label);
        let buttons = row_from_right(right, y, &[cancel_w, send_w]);
        let cancel = buttons[0];
        let send = buttons[1];
        let hint = if self.can_send() {
            None
        } else {
            let text = self
                .engine
                .simple_layout("Add a few words so we can help", theme::role::meta());
            let hx = x;
            if hx + text.width() + 12.0 < cancel.x {
                Some(Rect::new(
                    hx,
                    y + (koru_ui::button::H - text.height()) * 0.5,
                    text.width(),
                    text.height(),
                ))
            } else {
                None
            }
        };
        y += koru_ui::button::H + PAD;
        let content_h = (y - col.y) + PAD + self.scroll;

        Laid {
            x,
            header_y,
            kind_label_y,
            kinds,
            composer,
            freq_label_y,
            freqs,
            expected_label_y,
            expected,
            importance_label_y,
            importances,
            add_shot,
            paste_hint_x,
            shots,
            logs,
            more,
            contact,
            email,
            error_y,
            hint,
            cancel,
            send,
            content_h,
        }
    }

    /// The page that replaces the form the instant Send is pressed.
    ///
    /// It says the report has been sent because, as far as this person is
    /// concerned, it has: the outbox will keep trying, and the only thing that
    /// ever contradicts this page is a failure toast offering the form back.
    fn paint_thanks(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        let (vw, vh) = self.viewport;
        let title = self
            .engine
            .simple_layout("Thank you", theme::role::subtitle());
        let title_w = title.width();
        scene::draw_layout(
            scene,
            root,
            &title,
            (vw - title_w) * 0.5,
            vh * 0.5 - 44.0,
            Some(p.text),
        );
        let body = self.engine.simple_layout(
            "We read every report, and it genuinely shapes what we build next.",
            theme::role::meta(),
        );
        let body_w = body.width();
        scene::draw_layout(
            scene,
            root,
            &body,
            (vw - body_w) * 0.5,
            vh * 0.5 - 12.0,
            Some(p.text_dim),
        );
        let close = self.thanks_close();
        draw_button(
            scene,
            root,
            &mut self.engine,
            close,
            "Close",
            ButtonRole::Primary,
            self.hover == Some(Hit::ThanksClose),
            false,
        );
    }

    fn paint(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        self.paint_header(scene, root, laid);
        self.paint_kinds(scene, root, laid);
        self.paint_composer(scene, root, laid);
        self.paint_kind_fields(scene, root, laid);
        self.paint_screenshots(scene, root, laid);
        self.paint_logs(scene, root, laid);
        self.paint_more(scene, root, laid);
        if let (Some(error), Some(y)) = (self.error.clone(), laid.error_y) {
            let layout = self.engine.simple_layout(&error, theme::role::meta());
            scene::draw_layout(
                scene,
                root,
                &layout,
                laid.x,
                y,
                Some(theme::p().close_hover),
            );
        }
        if let Some(hint) = laid.hint {
            let layout = self
                .engine
                .simple_layout("Add a few words so we can help", theme::role::meta());
            scene::draw_layout(
                scene,
                root,
                &layout,
                hint.x,
                hint.y,
                Some(theme::p().text_dim),
            );
        }
        let send_label = "Send";
        let cancel_hot = self.hover == Some(Hit::Cancel);
        let send_hot = self.hover == Some(Hit::Send) && self.can_send();
        draw_button(
            scene,
            root,
            &mut self.engine,
            laid.cancel,
            "Cancel",
            ButtonRole::Secondary,
            cancel_hot,
            false,
        );
        draw_button(
            scene,
            root,
            &mut self.engine,
            laid.send,
            send_label,
            ButtonRole::Primary,
            send_hot,
            false,
        );
    }

    fn paint_header(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        let title = self
            .engine
            .simple_layout("Send feedback", theme::role::subtitle());
        scene::draw_layout(
            scene,
            root,
            &title,
            laid.x,
            laid.header_y,
            Some(theme::p().text),
        );
        let sub = self.engine.simple_layout(
            "Goes straight to the people building Koru. We read everything.",
            theme::role::meta(),
        );
        scene::draw_layout(
            scene,
            root,
            &sub,
            laid.x,
            laid.header_y + title.height() + 4.0,
            Some(theme::p().text_dim),
        );
    }

    fn paint_kinds(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        let label = self
            .engine
            .simple_layout("What would you like to tell us?", theme::role::meta());
        scene::draw_layout(
            scene,
            root,
            &label,
            laid.x,
            laid.kind_label_y,
            Some(theme::p().text),
        );
        for (i, kind) in Kind::all().into_iter().enumerate() {
            let r = laid.kinds[i];
            let selected = self.kind == kind;
            let hover = self.hover == Some(Hit::Kind(kind));
            let fill = if selected {
                theme::p().row_selected
            } else {
                theme::p().bg
            };
            let shape = scene::rrect(r.x, r.y, r.w, r.h, 8.0);
            scene::fill(scene, root, fill, &shape);
            let border = if selected {
                theme::p().accent
            } else if hover {
                theme::p().text_dim
            } else {
                theme::p().panel_border
            };
            scene::stroke(
                scene,
                root,
                border,
                if selected { 2.0 } else { 1.0 },
                &shape,
            );
            let glyph = self.engine.simple_layout(kind.emoji(), KIND_EMOJI);
            let text = self.engine.simple_layout_wrapped(
                kind.label(),
                theme::role::row(),
                (r.w - 24.0).max(1.0),
            );
            let content_h = glyph.height() + 6.0 + text.height();
            let top = r.y + (r.h - content_h) * 0.5;
            scene::draw_layout(
                scene,
                root,
                &glyph,
                r.x + (r.w - glyph.width()) * 0.5,
                top,
                None,
            );
            scene::draw_layout(
                scene,
                root,
                &text,
                r.x + (r.w - text.width()) * 0.5,
                top + glyph.height() + 6.0,
                Some(theme::p().text),
            );
        }
    }

    fn paint_composer(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        let r = laid.composer;
        let focused = self.focus == Focus::Composer;
        let shape = scene::rrect(r.x, r.y, r.w, r.h, 8.0);
        scene::fill(scene, root, theme::p().bg, &shape);
        scene::stroke(
            scene,
            root,
            if focused {
                theme::p().accent
            } else {
                theme::p().panel_border
            },
            1.0,
            &shape,
        );
        self.composer.draw(
            scene,
            root,
            &mut self.engine,
            r,
            Some(self.kind.placeholder()),
            focused,
        );
    }

    fn paint_kind_fields(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        if let Some(y) = laid.freq_label_y {
            let label = self
                .engine
                .simple_layout("How often does this happen?", theme::role::meta());
            scene::draw_layout(scene, root, &label, laid.x, y, Some(theme::p().text));
            for (freq, rect) in &laid.freqs {
                koru_ui::Chip::new(freq.label(), self.frequency == Some(*freq)).draw(
                    scene,
                    root,
                    &mut self.engine,
                    *rect,
                    self.hover == Some(Hit::Frequency(*freq)),
                    false,
                );
            }
        }
        if let (Some(y), Some(rect)) = (laid.expected_label_y, laid.expected) {
            let label = self
                .engine
                .simple_layout("What did you expect to happen?", theme::role::meta());
            scene::draw_layout(scene, root, &label, laid.x, y, Some(theme::p().text));
            self.expected.draw(
                scene,
                root,
                &mut self.engine,
                rect,
                InputStyle::inline(),
                Some("Optional"),
                self.focus == Focus::Expected,
            );
        }
        if let Some(y) = laid.importance_label_y {
            let label = self
                .engine
                .simple_layout("How important would this be to you?", theme::role::meta());
            scene::draw_layout(scene, root, &label, laid.x, y, Some(theme::p().text));
            for (importance, rect) in &laid.importances {
                let on = self.importance == Some(*importance);
                const RADIO: f32 = 14.0;
                let cy = rect.y + (rect.h - RADIO) * 0.5;
                scene::stroke(
                    scene,
                    root,
                    theme::p().text_dim,
                    1.0,
                    &scene::rrect(rect.x, cy, RADIO, RADIO, RADIO * 0.5),
                );
                if on {
                    scene::fill(
                        scene,
                        root,
                        theme::p().accent,
                        &scene::rrect(rect.x + 3.0, cy + 3.0, 8.0, 8.0, 4.0),
                    );
                }
                let text = self
                    .engine
                    .simple_layout(importance.label(), theme::role::row());
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &text,
                    rect.x + 22.0,
                    koru_ui::text_field::baseline_centred_y(&text, rect.y, rect.h),
                    Some(theme::p().text),
                );
            }
        }
    }

    fn paint_screenshots(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        ActionButton::new("+ Add screenshot", ActionStyle::Ghost, None).draw(
            scene,
            root,
            &mut self.engine,
            laid.add_shot,
            self.hover == Some(Hit::AddScreenshot),
        );
        let hint = self
            .engine
            .simple_layout("or paste an image", theme::role::meta());
        scene::draw_layout(
            scene,
            root,
            &hint,
            laid.paste_hint_x,
            laid.add_shot.y + (laid.add_shot.h - hint.height()) * 0.5,
            Some(theme::p().text_dim),
        );
        let [a, b, c, d, _, _] = root.as_coeffs();
        let scale = a.hypot(b).max(c.hypot(d));
        for (i, geom) in laid.shots.iter().enumerate() {
            let thumb = geom.thumb;
            scene::fill(
                scene,
                root,
                theme::p().chip_bg,
                &scene::rrect(thumb.x, thumb.y, thumb.w, thumb.h, 6.0),
            );
            let wanted = ((thumb.w.max(thumb.h) as f64 * scale).ceil().max(1.0)) as u32;
            if let Some(brush) = self.thumbs.scaled(&i.to_string(), wanted) {
                let iw = brush.image.width.max(1) as f32;
                let ih = brush.image.height.max(1) as f32;
                let fit = (thumb.w / iw).min(thumb.h / ih);
                let dw = iw * fit;
                let dh = ih * fit;
                let dx = thumb.x + (thumb.w - dw) * 0.5;
                let dy = thumb.y + (thumb.h - dh) * 0.5;
                scene.push_clip_layer(
                    Fill::NonZero,
                    root,
                    &scene::rrect(thumb.x, thumb.y, thumb.w, thumb.h, 6.0),
                );
                let transform = root
                    * Affine::translate((dx as f64, dy as f64))
                    * Affine::scale_non_uniform(dw as f64 / iw as f64, dh as f64 / ih as f64);
                scene.draw_image(brush.as_ref(), transform);
                scene.pop_layer();
            }
            if let Some(shot) = self.attachments.get(i) {
                let name = self
                    .engine
                    .elided(&shot.filename, theme::role::hint(), thumb.w);
                let laid_name = self.engine.simple_layout(&name, theme::role::hint());
                scene::draw_layout(
                    scene,
                    root,
                    &laid_name,
                    thumb.x + (thumb.w - laid_name.width()) * 0.5,
                    geom.name.y,
                    Some(theme::p().text_dim),
                );
            }
            let over = matches!(self.hover, Some(Hit::Shot(n) | Hit::RemoveShot(n)) if n == i);
            if over {
                self.paint_shot_close(
                    scene,
                    root,
                    geom.close,
                    self.hover == Some(Hit::RemoveShot(i)),
                );
            }
        }
    }

    fn paint_shot_close(&mut self, scene: &mut Scene, root: Affine, close: Rect, hovered: bool) {
        let p = theme::p();
        let shape = scene::rrect(close.x, close.y, close.w, close.h, 4.0);
        scene::fill(scene, root, p.bg, &shape);
        if hovered {
            scene::fill(scene, root, p.hover_tint, &shape);
        }
        scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &shape);
        let glyph = self.engine.icon_layout(icons::WINDOW_CLOSE, 11.0);
        scene::draw_layout(
            scene,
            root,
            &glyph,
            close.x + (close.w - glyph.width()) * 0.5,
            close.y + (close.h - glyph.height()) * 0.5,
            Some(p.text),
        );
    }

    fn paint_logs(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        let y = laid.logs.y;
        draw_check(scene, root, laid.x, y, self.include_logs);
        let label = self
            .engine
            .simple_layout("Include recent application logs", theme::role::row());
        scene::draw_layout(scene, root, &label, laid.x + 26.0, y, Some(theme::p().text));
        let hint = self.engine.simple_layout(
            "Only if we ask, or if the problem is hard to reproduce.",
            theme::role::meta(),
        );
        scene::draw_layout(
            scene,
            root,
            &hint,
            laid.x + 26.0,
            y + 18.0,
            Some(theme::p().text_dim),
        );
    }

    fn paint_more(&mut self, scene: &mut Scene, root: Affine, laid: &Laid) {
        let caret = if self.more_open {
            icons::CARET_DOWN
        } else {
            icons::CARET_RIGHT
        };
        let icon = self.engine.icon_layout(caret, 12.0);
        scene::draw_layout(
            scene,
            root,
            &icon,
            laid.x,
            laid.more.y + 4.0,
            Some(theme::p().text_dim),
        );
        let label = self
            .engine
            .simple_layout("More details", theme::role::meta());
        scene::draw_layout(
            scene,
            root,
            &label,
            laid.x + 16.0,
            laid.more.y,
            Some(theme::p().text_dim),
        );
        if let Some(contact) = laid.contact {
            draw_check(scene, root, laid.x, contact.y, self.contact);
            let label = self
                .engine
                .simple_layout("Koru can contact me about this", theme::role::row());
            scene::draw_layout(
                scene,
                root,
                &label,
                laid.x + 26.0,
                contact.y,
                Some(theme::p().text),
            );
        }
        if let Some(email) = laid.email {
            self.email.draw(
                scene,
                root,
                &mut self.engine,
                email,
                InputStyle::inline(),
                Some("Email"),
                self.focus == Focus::Email,
            );
        }
    }
}

fn decode_shot(bytes: &[u8]) -> Option<ImageBrush> {
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(ImageBrush::new(ImageData {
        data: Blob::new(Arc::new(rgba.into_raw())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }))
}

fn draw_check(scene: &mut Scene, root: Affine, x: f32, y: f32, on: bool) {
    let r = scene::rrect(x, y + 2.0, 16.0, 16.0, 3.0);
    if on {
        scene::fill(scene, root, theme::p().accent, &r);
    } else {
        scene::stroke(scene, root, theme::p().text_dim, 1.0, &r);
    }
}

impl FeedbackView {
    #[cfg(test)]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    #[cfg(test)]
    pub fn set_message(&mut self, text: &str) {
        self.composer.set_text(&mut self.engine, text);
        self.persist();
    }

    #[cfg(test)]
    pub fn message(&self) -> String {
        self.composer.value()
    }

    #[cfg(test)]
    pub fn raw_message(&self) -> String {
        self.composer.raw_text().to_string()
    }

    #[cfg(test)]
    pub fn contact(&self) -> bool {
        self.contact
    }

    #[cfg(test)]
    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
    }

    #[cfg(test)]
    pub fn can_send_now(&self) -> bool {
        self.can_send()
    }

    #[cfg(test)]
    pub fn press_enter(&mut self) {
        let _ = self.composer.key(
            &mut self.engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        self.persist();
    }

    #[cfg(test)]
    fn form_layout(&mut self) -> Laid {
        self.layout()
    }

    #[cfg(test)]
    pub fn click_kind(&mut self, kind: Kind) {
        self.kind = kind;
        self.persist();
    }

    #[cfg(test)]
    pub fn toggle_contact(&mut self) {
        self.contact = !self.contact;
        self.persist();
    }

    #[cfg(test)]
    pub fn set_email(&mut self, email: &str) {
        self.email = TextInput::with_text(&mut self.engine, theme::role::row(), email);
        self.persist();
    }
}
