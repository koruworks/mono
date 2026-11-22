// SPDX-License-Identifier: MPL-2.0

//! An OCR tab owns a private editor; only explicit Save writes to the Store.
use crate::result_document::ResultDocument;
use koru_core::{DocumentId, WorkspaceId, schema::WorkspaceDoc};
use koru_draw::{
    ImageCache, el,
    geometry::Rect,
    text::TextEngine,
    theme::{self, Space},
};
use koru_i18n::{Key as Label, t};
use koru_store_wire::readings::CorrectReading;
use koru_ui::{ActionButton, ActionStyle};
use koru_workspace_host::{
    View,
    reading_details::{ReadingDetails, plain_snapshot},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use vello::{Scene, kurbo::Affine};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, ModifiersState, NamedKey},
    window::CursorIcon,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Edit,
    Save,
    Cancel,
    Reset,
    Refresh,
    Export,
    Version,
}
impl Action {
    fn label(self) -> Label {
        match self {
            Self::Edit => Label::OcrEdit,
            Self::Save => Label::OcrSave,
            Self::Cancel => Label::OcrCancel,
            Self::Reset => Label::OcrReset,
            Self::Refresh => Label::OcrRefresh,
            Self::Export => Label::OcrExport,
            Self::Version => Label::OcrVersion,
        }
    }
    fn key(self) -> el::Key {
        el::Key::surface(
            "OCR",
            match self {
                Self::Edit => "edit",
                Self::Save => "save",
                Self::Cancel => "cancel",
                Self::Reset => "reset",
                Self::Refresh => "refresh",
                Self::Export => "export",
                Self::Version => "version",
            },
        )
    }
}

struct Button {
    label: Label,
    width: f32,
}
impl el::Leaf for Button {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (self.width, ActionButton::H)
    }
    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _: &el::FrameCx,
        state: el::ElState,
    ) {
        ActionButton::new(
            t(crate::format::lang(), self.label),
            ActionStyle::Secondary,
            None,
        )
        .draw_focused(scene, root, engine, at, state.hovered, state.focused);
    }
}

pub struct OcrView {
    pub file: DocumentId,
    pub workspace: Option<WorkspaceId>,
    details: Option<ReadingDetails>,
    document: Option<ResultDocument>,
    engine: TextEngine,
    cache: el::TextCache,
    frame: Option<el::Frame>,
    viewport: (f32, f32),
    band: f32,
    focus: Option<Action>,
    hot: Option<el::Key>,
    notice: Option<Label>,
    receiver: Option<mpsc::Receiver<Result<ReadingDetails, ()>>>,
    saving: bool,
    keep_draft: bool,
    version: usize,
    request: Option<koru_workspace_host::PaneRequest>,
}

impl OcrView {
    pub(crate) fn controls(&self) -> Vec<(&'static str, Label, Rect)> {
        self.actions()
            .into_iter()
            .filter_map(|action| {
                Some((
                    action.key().part,
                    action.label(),
                    self.frame.as_ref()?.node(action.key())?.rect,
                ))
            })
            .collect()
    }
    pub(crate) fn focused_control(&self) -> Option<&'static str> {
        self.focus.map(|a| a.key().part)
    }
    pub(crate) fn accessible_action(&mut self, part: &str, click: bool) -> bool {
        let Some(action) = self.actions().into_iter().find(|a| a.key().part == part) else {
            return false;
        };
        self.focus = Some(action);
        if click {
            self.activate(action);
        }
        true
    }
    pub(crate) fn revise(&self, revision: &mut koru_workspace_host::accessibility::Revision) {
        for (part, _, rect) in self.controls() {
            revision.str(part);
            revision.f32(rect.x).f32(rect.y).f32(rect.w).f32(rect.h);
        }
        revision.str(self.focused_control().unwrap_or(""));
        revision.str(self.notice.map_or("", |key| t(crate::format::lang(), key)));
    }
    pub fn new(file: DocumentId, workspace: Option<WorkspaceId>) -> Self {
        Self {
            file,
            workspace,
            details: None,
            document: None,
            engine: TextEngine::new(),
            cache: Default::default(),
            frame: None,
            viewport: (0.0, 0.0),
            band: 0.0,
            focus: None,
            hot: None,
            notice: Some(Label::ReadingLoading),
            receiver: None,
            saving: false,
            keep_draft: false,
            version: 0,
            request: None,
        }
    }
    fn editing(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(ResultDocument::is_editable)
    }
    pub fn is_dragging(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(ResultDocument::is_dragging)
    }
    pub fn request_close(&mut self) -> bool {
        if self.saving || self.editing() {
            self.notice = Some(if self.saving {
                Label::OcrSaving
            } else {
                Label::OcrUnsaved
            });
            true
        } else {
            false
        }
    }
    pub fn receive(&mut self, details: Result<ReadingDetails, String>) {
        // Reopening the menu must never replace a draft or a pending save.
        if self.editing() || self.receiver.is_some() {
            return;
        }
        match details {
            Ok(details) => self.install(details, false),
            Err(_) => self.notice = Some(Label::ReadingFetchFailed),
        }
    }
    fn install(&mut self, details: ReadingDetails, keep_draft: bool) {
        self.version = details
            .corrections
            .versions
            .iter()
            .position(|v| Some(&v.id) == details.corrections.selected.as_ref())
            .unwrap_or(0);
        if !keep_draft {
            self.document = ResultDocument::of(&details.snapshot, false);
        }
        self.notice = if keep_draft {
            Some(Label::OcrDraftRetained)
        } else if details.corrections.versions.len() > 1 {
            Some(Label::OcrConflict)
        } else {
            None
        };
        self.details = Some(details);
        self.resize();
    }
    fn actions(&self) -> Vec<Action> {
        if self.receiver.is_some() || self.details.is_none() {
            return vec![];
        }
        if self.editing() {
            vec![Action::Save, Action::Cancel, Action::Refresh]
        } else {
            let mut actions = vec![Action::Edit, Action::Refresh, Action::Export];
            if self
                .details
                .as_ref()
                .is_some_and(|d| !d.corrections.versions.is_empty())
            {
                actions.push(Action::Reset);
            }
            if self
                .details
                .as_ref()
                .is_some_and(|d| d.corrections.versions.len() > 1)
            {
                actions.push(Action::Version);
            }
            actions
        }
    }
    fn activate(&mut self, action: Action) {
        if !self.actions().contains(&action) {
            return;
        }
        match action {
            Action::Edit => {
                if let Some(details) = &self.details {
                    self.document = ResultDocument::of(&details.snapshot, true);
                    self.focus = None;
                    self.notice = None;
                    self.resize();
                }
            }
            Action::Cancel => {
                if let Some(details) = &self.details {
                    self.document = ResultDocument::of(&details.snapshot, false);
                    self.notice = None;
                    self.resize();
                }
            }
            Action::Save => {
                if let Some(text) = self.document.as_ref().and_then(ResultDocument::plain_text) {
                    self.request(Some(Some(text)));
                } else {
                    self.notice = Some(Label::OcrPlainOnly);
                }
            }
            Action::Reset => self.request(Some(None)),
            Action::Refresh => self.request(None),
            Action::Export => {
                self.request = Some(koru_workspace_host::PaneRequest::SaveReading(
                    koru_workspace_host::reading_file::ReadingTarget::File(self.file),
                ));
            }
            Action::Version => {
                let Some(details) = self.details.as_mut() else {
                    return;
                };
                self.version = (self.version + 1) % details.corrections.versions.len();
                let version = &details.corrections.versions[self.version];
                let text = version.text.as_deref().unwrap_or(&details.extracted_text);
                // This is an explicit review action, never frame work.
                if let Ok(snapshot) = plain_snapshot(text) {
                    details.snapshot = snapshot;
                    self.document = ResultDocument::of(&details.snapshot, false);
                    self.notice = Some(Label::OcrConflict);
                    self.resize();
                }
            }
        }
    }
    fn request(&mut self, correction: Option<Option<String>>) {
        let Some(details) = self.details.as_ref() else {
            return;
        };
        let Some((workspace, hash)) = details.target.clone() else {
            return;
        };
        let heads = details
            .corrections
            .versions
            .iter()
            .map(|v| v.id.clone())
            .collect();
        let name = details.name.clone();
        self.saving = correction.is_some();
        self.keep_draft = !self.saving && self.editing();
        self.notice = Some(if self.saving {
            Label::OcrSaving
        } else {
            Label::ReadingLoading
        });
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        if cfg!(test) {
            return;
        }
        let root = koru_workspace_host::cli::root_from_args();
        let lang = crate::format::lang();
        std::thread::spawn(move || {
            let result = (|| {
                let scopes = &["store.read_documents", "store.edit_documents"];
                let client = match root {
                    Some(root) => koru_api_client::Client::connect_at_with_scopes(
                        koru_channel::segment(),
                        root,
                        scopes,
                    ),
                    None => koru_api_client::Client::connect_with_scopes(
                        koru_channel::segment(),
                        scopes,
                    ),
                }
                .map_err(|_| ())?;
                let saved = correction
                    .map(|text| {
                        client.correct_reading(
                            workspace,
                            &hash,
                            &CorrectReading {
                                expected_heads: heads,
                                text,
                            },
                        )
                    })
                    .transpose()
                    .map_err(|_| ())?;
                let answer = client.reading(workspace, &hash).map_err(|_| ())?;
                let mut details =
                    koru_workspace_host::reading_details::format(name, answer.as_ref(), lang);
                details.target = Some((workspace, hash));
                // The cache may still be catching up. The durable write's answer
                // is authoritative for the correction we just saved.
                if let Some(saved) = saved {
                    details.corrections = saved;
                    let effective = details
                        .corrections
                        .versions
                        .iter()
                        .find(|v| Some(&v.id) == details.corrections.selected.as_ref())
                        .and_then(|v| v.text.as_deref())
                        .unwrap_or(&details.extracted_text);
                    details.snapshot = plain_snapshot(effective).map_err(|_| ())?;
                }
                Ok(details)
            })();
            let _ = tx.send(result);
        });
    }
    fn poll(&mut self) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(_) => Err(()),
        };
        self.receiver = None;
        let saving = std::mem::take(&mut self.saving);
        match result {
            Ok(details) => {
                self.install(details, self.keep_draft);
                if saving {
                    self.notice = Some(Label::OcrSaved);
                }
            }
            Err(_) => {
                self.notice = Some(if saving {
                    Label::OcrSaveFailed
                } else {
                    Label::ReadingFetchFailed
                })
            }
        }
        self.keep_draft = false;
        true
    }
    fn resize(&mut self) {
        if let Some(document) = &mut self.document {
            document.set_viewport(self.viewport.0, (self.viewport.1 - self.band).max(0.0));
        }
    }
    fn tree(&mut self) -> el::El {
        let lang = crate::format::lang();
        let mut buttons = el::row("OCR actions").gap(Space::S2).items_start();
        for action in self.actions() {
            let label = action.label();
            let width = ActionButton::new(t(lang, label), ActionStyle::Secondary, None)
                .width(&mut self.engine);
            buttons = buttons.child(
                el::leaf("OCR action", Button { label, width })
                    .semantic()
                    .id(action.key())
                    .name(label)
                    .semantics(el::Role::Button)
                    .hit()
                    .focusable()
                    .action(el::Action::Click),
            );
        }
        let corrected = self
            .details
            .as_ref()
            .and_then(|d| d.corrections.versions.get(self.version))
            .is_some_and(|v| v.text.is_some());
        let status = self.notice.unwrap_or(if corrected {
            Label::OcrCorrected
        } else {
            Label::OcrOriginal
        });
        el::col("OCR text tab").child(
            el::col("OCR toolbar")
                .pad(Space::S4)
                .gap(Space::S2)
                .child(buttons)
                .child(el::text(t(lang, status)).role(theme::roles::HINT).wrap()),
        )
    }
}

impl View for OcrView {
    fn take_pane_request(&mut self) -> Option<koru_workspace_host::PaneRequest> {
        self.request.take()
    }

    fn key(
        &mut self,
        _: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if matches!(key, Key::Named(NamedKey::Escape | NamedKey::Tab)) {
            let actions = self.actions();
            if !actions.is_empty() {
                self.focus = match self
                    .focus
                    .and_then(|a| actions.iter().position(|v| *v == a))
                {
                    None => actions.first().copied(),
                    Some(0) if mods.shift_key() => None,
                    Some(i) if mods.shift_key() => actions.get(i - 1).copied(),
                    Some(i) => actions.get(i + 1).copied(),
                };
                return true;
            }
        }
        if let Some(action) = self.focus {
            if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) {
                self.activate(action);
                return true;
            }
            return false;
        }
        if self.saving {
            return true;
        }
        // OCR corrections are plain text, including content pasted from a
        // browser or another Koru document. Use the editor's existing plain paste.
        let mut mods = mods;
        if matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("v"))
            && (mods.control_key() || mods.super_key())
        {
            mods.set(ModifiersState::SHIFT, true);
        }
        self.document
            .as_mut()
            .is_some_and(|d| d.key(key, text, mods))
    }
    fn mouse_down(&mut self, _: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        if let Some(hit) = self.frame.as_ref().and_then(|f| f.hit_at(x, y)) {
            if let Some(action) = self.actions().into_iter().find(|a| a.key() == hit) {
                self.focus = Some(action);
                self.activate(action);
                return true;
            }
        }
        self.focus = None;
        !self.saving
            && y >= self.band
            && self
                .document
                .as_mut()
                .is_some_and(|d| d.mouse_down(x, y - self.band, ctrl))
    }
    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.frame.as_ref().and_then(|f| f.hit_at(x, y));
        let changed = self.hot != hot;
        self.hot = hot;
        self.document
            .as_mut()
            .is_some_and(|d| d.mouse_move(x, y - self.band))
            || changed
    }
    fn mouse_up(&mut self, _: &mut WorkspaceDoc) -> bool {
        self.document.as_mut().is_some_and(ResultDocument::mouse_up)
    }
    fn mouse_double_click(&mut self, _: &WorkspaceDoc, x: f32, y: f32) -> bool {
        y >= self.band
            && self
                .document
                .as_mut()
                .is_some_and(|d| d.mouse_double_click(x, y - self.band))
    }
    fn set_mods(&mut self, mods: ModifiersState) {
        if let Some(d) = &mut self.document {
            d.set_mods(mods);
        }
    }
    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if let Some(d) = &mut self.document {
            d.scroll_by(delta);
        }
    }
    fn cursor_icon(&self) -> CursorIcon {
        if self.hot.is_some() {
            CursorIcon::Pointer
        } else {
            CursorIcon::Text
        }
    }
    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.resize();
    }
    fn adopt(&mut self, _: &WorkspaceDoc) {}
    fn reseat(&mut self, _: &WorkspaceDoc) {}
    fn invalidate(&mut self) {
        self.cache.clear();
        if let Some(d) = &mut self.document {
            d.invalidate();
        }
    }
    fn animating(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(ResultDocument::is_animating)
    }
    fn maintenance_deadline(&self) -> Option<Instant> {
        self.receiver
            .as_ref()
            .map(|_| Instant::now() + Duration::from_millis(30))
    }
    fn maintain(&mut self, _: &mut WorkspaceDoc, _: Instant) -> bool {
        self.poll()
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        _: &koru_workspace::Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        let _scope = koru_draw::inspect::scope(
            "OCR tab",
            "container",
            root,
            koru_draw::scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
            |_| {},
        );
        let tree = self.tree();
        let frame = tree.layout(
            &mut self.engine,
            &mut self.cache,
            Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
            el::FrameCx::root().with_interaction(el::Interaction {
                hot: self.hot,
                focused: self.focus.map(Action::key),
                pressed: None,
            }),
        );
        frame.paint(&tree, scene, root, &mut self.engine);
        self.band = frame.rect_of("OCR toolbar").map_or(0.0, |r| r.y + r.h);
        self.frame = Some(frame);
        self.resize();
        if let Some(document) = &mut self.document {
            let _scope = koru_draw::inspect::scope(
                "OCR document",
                "container",
                root,
                koru_draw::scene::rect(
                    0.0,
                    self.band,
                    self.viewport.0,
                    (self.viewport.1 - self.band).max(0.0),
                ),
                |_| {},
            );
            scene.push_clip_layer(
                vello::peniko::Fill::NonZero,
                root,
                &vello::kurbo::Rect::new(
                    0.0,
                    self.band as f64,
                    self.viewport.0 as f64,
                    self.viewport.1 as f64,
                ),
            );
            document.draw(
                scene,
                images,
                root * Affine::translate((0.0, self.band as f64)),
                caret_on,
                (pointer.0, pointer.1 - self.band),
            );
            scene.pop_layer();
        }
    }
}

#[cfg(test)]
mod tests;
