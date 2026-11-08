// SPDX-License-Identifier: MPL-2.0

//! **A run's result as a tab**, the way every other document in Koru opens.
//!
//! Two surfaces name a result and ask this window to open it — a run's
//! Result view, and the Artifacts column — and this is the seam that holds
//! the one surface they both open. It is thinner than `run_view.rs` because
//! there is less to be a seam *for*: the tab reads one artifact version
//! through its own worker, revises it there, and needs nothing from this
//! window's session connection — no lease, no handover, no request.
//!
//! **The tab is not a second way to read a result; it is the only way.**
//! Opening one used to replace the run pane's content with the document and
//! leave a *back* button as the only way out (ADR-368), and the Artifacts
//! column had a reader of its own that did the same thing and could also
//! revise it. Both are gone (ADR-369). What is left is
//! [`koru_artifacts_ui::ArtifactTab`], which reads *and* saves, and lives in
//! the crate that owns the register path a save goes through.
//!
//! ## Two halves, and the line between them
//!
//! A Markdown result **is** a document, so it is drawn by Koru's document
//! view — the one every page in the library opens in — over a private
//! in-memory workspace nobody owns ([`crate::result_document`], ADR-372).
//! The tab keeps everything around it: the result's name, which version it
//! is and how big, *Edit*, *Save as version N*, the sentences saying what a
//! save would do, and every state that is not a document — still reading,
//! held on another device, would not read, not Markdown at all.
//!
//! `ArtifactTab::document_for_host` is the whole of the rule and it is asked
//! once per frame here. `Some` means the page below is the document view's;
//! `None` means the tab is showing something of its own in that space and
//! this view is a pass-through, exactly as it was before.
//!
//! ## Editing is the same page (ADR-378)
//!
//! *Edit* used to swap the page for a composer full of Markdown source. It
//! does not any more: it rebuilds the page with typing allowed, and *Save*
//! exports that document back to Markdown and registers it as the next
//! version, through the register path that was always there.
//!
//! That leaves this file with one job it did not have — **settling**. The
//! tab holds the decision and the sentences; the page holds the bytes and is
//! the only thing that knows whether anybody typed; and neither can reach
//! the other. [`ResultView::settle`] is the single place the two are put
//! back in agreement after any input: rebuild the page when its editability
//! no longer matches the tab's, answer a pending ask to save with the
//! document's own Markdown, and push down whether the page has moved. One
//! function, called from every input path, so the answer cannot differ by
//! which key was pressed.

use koru_artifacts_ui::{ArtifactTab, OpenedArtifact};
use koru_core::schema::WorkspaceDoc;
use koru_draw::ImageCache;
use koru_workspace_host::{Settings, View, WorkspaceStat};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use crate::result_document::ResultDocument;

#[cfg(test)]
pub(crate) mod tests;

pub struct ResultView {
    surface: ArtifactTab,
    /// The version's own document, when there is one to draw. Rebuilt from
    /// the tab whenever the bytes it holds change — a read landing, a save
    /// making the next version — and dropped when they stop being a
    /// document, so the page on screen can never outlive the version it was
    /// made from.
    document: Option<ResultDocument>,
    /// How tall the tab's own chrome is, from the last layout: where the
    /// document starts, and the offset every press and every draw below it
    /// is expressed in.
    band: f32,
    viewport: (f32, f32),
}

impl Default for ResultView {
    fn default() -> Self {
        Self::new()
    }
}

impl ResultView {
    pub fn new() -> Self {
        // **Under test it dials nothing**, for `run_view.rs`'s reason: a
        // dependency is never compiled with `cfg(test)`, so a guard inside
        // `koru-artifacts-ui` could not stop this binary's suite from asking
        // whatever gateway the machine has running. That is what this
        // binary's `no-reader` dev-dependency on the crate is for, and
        // `ArtifactTab::new` honours both.
        let mut surface = match cfg!(test) {
            true => ArtifactTab::without_reader(),
            false => ArtifactTab::new(koru_workspace_host::cli::root_from_args()),
        };
        surface.set_lang(crate::format::lang());
        Self {
            surface,
            document: None,
            band: 0.0,
            viewport: (0.0, 0.0),
        }
    }

    /// Show this result, and ask for its bytes.
    pub fn open(&mut self, opened: OpenedArtifact) {
        self.surface.open(opened);
        self.document = None;
    }

    /// The version this tab holds — how the window finds the tab already
    /// open on a result rather than opening a second one on it.
    ///
    /// **It moves when a save lands.** Saving registers the next version and
    /// the tab is then reading that one, so the strip keeps one tab per
    /// version whichever surface opened it and however many times it has
    /// been revised.
    pub fn version(&self) -> Option<koru_delegate_core::ArtifactVersionId> {
        self.surface.version()
    }

    /// Take whatever the surface's link has. Answers whether to repaint.
    pub fn poll(&mut self) -> bool {
        let changed = self.surface.poll();
        if changed {
            self.rebuild_document();
            self.settle();
        }
        changed
    }

    pub fn set_lang(&mut self, lang: koru_i18n::Lang) {
        self.surface.set_lang(lang);
    }

    /// The immutable version's producing run, when it had one.
    pub fn producing_run(&self) -> Option<koru_delegate_core::RunId> {
        self.surface.producing_run()
    }

    /// Keep the run icon honest as the live/Trash run catalogs move.
    pub fn set_run_available(&mut self, available: bool) -> bool {
        self.surface.set_run_available(available)
    }

    /// Take one click on the producing-run icon for the window to route.
    pub fn take_wants_run(&mut self) -> Option<koru_delegate_core::RunId> {
        self.surface.take_wants_run()
    }

    /// Whether a selection drag in the page is in flight, so the container
    /// keeps forwarding pointer moves that have left the content rect —
    /// which is what a person dragging a selection past the edge is doing.
    pub fn is_dragging(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(ResultDocument::is_dragging)
    }

    /// **Build the page from whatever the tab now holds.**
    ///
    /// Called on every path that can change the bytes — a read landing, a
    /// save's answer, a version being opened — rather than per frame,
    /// because building a document is proportional to it. The snapshot comes
    /// from the worker that already built it, so nothing is parsed here.
    fn rebuild_document(&mut self) {
        let editable = self.surface.is_editing();
        let built = self
            .surface
            .document_for_host()
            .map(|document| ResultDocument::of(document.snapshot(), editable));
        match built {
            // A document, and it opened.
            Some(Some(document)) => {
                self.document = Some(document);
                self.resize_document();
            }
            // Bytes the tab calls a document that this view could not open.
            // Dropping the page leaves the tab saying what it can, rather
            // than an empty column under a version's name.
            Some(None) | None => self.document = None,
        }
    }

    /// **Put the tab and the page back in agreement**, after any input that
    /// either of them answered.
    ///
    /// Three things, in this order, because each depends on the one before:
    /// the page is rebuilt when what the tab wants and what is on screen no
    /// longer match; a pending ask to save is answered with the document's
    /// own Markdown; and the tab is told whether the page has been typed
    /// into, which is the only thing it needs from the page per frame.
    fn settle(&mut self) {
        self.sync_document();
        if self.surface.take_save_request() {
            match self.document.as_ref().and_then(ResultDocument::markdown) {
                // The export is proportional to the document and happens
                // here, once, on the press — never in a frame.
                Some(markdown) => self.surface.save_as_next_version(markdown),
                // A page that will not export is a refusal said in the same
                // place a refusal from the store is said, rather than a save
                // that quietly writes something else. It is a code with no
                // sentence of its own, on purpose: the page was built from a
                // document that had already exported once, so this is the
                // shape of a failure nobody has a better sentence for than
                // *nothing was saved and the version is unchanged*.
                None => self.surface.refuse_save("artifact_export_failed"),
            }
        }
        let edited = self
            .document
            .as_ref()
            .is_some_and(ResultDocument::is_edited);
        self.surface.set_document_edited(edited);
    }

    /// Build, drop or rebuild the page after something the tab did —
    /// pressing *Edit* opens the same bytes for typing, and *Stop editing*
    /// closes them again.
    ///
    /// Only when the two disagree, on either count: whether there is a page
    /// at all, and whether it may be typed into. The bytes themselves move
    /// on a read or a save, and both of those come through [`Self::poll`].
    fn sync_document(&mut self) {
        let wanted = self.surface.document_for_host().is_some();
        let editable = self.surface.is_editing();
        let matches = match self.document.as_ref() {
            Some(document) => wanted && document.is_editable() == editable,
            None => !wanted,
        };
        if !matches {
            self.rebuild_document();
        }
    }

    /// Push the split of the viewport down: chrome above, page below.
    /// Whether this result shows a built version as a page — the one case
    /// that holds a document to lay out.
    pub(crate) fn has_document(&self) -> bool {
        self.document.is_some()
    }

    /// For a tab that is not shown: the chrome is invalidated as always (it
    /// is cheap), the page only marked. `set_viewport` lays the page out when
    /// the tab is next sized, through `resize_document`.
    pub(crate) fn invalidate_deferred(&mut self) {
        self.surface.invalidate();
        if let Some(document) = self.document.as_mut() {
            document.invalidate_deferred();
        }
    }

    fn resize_document(&mut self) {
        let (w, h) = self.viewport;
        let (column_x, column_w) = ResultDocument::text_column(w);
        self.surface.set_text_column(column_x, column_w);
        self.band = self.surface.document_top();
        let band = self.band;
        if let Some(document) = self.document.as_mut() {
            document.set_viewport(w, (h - band).max(0.0));
        }
    }

    /// The page, and where it sits, when there is one on screen.
    ///
    /// The band is read out of the tab's own layout rather than remembered,
    /// so a press can never be aimed at where the chrome ended one frame
    /// ago: `document_top` relays out only when something moved.
    fn page(&mut self) -> Option<(&mut ResultDocument, f32)> {
        self.document.as_ref()?;
        let band = self.surface.document_top();
        self.band = band;
        self.document.as_mut().map(|document| (document, band))
    }

    #[cfg(test)]
    pub(crate) fn surface_mut(&mut self) -> &mut ArtifactTab {
        &mut self.surface
    }

    /// The page this result opened as, for a test to read it out of.
    #[cfg(test)]
    pub(crate) fn document(&self) -> Option<&ResultDocument> {
        self.document.as_ref()
    }

    /// Fold in a version's content the test drove in itself, and rebuild the
    /// page from it — the seam `poll` uses, without the worker.
    #[cfg(test)]
    pub(crate) fn take_content(
        &mut self,
        version: koru_delegate_core::ArtifactVersionId,
        place: Option<koru_artifacts_ui::VersionPlace>,
        content: koru_artifacts_ui::Content,
    ) {
        self.surface.take_content(version, place, content);
        self.rebuild_document();
    }
}

impl View for ResultView {
    /// **The ring first, then the page, then the chrome.**
    ///
    /// While a chrome row holds the focus ring the arrows step between
    /// controls, because that is what the ring means. Otherwise the page has
    /// the keyboard: the arrows move the caret, Shift extends a selection,
    /// Ctrl+C copies and Ctrl+F finds — and, once an edit is open, typing
    /// goes into the document. What the page declines falls through to the
    /// chrome, which is how Tab still reaches every control on a read-only
    /// page without a mouse.
    ///
    /// **Escape is the way out of an open edit**, and it has to be taken
    /// before the page sees it: a page being typed into answers Tab itself,
    /// so Tab is no longer a way back to the controls. Escape puts the ring
    /// on the first of them, and from there Tab walks the rest.
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if self.surface.focus().is_some() {
            let handled = self.surface.key(key, mods);
            self.settle();
            return handled;
        }
        if self.surface.is_editing()
            && matches!(key, Key::Named(winit::keyboard::NamedKey::Escape))
            && self.surface.focus_first_control()
        {
            return true;
        }
        if let Some((document, _)) = self.page()
            && document.key(key, text, mods)
        {
            self.settle();
            return true;
        }
        let handled = self.surface.key(key, mods);
        self.settle();
        handled
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        if let Some((document, band)) = self.page()
            && y >= band
        {
            let took = document.mouse_down(x, y - band, ctrl);
            // The caret is in the page now, so the ring on a control is a
            // second place the keyboard appears to be.
            self.surface.clear_focus();
            self.settle();
            return took;
        }
        let took = self.surface.mouse_down(x, y);
        self.settle();
        took
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if let Some((document, band)) = self.page()
            && (y >= band || document.is_dragging())
        {
            let moved = document.mouse_move(x, y - band);
            return self.surface.mouse_move(x, y) || moved;
        }
        self.surface.mouse_move(x, y)
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        let released = match self.page() {
            Some((document, _)) => document.mouse_up(),
            None => false,
        };
        let took = self.surface.mouse_up() || released;
        self.settle();
        took
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        match self.page() {
            Some((document, band)) if y >= band => document.mouse_double_click(x, y - band),
            _ => false,
        }
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        if let Some((document, _)) = self.page() {
            document.set_mods(mods);
        }
    }

    /// **The page scrolls, when there is one.** The chrome above it is four
    /// short rows that never need to move, so a result scrolls the way a
    /// document does — and a state sentence or an open composer, which are
    /// the tab's own, scroll the tab's own way.
    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        match self.page() {
            Some((document, _)) => document.scroll_by(delta),
            None => self.surface.scroll_by(delta),
        }
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.surface.set_viewport(w, h);
        self.resize_document();
    }

    fn cursor_icon(&self) -> CursorIcon {
        match self.document.as_ref() {
            Some(document) if self.surface.hover().is_none() => document.cursor_icon(),
            _ => self.surface.cursor_icon(),
        }
    }

    /// A result is an artifact version, not the open workspace document.
    /// Nothing here reads one, and nothing that happens to one changes what
    /// this tab shows: a version is immutable (ADR-344), and revising one
    /// makes the next version rather than editing this workspace.
    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.surface.invalidate();
        if let Some(document) = self.document.as_mut() {
            document.invalidate();
        }
    }

    fn set_workspace_stats(&mut self, _stats: Vec<WorkspaceStat>) -> bool {
        false
    }

    /// **The page is what animates here.** A selection dragged past the
    /// bottom of the band scrolls on its own, and that needs frames — the
    /// tab itself is still chrome and never asks for one.
    ///
    /// `maintain` is deliberately *not* forwarded, and stays that way now
    /// that the page can be typed into. Everything behind it is derived
    /// polish on the *window's* document — a delayed table control, a search
    /// flash, the spell checker — and it is handed the window's workspace,
    /// which this page must never be given. Spelling in particular would
    /// underline a result somebody else delivered against a dictionary this
    /// private workspace does not have. `docs/features/macos/CATCHUP.md` is not
    /// involved; this is a deliberate omission, noted in ADR-378.
    fn animating(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(ResultDocument::is_animating)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _ws: &koru_workspace::Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        self.surface.draw(scene, root);
        // Measured after the chrome drew, because that is the layout that
        // decided where it ends — the page cannot be drawn anywhere other
        // than under the row above it.
        self.band = self.surface.document_top();
        let (w, h) = self.viewport;
        let band = self.band;
        if let Some(document) = self.document.as_mut() {
            document.set_viewport(w, (h - band).max(0.0));
            // **The clip is not optional**, for the container's own reason:
            // the document view draws one row of margin either side of the
            // visible range at an unclamped `y`, so a page scrolled by half a
            // heading would paint that heading over the result's name.
            scene.push_clip_layer(
                vello::peniko::Fill::NonZero,
                root,
                &vello::kurbo::Rect::new(0.0, band as f64, w as f64, h as f64),
            );
            document.draw(
                scene,
                images,
                root * Affine::translate((0.0, band as f64)),
                caret_on,
                (pointer.0, pointer.1 - band),
            );
            scene.pop_layer();
        }
    }

    fn restore(&mut self, _ws: &koru_workspace::Workspace, _settings: &Settings) {}

    fn capture(&self, _settings: &mut Settings) {}
}
