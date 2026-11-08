// SPDX-License-Identifier: MPL-2.0

//! The one-field name modal: the taskbar's “New document”, and the Agents
//! tree's project prompts.
//!
//! It deliberately reuses the editor's [`NamePrompt`]: text selection, undo,
//! paste and platform key behaviour should not change just because the prompt
//! was opened from the taskbar instead of `/document`.
//!
//! **One surface, three asks** — the same shape Koru Terminal's `prompt.rs`
//! gives its Create/Rename/Set-folder panel. The Agents tree's *New project*
//! and *Rename project* are a title, a field and an Enter, exactly what this
//! modal already is; a second panel would be a second copy of the recipe, and
//! a second `Layer` in the routing table for the same kind of surface. What
//! the committed name *means* stays with the container ([`Purpose`] only says
//! which question was asked).

use koru_core::NodeId;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_editor::components::Outcome;
use koru_editor::components::draw::draw_name_prompt_contents;
use koru_editor::components::name_prompt::{NamePrompt, field_rect};
use koru_editor::components::text_field::TextField;
use koru_editor::trigger::{Anchor, Trigger, TriggerKind};
use koru_session_protocol::ProjectId;
use koru_ui::{KoruDialogFrame, KoruDialogFrameInspection, KoruDialogFrameState};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

pub(super) enum Action {
    Keep,
    Cancel,
    CreateDocument(String),
    CreateWorkspace(String),
}

/// What the committed name is for. Resolved by the container, not here — the
/// same contract `MenuAction` keeps.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum Purpose {
    /// Ask for a workspace name before creating any durable catalog row.
    Workspace,
    /// The taskbar's ask: create and open a document under this title.
    Document,
    /// A sidebar row's `+`: create and open a document **inside** this one,
    /// as its last child (the review's note).
    ChildDocument(NodeId),
    /// The Agents tree's `+`: create a project (client-minted id, scoped to
    /// the window's workspace — both the container's business).
    NewAgentProject,
    /// A project row's Rename…, seeded with the registry's current name.
    RenameAgentProject(ProjectId),
}

pub(super) struct NewDocument {
    prompt: NamePrompt,
    dragging: bool,
    purpose: Purpose,
}

impl NewDocument {
    /// The name box, for a dictation the container is routing.
    pub(super) fn dictation_field(&mut self) -> &mut koru_ui::TextField {
        &mut self.prompt.input
    }

    pub(super) fn new(engine: &mut TextEngine) -> Self {
        Self::with_purpose(engine, Purpose::Document, "")
    }

    /// The same prompt, for a document inside `parent`.
    pub(super) fn child(engine: &mut TextEngine, parent: NodeId) -> Self {
        Self::with_purpose(engine, Purpose::ChildDocument(parent), "")
    }

    pub(super) fn workspace(engine: &mut TextEngine) -> Self {
        Self::with_purpose(engine, Purpose::Workspace, "")
    }

    /// The Agents tree's *New project…* ask.
    pub(super) fn new_agent_project(engine: &mut TextEngine) -> Self {
        Self::with_purpose(engine, Purpose::NewAgentProject, "")
    }

    /// …and its *Rename…*, seeded with what is already true so the field is
    /// an edit rather than a retype.
    pub(super) fn rename_agent_project(
        engine: &mut TextEngine,
        project: ProjectId,
        name: &str,
    ) -> Self {
        Self::with_purpose(engine, Purpose::RenameAgentProject(project), name)
    }

    fn with_purpose(engine: &mut TextEngine, purpose: Purpose, seed: &str) -> Self {
        let mut input = TextField::new(engine, theme::role::body());
        if !seed.is_empty() {
            input.set_text(engine, seed);
        }
        Self {
            prompt: NamePrompt {
                // This prompt is not anchored to document text, but
                // NamePrompt hands a Trigger back as part of its reusable
                // component contract. This detached insert trigger is never
                // committed into a row; the container consumes only the name.
                trigger: Trigger {
                    kind: TriggerKind::Slash,
                    start: 0,
                    node: NodeId::new(),
                    anchor: Anchor::Insert,
                    stable: None,
                },
                input,
                page_icon: None,
            },
            dragging: false,
            purpose,
        }
    }

    pub(super) fn purpose(&self) -> Purpose {
        self.purpose
    }

    pub(super) fn panel(viewport: (f32, f32)) -> Rect {
        KoruDialogFrame::name_prompt(viewport).panel()
    }

    fn field(&self, viewport: (f32, f32)) -> Rect {
        let panel = Self::panel(viewport);
        let field = field_rect(panel.w);
        Rect::new(panel.x + field.x, panel.y + field.y, field.w, field.h)
    }

    pub(super) fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Action {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            return Action::Cancel;
        }
        match self.prompt.key(engine, key, text, mods) {
            // NamePrompt currently keeps every empty/whitespace-only Enter
            // open before returning a Commit. Keep this project validation
            // too: the protocol never accepts an unnamed project.
            Outcome::Commit(spec)
                if matches!(
                    self.purpose,
                    Purpose::NewAgentProject | Purpose::RenameAgentProject(_)
                ) && spec.label.trim().is_empty() =>
            {
                Action::Keep
            }
            Outcome::Commit(spec) if self.purpose == Purpose::Workspace => {
                Action::CreateWorkspace(spec.label)
            }
            Outcome::Commit(spec) => Action::CreateDocument(spec.label),
            _ => Action::Keep,
        }
    }

    pub(super) fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) {
        let panel = Self::panel(viewport);
        self.dragging = self.prompt.click(engine, x - panel.x, y - panel.y, panel.w);
    }

    pub(super) fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
    ) -> bool {
        if !self.dragging {
            return false;
        }
        let panel = Self::panel(viewport);
        self.prompt.drag(engine, x - panel.x, panel.w);
        true
    }

    pub(super) fn mouse_up(&mut self) -> bool {
        std::mem::take(&mut self.dragging)
    }

    pub(super) fn double_click(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) {
        let panel = Self::panel(viewport);
        self.prompt
            .double_click(engine, x - panel.x, y - panel.y, panel.w);
    }

    pub(super) fn cursor(&self, viewport: (f32, f32), x: f32, y: f32) -> CursorIcon {
        if self.field(viewport).contains(x, y) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        KoruDialogFrame::name_prompt(viewport).draw(
            scene,
            root,
            KoruDialogFrameInspection {
                state: KoruDialogFrameState::Open,
                ..Default::default()
            },
            |scene, root, panel| {
                koru_draw::inspect::current(|r| {
                    let purpose = match self.purpose {
                        Purpose::Document => "document",
                        Purpose::ChildDocument(_) => "child document",
                        Purpose::Workspace => "workspace",
                        Purpose::NewAgentProject => "new project",
                        Purpose::RenameAgentProject(_) => "rename project",
                    };
                    r.property(
                        "purpose",
                        purpose,
                        "NewDocument::purpose; domain IDs and names excluded",
                    );
                    r.property(
                        "native source part",
                        "s:shell#new-document",
                        "static host blocker part; not a per-opening occurrence or full locator",
                    );
                    r.property(
                        "field width",
                        field_rect(panel.w).w,
                        "existing NamePrompt::field_rect; may be negative in tiny viewport",
                    );
                });
                let _content = koru_draw::inspect::scope(
                    "Name prompt content",
                    "unregistered",
                    root,
                    scene::rect(panel.x, panel.y, panel.w, panel.h),
                    |_| {},
                );
                let p = theme::p();
                // The project titles come off the i18n rows Koru Terminal's own prompt
                // already owns; nothing new lands in i18n.
                let lang = crate::format::lang();
                let (title, placeholder, hint_text) = match self.purpose {
                    Purpose::Document | Purpose::ChildDocument(_) => (
                        "New document",
                        "Document name",
                        "Enter to create · Esc to cancel",
                    ),
                    Purpose::Workspace => (
                        "New workspace",
                        "Workspace name",
                        "Enter to create · Esc to cancel",
                    ),
                    Purpose::NewAgentProject => (
                        koru_i18n::t(lang, koru_i18n::Key::TerminalNewProject),
                        koru_i18n::t(lang, koru_i18n::Key::TerminalProjectNamePlaceholder),
                        "Enter to create · Esc to cancel",
                    ),
                    Purpose::RenameAgentProject(_) => (
                        koru_i18n::t(lang, koru_i18n::Key::TerminalRenameProject),
                        koru_i18n::t(lang, koru_i18n::Key::TerminalProjectNamePlaceholder),
                        "Enter to save · Esc to cancel",
                    ),
                };
                draw_name_prompt_contents(
                    scene,
                    root,
                    &mut self.prompt,
                    engine,
                    panel.x,
                    panel.y,
                    panel.w,
                    title,
                    placeholder,
                    // No page-icon button — `can_set_page_icon` is true only for a
                    // document rename, which has a node to put an icon on. There is
                    // nothing here to hover.
                    false,
                );

                let hint = engine.simple_layout(hint_text, theme::role::hint());
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &hint,
                    panel.x + theme::PANEL_PAD + 4.0,
                    panel.y + 62.0,
                    Some(p.text_dim),
                );
            },
        );
    }
}

#[cfg(test)]
mod tests;
