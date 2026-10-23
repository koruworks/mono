// SPDX-License-Identifier: MPL-2.0

//! **Home's Today page** (workspace foundation, frame 1): the greeting, the
//! date, one line of numbers, and three sections — what is due today, what
//! to continue, and the agents at work — each drawn from a fact the window
//! already holds.
//!
//! The page is a [`View`] like Welcome: it owns its text engine, its scroll
//! and its targets, and asks the container for anything it cannot do itself
//! through [`HomeWant`]. The container feeds it (`feed`) each frame it is in
//! front — the person's name, the recents, the runs, whether the store is
//! reachable — and drives its task panel the way it drives the Tasks page's,
//! so the *Due today* rows come off the same worker thread
//! (`tasks_view/query.rs`) and never a walk on the frame thread.
//!
//! Each section fails alone: a task answer the store refused shows *Could not
//! load* and *Try again* under its own heading while the others stand.

use koru_core::DocumentId;
use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, icons, scene, theme};
use koru_i18n::{Key, Lang, fill, t};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::tasks_view::TaskEntry;
use crate::tasks_view::query::TaskListPanel;

/// What the page asks the container for. Every want is an existing command;
/// the page only names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeWant {
    /// *Add task*: the Tasks page on Today with the caret in its capture row.
    AddTask,
    /// A Continue card.
    OpenDocument(DocumentId),
    /// A Due today row: the task's node in its document.
    OpenTask { document: DocumentId, node: NodeId },
    /// An Agents row, or its *Review*.
    OpenRun(koru_delegate_core::RunId),
}

/// Something on the page a press or the keyboard can land on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Target {
    AddTask,
    Task(usize),
    Recent(usize),
    Run(usize),
    /// *Try again* under a section that could not load.
    Retry,
}

/// How many Continue cards the page draws — the nav's three.
pub const RECENT_MAX: usize = 3;
/// How many Due today rows before the page says there are more.
const TASKS_MAX: usize = 8;

const PAD_X: f32 = 40.0;
const PAD_TOP: f32 = 36.0;
const PAD_BOTTOM: f32 = 48.0;
const SECTION_GAP: f32 = 30.0;
const HEADING_GAP: f32 = 10.0;
const ROW_H: f32 = 44.0;
const CARD_H: f32 = 56.0;
const CARD_GAP: f32 = 8.0;
const BUTTON_H: f32 = 32.0;
const RADIUS: f32 = 8.0;
const CONTENT_MAX_W: f32 = 720.0;
const WHEEL_LINES: f32 = 3.0;

pub struct HomePage {
    name: String,
    recents: Vec<(String, DocumentId)>,
    runs: Vec<koru_runs_ui::RunRow>,
    offline: bool,
    panel: TaskListPanel,
    engine: TextEngine,
    viewport: (f32, f32),
    pointer: (f32, f32),
    scroll: f32,
    content_h: f32,
    hover: Option<Target>,
    focus: Option<Target>,
    /// Where each target was drawn last frame, in the page's coordinates
    /// with the scroll applied — what the pointer and the ring are matched
    /// against.
    drawn: Vec<(Target, Rect)>,
    wants: Vec<HomeWant>,
}

impl Default for HomePage {
    fn default() -> Self {
        Self::new()
    }
}

impl HomePage {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            recents: Vec::new(),
            runs: Vec::new(),
            offline: false,
            panel: TaskListPanel::default(),
            engine: TextEngine::new(),
            viewport: (0.0, 0.0),
            pointer: (-1.0, -1.0),
            scroll: 0.0,
            content_h: 0.0,
            hover: None,
            focus: None,
            drawn: Vec::new(),
            wants: Vec::new(),
        }
    }

    /// The facts the page is drawn from, from the container, once a frame
    /// while the page is in front. Returns whether any moved.
    pub fn feed(
        &mut self,
        name: &str,
        recents: Vec<(String, DocumentId)>,
        runs: Vec<koru_runs_ui::RunRow>,
        offline: bool,
    ) -> bool {
        let recents: Vec<_> = recents.into_iter().take(RECENT_MAX).collect();
        let changed = self.name != name
            || self.recents != recents
            || self.runs != runs
            || self.offline != offline;
        if changed {
            self.name = name.to_owned();
            self.recents = recents;
            self.runs = runs;
            self.offline = offline;
            self.content_h = 0.0;
        }
        changed
    }

    /// The *Due today* panel: the container gives it the client and drives
    /// it, as it does the Tasks page's.
    pub fn panel_mut(&mut self) -> &mut TaskListPanel {
        &mut self.panel
    }

    pub fn take_wants(&mut self) -> Vec<HomeWant> {
        std::mem::take(&mut self.wants)
    }

    /// The open tasks the panel answered with, overdue included.
    fn due_rows(&self) -> Vec<&TaskEntry> {
        self.panel
            .rows()
            .iter()
            .filter(|row| !row.completed && !row.trashed)
            .collect()
    }

    fn running(&self) -> usize {
        self.runs
            .iter()
            .filter(|run| run.motion == koru_runs_ui::Motion::Working)
            .count()
    }

    /// *{n} tasks due · {n} agents running*, the offline word after it. The
    /// counts wait for the facts: before the panel has answered the tasks
    /// half is left out rather than printed as zero.
    pub fn summary(&self, lang: Lang) -> String {
        let mut parts = Vec::new();
        if self.panel.known() {
            let n = self.due_rows().len();
            let key = if n == 1 {
                Key::HomeTasksDueOne
            } else {
                Key::HomeTasksDueMany
            };
            parts.push(fill(t(lang, key), &[&n.to_string()]));
        }
        if koru_features::agent_views() {
            let n = self.running();
            let key = if n == 1 {
                Key::HomeAgentsRunningOne
            } else {
                Key::HomeAgentsRunningMany
            };
            parts.push(fill(t(lang, key), &[&n.to_string()]));
        }
        if self.offline {
            parts.push(t(lang, Key::StoreOfflineChip).to_owned());
        }
        parts.join(" \u{b7} ")
    }

    /// *Hello, {name}* — or just *Hello* for a profile with no name yet.
    pub fn greeting(&self, lang: Lang) -> String {
        if self.name.trim().is_empty() {
            t(lang, Key::HomeGreetingPlain).to_owned()
        } else {
            fill(t(lang, Key::HomeGreeting), &[self.name.trim()])
        }
    }

    /// Today's date in the reader's language and order (Q-N: the numeric
    /// form until the long date lands).
    fn date_line(&self, lang: Lang) -> String {
        let now = chrono::Local::now().timestamp_millis();
        crate::format::local_parts(now)
            .map(|parts| koru_i18n::time::absolute_date(lang, parts, false))
            .unwrap_or_default()
    }

    fn targets(&self) -> Vec<Target> {
        self.drawn.iter().map(|(target, _)| *target).collect()
    }

    fn activate(&mut self, target: Target) {
        match target {
            Target::AddTask => self.wants.push(HomeWant::AddTask),
            Target::Task(i) => {
                if let Some(row) = self.due_rows().get(i) {
                    let (document, node) = (row.document, row.node);
                    self.wants.push(HomeWant::OpenTask { document, node });
                }
            }
            Target::Recent(i) => {
                if let Some((_, id)) = self.recents.get(i) {
                    self.wants.push(HomeWant::OpenDocument(*id));
                }
            }
            Target::Run(i) => {
                if let Some(run) = self.runs.get(i) {
                    self.wants.push(HomeWant::OpenRun(run.run));
                }
            }
            Target::Retry => self.panel.invalidate(),
        }
    }

    fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        self.drawn
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(target, _)| *target)
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h - self.viewport.1).max(0.0)
    }

    fn move_focus(&mut self, forward: bool) {
        let targets = self.targets();
        if targets.is_empty() {
            self.focus = None;
            return;
        }
        let at = self
            .focus
            .and_then(|f| targets.iter().position(|t| *t == f));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => targets.len() - 1,
            (Some(i), true) => (i + 1) % targets.len(),
            (Some(i), false) => (i + targets.len() - 1) % targets.len(),
        };
        self.focus = Some(targets[next]);
        if let Some((_, r)) = self.drawn.iter().find(|(t, _)| Some(*t) == self.focus) {
            // Keep the ring on screen: scroll by the overhang, not to the row.
            let top = r.y + self.scroll;
            if top < self.scroll {
                self.scroll = top.max(0.0);
            } else if top + r.h > self.scroll + self.viewport.1 {
                self.scroll = (top + r.h - self.viewport.1).min(self.max_scroll());
            }
        }
    }

    // ---------------------------------------------------------------- draw

    fn content_x(&self) -> f32 {
        let w = self.viewport.0.min(CONTENT_MAX_W + PAD_X * 2.0);
        ((self.viewport.0 - w) * 0.5 + PAD_X).max(PAD_X)
    }

    fn content_w(&self) -> f32 {
        (self.viewport.0.min(CONTENT_MAX_W + PAD_X * 2.0) - PAD_X * 2.0).max(1.0)
    }

    fn draw_page(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        let lang = crate::format::lang();
        scene::fill(
            scene,
            root,
            p.bg,
            &scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
        );
        self.drawn.clear();
        let x = self.content_x();
        let w = self.content_w();
        let mut y = PAD_TOP - self.scroll;

        // The greeting, the date, the numbers.
        let greeting = self
            .engine
            .simple_layout(&self.greeting(lang), theme::role::title());
        scene::draw_layout_first_line(scene, root, &greeting, x, y, Some(p.text));
        y += scene::first_line_height(&greeting) + 6.0;
        let date = self
            .engine
            .simple_layout(&self.date_line(lang), theme::role::subtitle());
        scene::draw_layout_first_line(scene, root, &date, x, y, Some(p.text_dim));
        y += scene::first_line_height(&date) + 4.0;
        let summary = self.summary(lang);
        if !summary.is_empty() {
            let line = self.engine.simple_layout(&summary, theme::role::meta());
            scene::draw_layout_first_line(scene, root, &line, x, y, Some(p.text_dim));
            y += scene::first_line_height(&line);
        }
        y += SECTION_GAP;

        // Due today.
        let due = self
            .due_rows()
            .iter()
            .map(|r| (*r).clone())
            .collect::<Vec<_>>();
        let count = self.panel.known().then_some(due.len());
        y = self.draw_heading(scene, root, x, w, y, t(lang, Key::HomeDueToday), count);
        if let Some(notice) = self.panel.notice().map(str::to_owned) {
            y = self.draw_failed(scene, root, x, w, y, &notice);
        } else if !self.panel.known() {
            y = self.draw_quiet(scene, root, x, y, t(lang, Key::HomeLoading));
        } else if due.is_empty() {
            y = self.draw_quiet(scene, root, x, y, t(lang, Key::HomeNothingDue));
        } else {
            for (i, row) in due.iter().take(TASKS_MAX).enumerate() {
                let r = Rect::new(x, y, w, ROW_H);
                self.draw_task_row(scene, root, r, row, Target::Task(i));
                y += ROW_H;
            }
            if due.len() > TASKS_MAX {
                let more = fill(
                    t(lang, Key::HomeMoreTasks),
                    &[&(due.len() - TASKS_MAX).to_string()],
                );
                y = self.draw_quiet(scene, root, x, y, &more);
            }
        }
        y += 8.0;
        y = self.draw_button(
            scene,
            root,
            x,
            y,
            icons::PLUS,
            t(lang, Key::HomeAddTask),
            Target::AddTask,
        );
        y += SECTION_GAP;

        // Continue.
        y = self.draw_heading(scene, root, x, w, y, t(lang, Key::Continue), None);
        if self.recents.is_empty() {
            y = self.draw_quiet(scene, root, x, y, t(lang, Key::HomeNothingRecent));
        } else {
            let recents = self.recents.clone();
            for (i, (title, _)) in recents.iter().enumerate() {
                let r = Rect::new(x, y, w, CARD_H);
                self.draw_card(
                    scene,
                    root,
                    r,
                    icons::DOCUMENT,
                    title,
                    t(lang, Key::DocumentsHeading),
                    Target::Recent(i),
                );
                y += CARD_H + CARD_GAP;
            }
        }

        // Agents, when the product shows them.
        if koru_features::agent_views() {
            y += SECTION_GAP - CARD_GAP;
            let running = self.running();
            y = self.draw_heading(
                scene,
                root,
                x,
                w,
                y,
                t(lang, Key::A11yAgentsHeading),
                Some(running),
            );
            if self.runs.is_empty() {
                y = self.draw_quiet(scene, root, x, y, t(lang, Key::HomeNoAgents));
            } else {
                let runs = self.runs.clone();
                for (i, run) in runs.iter().enumerate() {
                    let r = Rect::new(x, y, w, CARD_H);
                    let detail = if run.needs_attention {
                        t(lang, Key::HomeReview).to_owned()
                    } else {
                        run.status.clone()
                    };
                    let subtitle = if run.objective.is_empty() {
                        detail
                    } else {
                        format!("{} \u{b7} {detail}", run.objective)
                    };
                    self.draw_card(
                        scene,
                        root,
                        r,
                        icons::ROBOT,
                        &run.name,
                        &subtitle,
                        Target::Run(i),
                    );
                    y += CARD_H + CARD_GAP;
                }
            }
        }
        self.content_h = y + self.scroll + PAD_BOTTOM;
        // The ring, over whatever has it.
        if let Some((_, r)) = self.drawn.iter().find(|(t, _)| Some(*t) == self.focus) {
            scene::stroke(
                scene,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0, RADIUS + 1.0),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_heading(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        x: f32,
        w: f32,
        y: f32,
        label: &str,
        count: Option<usize>,
    ) -> f32 {
        let p = theme::p();
        let heading = self.engine.simple_layout(
            label,
            theme::role::ui().with_weight(theme::FontWeight::SemiBold),
        );
        scene::draw_layout_first_line(scene, root, &heading, x, y, Some(p.text));
        if let Some(n) = count {
            let badge = self
                .engine
                .simple_layout(&n.to_string(), theme::role::meta());
            scene::draw_layout_first_line(
                scene,
                root,
                &badge,
                x + heading.width() + 8.0,
                y + 2.0,
                Some(p.text_dim),
            );
        }
        let bottom = y + scene::first_line_height(&heading) + HEADING_GAP;
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(x, bottom - 4.0, w, 1.0),
        );
        bottom + 4.0
    }

    fn draw_quiet(&mut self, scene: &mut Scene, root: Affine, x: f32, y: f32, text: &str) -> f32 {
        let p = theme::p();
        let line = self.engine.simple_layout(text, theme::role::meta());
        scene::draw_layout_first_line(scene, root, &line, x, y + 6.0, Some(p.text_dim));
        y + scene::first_line_height(&line) + 12.0
    }

    /// *Could not load — Try again*, under the section's own heading.
    fn draw_failed(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        x: f32,
        w: f32,
        y: f32,
        detail: &str,
    ) -> f32 {
        let p = theme::p();
        let lang = crate::format::lang();
        let title = self
            .engine
            .simple_layout(t(lang, Key::HomeCouldNotLoad), theme::role::row());
        scene::draw_layout_first_line(scene, root, &title, x, y + 6.0, Some(p.text));
        let mut yy = y + 6.0 + scene::first_line_height(&title) + 2.0;
        let detail = self.engine.elided(detail, theme::role::meta(), w);
        let detail = self.engine.simple_layout(&detail, theme::role::meta());
        scene::draw_layout_first_line(scene, root, &detail, x, yy, Some(p.text_dim));
        yy += scene::first_line_height(&detail) + 8.0;
        self.draw_button(
            scene,
            root,
            x,
            yy,
            icons::ARROW_RIGHT,
            t(lang, Key::TryAgain),
            Target::Retry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_button(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        x: f32,
        y: f32,
        glyph: char,
        label: &str,
        target: Target,
    ) -> f32 {
        let p = theme::p();
        let text = self.engine.simple_layout(label, theme::role::ui());
        let r = Rect::new(x, y, text.width() + 16.0 + 12.0 + 16.0, BUTTON_H);
        let hot = self.hover == Some(target);
        scene::fill(
            scene,
            root,
            if hot { p.hover_tint } else { p.panel_bg },
            &scene::rrect(r.x, r.y, r.w, r.h, RADIUS),
        );
        scene::stroke(
            scene,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(r.x, r.y, r.w, r.h, RADIUS),
        );
        let icon = self.engine.icon_layout(glyph, 14.0);
        scene::draw_layout_first_line(
            scene,
            root,
            &icon,
            r.x + 12.0,
            scene::centre_y_in(&icon, r),
            Some(p.text_dim),
        );
        scene::draw_layout_first_line(
            scene,
            root,
            &text,
            r.x + 12.0 + 16.0 + 6.0,
            scene::centre_y_in(&text, r),
            Some(p.text),
        );
        self.drawn.push((target, r));
        y + BUTTON_H
    }

    fn draw_task_row(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        r: Rect,
        row: &TaskEntry,
        target: Target,
    ) {
        let p = theme::p();
        if self.hover == Some(target) {
            scene::fill(
                scene,
                root,
                p.hover_tint,
                &scene::rrect(r.x - 8.0, r.y, r.w + 16.0, r.h, RADIUS),
            );
        }
        let icon = self.engine.icon_layout(icons::CHECK_SQUARE, 16.0);
        scene::draw_layout_first_line(
            scene,
            root,
            &icon,
            r.x,
            scene::centre_y_in(&icon, r),
            Some(if row.overdue { p.notice_fg } else { p.text_dim }),
        );
        let text_x = r.x + 26.0;
        let crumb = self
            .engine
            .simple_layout(&row.breadcrumb, theme::role::meta());
        let crumb_w = crumb.width().min(r.w * 0.4);
        let title_w = (r.w - 26.0 - crumb_w - 12.0).max(1.0);
        let title = self.engine.elided(&row.title, theme::role::row(), title_w);
        let title = self.engine.simple_layout(&title, theme::role::row());
        scene::draw_layout_first_line(
            scene,
            root,
            &title,
            text_x,
            scene::centre_y_in(&title, r),
            Some(p.text),
        );
        let crumb_text = self
            .engine
            .elided(&row.breadcrumb, theme::role::meta(), crumb_w);
        let crumb = self.engine.simple_layout(&crumb_text, theme::role::meta());
        scene::draw_layout_first_line(
            scene,
            root,
            &crumb,
            r.x + r.w - crumb.width(),
            scene::centre_y_in(&crumb, r),
            Some(p.text_dim),
        );
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(r.x + 26.0, r.y + r.h - 1.0, r.w - 26.0, 1.0),
        );
        self.drawn.push((target, r));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_card(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        r: Rect,
        glyph: char,
        title: &str,
        subtitle: &str,
        target: Target,
    ) {
        let p = theme::p();
        let hot = self.hover == Some(target);
        scene::fill(
            scene,
            root,
            if hot { p.hover_tint } else { p.panel_bg },
            &scene::rrect(r.x, r.y, r.w, r.h, RADIUS),
        );
        scene::stroke(
            scene,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(r.x, r.y, r.w, r.h, RADIUS),
        );
        let icon = self.engine.icon_layout(glyph, 18.0);
        scene::draw_layout_first_line(
            scene,
            root,
            &icon,
            r.x + 14.0,
            scene::centre_y_in(&icon, r),
            Some(p.text_dim),
        );
        let text_x = r.x + 14.0 + 18.0 + 12.0;
        let text_w = (r.x + r.w - text_x - 14.0).max(1.0);
        let title = self.engine.elided(title, theme::role::row(), text_w);
        let title = self.engine.simple_layout(&title, theme::role::row());
        scene::draw_layout_first_line(scene, root, &title, text_x, r.y + 10.0, Some(p.text));
        let subtitle = self.engine.elided(subtitle, theme::role::meta(), text_w);
        let subtitle = self.engine.simple_layout(&subtitle, theme::role::meta());
        scene::draw_layout_first_line(
            scene,
            root,
            &subtitle,
            text_x,
            r.y + 10.0 + scene::first_line_height(&title) + 2.0,
            Some(p.text_dim),
        );
        self.drawn.push((target, r));
    }
}

impl View for HomePage {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &WKey,
        _text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        match key {
            WKey::Named(NamedKey::ArrowDown) => {
                self.move_focus(true);
                true
            }
            WKey::Named(NamedKey::ArrowUp) => {
                self.move_focus(false);
                true
            }
            WKey::Named(NamedKey::Tab) => {
                self.move_focus(!mods.shift_key());
                true
            }
            WKey::Named(NamedKey::Enter) | WKey::Named(NamedKey::Space) => match self.focus {
                Some(target) => {
                    self.activate(target);
                    true
                }
                None => false,
            },
            WKey::Named(NamedKey::Escape) if self.focus.is_some() => {
                self.focus = None;
                true
            }
            _ => false,
        }
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        self.pointer = (x, y);
        self.focus = None;
        match self.target_at(x, y) {
            Some(target) => {
                self.activate(target);
                true
            }
            None => false,
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        let next = self.target_at(x, y);
        let changed = next != self.hover;
        self.hover = next;
        changed
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        false
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, _x: f32, _y: f32) -> bool {
        false
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                y * theme::text_metrics::DOC_BODY.line_height * WHEEL_LINES
            }
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.scroll = (self.scroll - lines).clamp(0.0, self.max_scroll());
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        if self.viewport != (w, h) {
            self.viewport = (w, h);
            self.scroll = self.scroll.min(self.max_scroll());
        }
    }

    fn cursor_icon(&self) -> CursorIcon {
        if self.hover.is_some() {
            koru_ui::cursor::PRESS
        } else {
            koru_ui::cursor::IDLE
        }
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}
    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.content_h = 0.0;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &Workspace,
        _images: &ImageCache,
        root: Affine,
        _caret_on: bool,
        pointer: (f32, f32),
    ) {
        self.pointer = pointer;
        self.draw_page(scene, root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers line waits for the facts: nothing until the panel has
    /// answered, the offline word last.
    #[test]
    fn the_summary_is_built_from_what_is_known() {
        let mut page = HomePage::new();
        assert!(page.feed("Steven", Vec::new(), Vec::new(), false));
        assert_eq!(page.greeting(Lang::En), "Hello, Steven");
        let before = page.summary(Lang::En);
        assert!(
            !before.contains("task"),
            "no answer yet: no count printed: {before}"
        );
        assert!(page.feed("Steven", Vec::new(), Vec::new(), true));
        assert!(page.summary(Lang::En).ends_with("Offline"));
        assert!(
            !page.feed("Steven", Vec::new(), Vec::new(), true),
            "unchanged"
        );
        assert!(page.feed("", Vec::new(), Vec::new(), true));
        assert_eq!(page.greeting(Lang::En), "Hello");
    }

    /// A press on what the page drew becomes the want the container acts
    /// on; the keyboard walks the same targets.
    #[test]
    fn targets_become_wants() {
        let mut page = HomePage::new();
        let id = DocumentId::new();
        page.feed("S", vec![("Notes".into(), id)], Vec::new(), false);
        // Stand in for a frame: the page records where it drew things.
        page.drawn = vec![
            (Target::AddTask, Rect::new(0.0, 0.0, 100.0, 30.0)),
            (Target::Recent(0), Rect::new(0.0, 40.0, 100.0, 30.0)),
        ];
        let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
        assert!(View::mouse_down(&mut page, &mut doc, 10.0, 50.0, false));
        assert_eq!(page.take_wants(), vec![HomeWant::OpenDocument(id)]);
        assert!(!View::mouse_down(&mut page, &mut doc, 10.0, 200.0, false));
        assert!(View::key(
            &mut page,
            &mut doc,
            &WKey::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty()
        ));
        assert_eq!(page.focus, Some(Target::AddTask));
        assert!(View::key(
            &mut page,
            &mut doc,
            &WKey::Named(NamedKey::Enter),
            None,
            ModifiersState::empty()
        ));
        assert_eq!(page.take_wants(), vec![HomeWant::AddTask]);
        assert!(View::mouse_move(&mut page, 10.0, 10.0));
        assert_eq!(page.hover, Some(Target::AddTask));
    }
}
