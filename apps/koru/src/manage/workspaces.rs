// SPDX-License-Identifier: MPL-2.0

//! A Profile's *Workspaces*: the section between its header and *Sign-in and
//! sync*. Its first line is a Settings card caption with the section's two
//! actions at its right end.
//!
//! ```text
//! Workspaces · 3                        Import…  [New workspace]
//! a notice from the last thing done              Retry  Dismiss
//! ┌──────────────────────────────────────────────────────────────┐
//! │ [▣] Roadmap                        Pinned  Open           ••• │
//! │     Opened 3 hours ago                                         │
//! ├──────────────────────────────────────────────────────────────┤
//! │ [▣] Notes                                                 ••• │
//! └──────────────────────────────────────────────────────────────┘
//! Show archived (2)   Show trash (1)
//! Archived                                  (while it is open)
//! ┌ [▣] Old plans                                     [Unarchive] ┐
//! Trash
//! ┌ [▣] Scratch                                         [Restore] ┐
//! Deleted workspaces are removed automatically.
//! ```
//!
//! **The data is a [`WorkspacesController`]**, the same state machine the
//! Workspaces tab drew: it reads the including-deleted catalog through its own
//! vouched link, owns the lifecycle and move confirmations, and says why a
//! control cannot be pressed. This file only places what it says for one
//! Profile — `rows_of(profile)` — and turns a press into one of its
//! `Action`s, or into a [`Request`] for what only the window can do: opening
//! a workspace, the shared workspace menu, the name prompt, the import menu.
//!
//! **Disabled, never hidden.** A control the controller refuses is drawn
//! dimmed and its press is consumed; the reason is on the page as a quiet
//! line rather than behind a hover — *Unarchive* and *Restore* share one
//! reason (this window's `workspace.destroy`), so it is said once under the
//! list rather than once per row.
//!
//! **No *Delete forever*** (maintainers' decision 5): there is no purge route, and the
//! trash says deleted workspaces are removed automatically.

use super::profile_page::{Control, Look, Placed};
use super::*;
use koru_draw::geometry::Rect;
use koru_draw::tile::Tile;
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::card::Card;
use koru_ui::text_paragraph::KoruTextParagraph;
use koru_workspaces_ui::{Action, Group, Row as Catalog, WorkspacesController};

#[cfg(test)]
mod tests;

/// The catalog's workspace id. The window speaks `koru_core`'s; the two are
/// the same uuid.
pub(crate) type Wid = koru_store_core::ids::WorkspaceId;

/// One workspace row.
pub(crate) const ROW_H: f32 = 48.0;
const ROW_PAD: f32 = 12.0;
const ROW_TILE: f32 = 32.0;
/// The `•••` button's square, inside the row's right edge.
const MORE: f32 = 28.0;
const MORE_INSET: f32 = 10.0;
/// Between the heading line, the notice, each list and the links.
const LINE_GAP: f32 = 10.0;
/// Between two tags. A tag itself is the one chip component
/// (`koru_ui::label_chip`), compact and neutral — the same chip a document's
/// attribute and a task row wear.
const TAG_GAP: f32 = 6.0;
/// Between the two links on their line.
const LINK_GAP: f32 = 18.0;

pub(super) const TITLE: &str = "Workspaces";
const NEW: &str = "New workspace";
const IMPORT: &str = "Import\u{2026}";
const RETRY: &str = "Retry";
const DISMISS: &str = "Dismiss";
const ARCHIVED: &str = "Archived";
const TRASH: &str = "Trash";
const UNARCHIVE: &str = "Unarchive";
const RESTORE: &str = "Restore";
const PINNED_TAG: &str = "Pinned";
const OPEN_TAG: &str = "Open";
pub(super) const TRASH_NOTE: &str = "Deleted workspaces are removed automatically.";
const EMPTY: &str = "No workspaces in this profile yet.";
const LOADING: &str = "Looking for this profile\u{2019}s workspaces\u{2026}";

/// Which of the section's lists a row is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListKind {
    /// Pinned, then the rest, in the Profile's own order.
    Active,
    Archived,
    Trash,
}

/// One workspace, where everything on its row goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceRow {
    pub id: Wid,
    pub rect: Rect,
    pub tile: Rect,
    pub icon: Option<String>,
    pub color: Option<String>,
    /// The name as drawn — elided to fit — and where.
    pub name: Rect,
    pub name_text: String,
    /// The full name, for the reader and the rename field.
    pub full_name: String,
    /// *Opened {when}*, when the catalog knows (maintainers' decision 8).
    pub sub: Option<(Rect, String)>,
    /// *Pinned* and *Open*.
    pub tags: Vec<(Rect, &'static str)>,
}

/// One list and its card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceList {
    pub kind: ListKind,
    /// *Archived* or *Trash* above the card; the active list has none.
    pub label: Option<(Rect, &'static str)>,
    pub card: Rect,
    pub rows: Vec<WorkspaceRow>,
    /// What is said under the card: the trash's note, a refusal's reason.
    pub notes: Vec<(Rect, String)>,
}

/// The whole section, unscrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspacesSection {
    pub rect: Rect,
    pub heading: Rect,
    /// How many active workspaces — the number beside the heading.
    pub count: (Rect, String),
    /// The controller's notice (S24), with its Retry and Dismiss beside it.
    pub notice: Option<(Rect, String)>,
    /// Quiet lines under the heading: unavailable, looking, empty.
    pub quiet: Vec<(Rect, String)>,
    pub lists: Vec<WorkspaceList>,
}

impl WorkspacesSection {
    pub(crate) fn rows(&self) -> impl Iterator<Item = (&WorkspaceList, &WorkspaceRow)> {
        self.lists
            .iter()
            .flat_map(|list| list.rows.iter().map(move |row| (list, row)))
    }

    pub(crate) fn row(&self, id: Wid) -> Option<&WorkspaceRow> {
        self.rows().map(|(_, row)| row).find(|row| row.id == id)
    }
}

/// `koru_core`'s ids and the catalog's: the same uuid, two types.
pub(crate) fn catalog_workspace(id: koru_core::WorkspaceId) -> Wid {
    Wid::from_uuid(id.as_uuid())
}

pub(crate) fn app_workspace(id: Wid) -> koru_core::WorkspaceId {
    koru_core::WorkspaceId::from_uuid(id.as_uuid())
}

pub(crate) fn catalog_profile(id: ProfileId) -> koru_store_core::ids::ProfileId {
    koru_store_core::ids::ProfileId::from_uuid(id.as_uuid())
}

pub(crate) fn app_profile(id: koru_store_core::ids::ProfileId) -> ProfileId {
    ProfileId::from_uuid(id.as_uuid())
}

/// *Opened 3 hours ago*: the catalog's last-opened time, in today's relative
/// wording, lower-cased after *Opened* in English.
pub(crate) fn opened_line(then_ms: i64, now_ms: i64) -> String {
    let when = crate::format::ago(then_ms, now_ms);
    let when = if crate::format::lang() == koru_i18n::Lang::En {
        let mut chars = when.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().chain(chars).collect(),
            None => when,
        }
    } else {
        when
    };
    format!("Opened {when}")
}

/// The count after the caption's word: *· 3*.
fn count_label(count: &str) -> String {
    format!("\u{b7} {count}")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl ManageView {
    /// The Workspaces controller, for the window's workspace menu: its rows,
    /// its reasons and its destinations.
    pub fn workspaces(&self) -> std::cell::Ref<'_, WorkspacesController> {
        self.w.borrow()
    }

    /// Run `call` on the Workspaces controller — a menu verb the window
    /// carries in — and lay the page out again.
    pub fn with_workspaces<R>(&mut self, call: impl FnOnce(&mut WorkspacesController) -> R) -> R {
        let result = call(&mut self.w.borrow_mut());
        self.relayout();
        self.clamp_scroll();
        result
    }

    /// Reproject the shared controller after the window drains its answers.
    pub fn refresh_workspaces(&mut self) {
        self.relayout();
        self.clamp_scroll();
    }

    /// The workspace this window has open, for its *Open* tag.
    pub fn set_current_workspace(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        let changed = self.current_workspace != workspace;
        self.current_workspace = workspace;
        if changed {
            self.relayout();
        }
        changed
    }

    /// Every Profile, as somewhere a workspace could be moved to. The
    /// controller fetches the same list through its link; the window's is
    /// here at once, and names a Profile the moment it is renamed.
    pub fn set_workspace_profiles(&mut self, profiles: &[ProfileSummary]) -> bool {
        let rows: Vec<koru_workspaces_ui::ProfileRow> = profiles
            .iter()
            .map(|profile| koru_workspaces_ui::ProfileRow {
                id: catalog_profile(profile.id),
                name: profile.name.clone(),
            })
            .collect();
        self.w.borrow_mut().adopt_profiles(&rows)
    }

    /// Bring the Workspaces section to the top of the page — the switcher's
    /// *Manage workspaces…*.
    pub fn reveal_workspaces(&mut self) {
        if let Some(section) = &self.laid.workspaces {
            self.page_scroll = (section.rect.y - 16.0).max(0.0);
            self.clamp_scroll();
        }
    }

    /// The workspace `F2` renames: the row selected on this page.
    pub fn selected_workspace(&self) -> Option<koru_core::WorkspaceId> {
        let selected = self.w.borrow().selected()?;
        self.laid
            .workspaces
            .as_ref()
            .and_then(|section| section.row(selected))
            .map(|row| app_workspace(row.id))
    }

    /// Where a workspace's name is drawn on the page, scrolled — for the
    /// window's rename field, which goes over it.
    pub fn workspace_name_rect(
        &self,
        workspace: koru_core::WorkspaceId,
    ) -> Option<koru_draw::geometry::Rect> {
        let row = self
            .laid
            .workspaces
            .as_ref()?
            .row(catalog_workspace(workspace))?;
        let line = theme::role::row().metrics.line_height;
        let room = (row.rect.x + row.rect.w - row.name.x - MORE - MORE_INSET * 2.0).max(1.0);
        Some(Rect::new(
            row.name.x,
            row.name.y + (row.name.h - line) * 0.5 - self.page_scroll,
            row.name.w.max(220.0).min(room),
            line,
        ))
    }

    /// Which workspace a right-click at `(x, y)` is about, selecting its row
    /// as the menu opens so the page and the menu agree.
    pub fn workspace_menu_target_at(&mut self, x: f32, y: f32) -> Option<koru_core::WorkspaceId> {
        if !self.no_question_up() {
            return None;
        }
        let id = match self.page_hit(x, y)? {
            Control::WorkspaceRow(id)
            | Control::WorkspaceMore(id)
            | Control::Unarchive(id)
            | Control::RestoreWorkspace(id) => id,
            _ => return None,
        };
        self.w.borrow_mut().set_selected(Some(id));
        self.page_focus = Some(Control::WorkspaceRow(id));
        self.focus = None;
        Some(app_workspace(id))
    }

    /// Open a workspace from its row — a double-click, or Enter on it — when
    /// the controller allows it (a workspace in the trash is restored first).
    pub(super) fn open_row(&mut self, id: Wid) -> bool {
        let Some(row) = self.w.borrow().row(id).cloned() else {
            return false;
        };
        if self
            .w
            .borrow()
            .reason_for(&Action::Open(id), &row)
            .is_some()
        {
            return true;
        }
        self.requests
            .push(Request::OpenWorkspace(app_workspace(id)));
        true
    }

    // ---- layout --------------------------------------------------------------

    /// Place the section from `y` down; returns it and where the next thing
    /// goes.
    pub(super) fn lay_workspaces_section(
        &mut self,
        profile: &ProfileSummary,
        column: Rect,
        y: f32,
        controls: &mut Vec<Placed>,
        disabled: &mut Vec<Control>,
    ) -> (WorkspacesSection, f32) {
        let owner = catalog_profile(profile.id);
        let blank = Catalog::default();
        let p = theme::p();
        let rows: Vec<(Group, Catalog)> = self
            .w
            .borrow()
            .rows_of(owner)
            .into_iter()
            .map(|(group, row)| (group, row.clone()))
            .collect();
        let active: Vec<&Catalog> = rows
            .iter()
            .filter(|(group, _)| matches!(group, Group::Pinned | Group::Active))
            .map(|(_, row)| row)
            .collect();

        // The caption line: *Workspaces · n* in Settings' caption style on
        // the left, the two buttons on the right — *Import…* only on the open
        // Profile's page (decision 10). The line is the buttons' height.
        let band = ActionButton::H;
        let new_w = ActionButton::new(NEW, ActionStyle::Secondary, None).width(&mut self.engine);
        let new_rect = Rect::new(column.x + column.w - new_w, y, new_w, band);
        let import = (self.c.current() == Some(profile.id)).then(|| {
            let w = ActionButton::new(IMPORT, ActionStyle::Ghost, None).width(&mut self.engine);
            Rect::new(new_rect.x - LINK_GAP * 0.5 - w, y, w, band)
        });
        if let Some(rect) = import {
            controls.push(Placed {
                control: Control::ImportWorkspaces,
                rect,
                label: IMPORT.into(),
                look: Look::Action(ActionStyle::Ghost),
            });
            if self
                .w
                .borrow()
                .reason_for(&Action::Import, &blank)
                .is_some()
            {
                disabled.push(Control::ImportWorkspaces);
            }
        }
        controls.push(Placed {
            control: Control::NewWorkspace,
            rect: new_rect,
            label: NEW.into(),
            look: Look::Action(ActionStyle::Secondary),
        });
        if self
            .w
            .borrow()
            .reason_for(&Action::Create, &blank)
            .is_some()
        {
            disabled.push(Control::NewWorkspace);
        }
        let caption = theme::role::meta();
        let meta_line = caption.metrics.line_height;
        let heading_w = self.engine.simple_layout(TITLE, caption).width();
        let heading = Rect::new(column.x, y + (band - meta_line) * 0.5, heading_w, meta_line);
        let count_text = active.len().to_string();
        let count_w = self
            .engine
            .simple_layout(&count_label(&count_text), caption)
            .width();
        let count = (
            Rect::new(heading.x + heading.w + 4.0, heading.y, count_w, meta_line),
            count_text,
        );
        let mut cy = y + band + Card::TITLE_GAP;

        // The controller's notice (S24): its words, then Retry and Dismiss.
        let notice = self.w.borrow().notice().map(|notice| {
            let mut answers = Vec::new();
            if notice.is_retryable() {
                answers.push((Control::NoticeRetry, RETRY));
            }
            answers.push((Control::NoticeDismiss, DISMISS));
            (notice.text().to_owned(), answers)
        });
        let notice = notice.map(|(text, answers)| {
            let widths: Vec<f32> = answers
                .iter()
                .map(|(_, label)| {
                    ActionButton::new(label, ActionStyle::Ghost, None).width(&mut self.engine)
                })
                .collect();
            let reserved: f32 = widths.iter().map(|w| w + 6.0).sum();
            let text_w = (column.w - reserved - 12.0).max(1.0);
            let text_h = KoruTextParagraph::new(&text, theme::role::meta(), "", p.text)
                .measure(&mut self.engine, text_w);
            let mut x = column.x + column.w;
            for ((control, label), w) in answers.iter().zip(&widths).rev() {
                x -= w;
                controls.push(Placed {
                    control: *control,
                    rect: Rect::new(x, cy, *w, ActionButton::H),
                    label: (*label).into(),
                    look: Look::Action(ActionStyle::Ghost),
                });
                x -= 6.0;
            }
            let rect = Rect::new(column.x, cy, text_w, text_h);
            cy += text_h.max(ActionButton::H) + LINE_GAP;
            (rect, text)
        });

        // What is true of the whole list: nothing answering, still looking,
        // or nothing there.
        let mut quiet = Vec::new();
        let availability = self.w.borrow().availability();
        let said = if let Some(reason) = self.w.borrow().unavailable_reason() {
            Some(reason)
        } else if availability.is_loading() && rows.is_empty() {
            Some(LOADING.to_owned())
        } else if !availability.is_loading() && active.is_empty() {
            Some(EMPTY.to_owned())
        } else {
            None
        };
        if let Some(text) = said {
            let h = self.quiet_height(&text, column.w);
            quiet.push((Rect::new(column.x, cy, column.w, h), text));
            cy += h + LINE_GAP;
        }

        let mut lists = Vec::new();
        if !active.is_empty() {
            let list = self.lay_list(
                ListKind::Active,
                None,
                &active,
                column,
                cy,
                controls,
                disabled,
            );
            cy = list.card.y + list.card.h + LINE_GAP;
            lists.push(list);
        }

        // *Show archived (n)* and *Show trash (n)*, each only while there is
        // something to show or its list is open.
        let archived_n = self.w.borrow().count_of(owner, Group::Archived);
        let trash_n = self.w.borrow().count_of(owner, Group::Deleted);
        let mut x = column.x;
        let mut links = false;
        for (control, open, n, show, hide) in [
            (
                Control::ShowArchived,
                self.w.borrow().show_archived(),
                archived_n,
                "Show archived",
                "Hide archived",
            ),
            (
                Control::ShowTrash,
                self.w.borrow().show_trash(),
                trash_n,
                "Show trash",
                "Hide trash",
            ),
        ] {
            if n == 0 && !open {
                continue;
            }
            let label = format!("{} ({n})", if open { hide } else { show });
            let w = self
                .engine
                .simple_layout(&label, theme::role::meta())
                .width();
            controls.push(Placed {
                control,
                rect: Rect::new(x, cy, w, meta_line),
                label,
                look: Look::Link,
            });
            x += w + LINK_GAP;
            links = true;
        }
        if links {
            cy += meta_line + LINE_GAP;
        }

        let show_archived = self.w.borrow().show_archived();
        let show_trash = self.w.borrow().show_trash();
        for (kind, open, group, label) in [
            (ListKind::Archived, show_archived, Group::Archived, ARCHIVED),
            (ListKind::Trash, show_trash, Group::Deleted, TRASH),
        ] {
            if !open {
                continue;
            }
            let members: Vec<&Catalog> = rows
                .iter()
                .filter(|(g, _)| *g == group)
                .map(|(_, row)| row)
                .collect();
            let label_rect = Rect::new(column.x, cy, column.w, meta_line);
            cy += meta_line + 6.0;
            let mut list = if members.is_empty() {
                WorkspaceList {
                    kind,
                    label: None,
                    card: Rect::new(column.x, cy, column.w, 0.0),
                    rows: Vec::new(),
                    notes: Vec::new(),
                }
            } else {
                let list = self.lay_list(kind, None, &members, column, cy, controls, disabled);
                cy = list.card.y + list.card.h + 6.0;
                list
            };
            list.label = Some((label_rect, label));
            // One reason for the list's one kind of button.
            let refused = members.iter().find_map(|row| {
                let action = match kind {
                    ListKind::Archived => Action::Archive(row.id, false),
                    _ => Action::Restore(row.id),
                };
                self.w.borrow().reason_for(&action, row)
            });
            let mut notes: Vec<String> = refused.into_iter().collect();
            if kind == ListKind::Trash {
                notes.push(TRASH_NOTE.to_owned());
            }
            for text in notes {
                let h = self.quiet_height(&text, column.w);
                list.notes
                    .push((Rect::new(column.x, cy, column.w, h), text));
                cy += h + 4.0;
            }
            cy += LINE_GAP - 4.0;
            lists.push(list);
        }

        let bottom = cy - LINE_GAP;
        let section = WorkspacesSection {
            rect: Rect::new(column.x, y, column.w, bottom - y),
            heading,
            count,
            notice,
            quiet,
            lists,
        };
        (section, bottom)
    }

    fn quiet_height(&mut self, text: &str, w: f32) -> f32 {
        KoruTextParagraph::new(text, theme::role::meta(), "", theme::p().text_dim)
            .measure(&mut self.engine, w)
    }

    #[allow(clippy::too_many_arguments)]
    fn lay_list(
        &mut self,
        kind: ListKind,
        label: Option<(Rect, &'static str)>,
        members: &[&Catalog],
        column: Rect,
        y: f32,
        controls: &mut Vec<Placed>,
        disabled: &mut Vec<Control>,
    ) -> WorkspaceList {
        let now = now_ms();
        let mut rows = Vec::new();
        for (index, catalog) in members.iter().enumerate() {
            let rect = Rect::new(column.x, y + index as f32 * ROW_H, column.w, ROW_H);
            rows.push(self.lay_workspace_row(kind, catalog, rect, now, controls, disabled));
        }
        WorkspaceList {
            kind,
            label,
            card: Rect::new(column.x, y, column.w, members.len() as f32 * ROW_H),
            rows,
            notes: Vec::new(),
        }
    }

    fn lay_workspace_row(
        &mut self,
        kind: ListKind,
        catalog: &Catalog,
        rect: Rect,
        now: i64,
        controls: &mut Vec<Placed>,
        disabled: &mut Vec<Control>,
    ) -> WorkspaceRow {
        let id = catalog.id;
        let tile = Rect::new(
            rect.x + ROW_PAD,
            rect.y + (ROW_H - ROW_TILE) * 0.5,
            ROW_TILE,
            ROW_TILE,
        );
        controls.push(Placed {
            control: Control::WorkspaceRow(id),
            rect,
            label: catalog.name.clone(),
            look: Look::Row,
        });
        // The row's one trailing control.
        let (control, label, look, w, h) = match kind {
            ListKind::Active => (
                Control::WorkspaceMore(id),
                format!("More actions for {}", catalog.name),
                Look::More,
                MORE,
                MORE,
            ),
            ListKind::Archived | ListKind::Trash => {
                let (control, label, action) = match kind {
                    ListKind::Archived => (
                        Control::Unarchive(id),
                        UNARCHIVE,
                        Action::Archive(id, false),
                    ),
                    _ => (Control::RestoreWorkspace(id), RESTORE, Action::Restore(id)),
                };
                if self.w.borrow().reason_for(&action, catalog).is_some() {
                    disabled.push(control);
                }
                let w =
                    ActionButton::new(label, ActionStyle::Secondary, None).width(&mut self.engine);
                (
                    control,
                    label.to_owned(),
                    Look::Action(ActionStyle::Secondary),
                    w,
                    ActionButton::H,
                )
            }
        };
        let trailing = Rect::new(
            rect.x + rect.w - MORE_INSET - w,
            rect.y + (ROW_H - h) * 0.5,
            w,
            h,
        );
        controls.push(Placed {
            control,
            rect: trailing,
            label,
            look,
        });
        // Tags, right to left before the trailing control.
        let mut tag_names = Vec::new();
        if kind == ListKind::Active && self.w.borrow().effective_pinned(catalog) {
            tag_names.push(PINNED_TAG);
        }
        if kind == ListKind::Active
            && self
                .current_workspace
                .is_some_and(|open| catalog_workspace(open) == id)
        {
            tag_names.push(OPEN_TAG);
        }
        let mut right = trailing.x - 10.0;
        let mut tags = Vec::new();
        for name in tag_names.into_iter().rev() {
            let chip = koru_ui::LabelChip::new(name);
            let (w, h) = (chip.width(&mut self.engine), chip.height());
            right -= w;
            tags.push((Rect::new(right, rect.y + (ROW_H - h) * 0.5, w, h), name));
            right -= TAG_GAP;
        }
        tags.reverse();
        let text_x = tile.x + ROW_TILE + ROW_PAD;
        let room = (right - 6.0 - text_x).max(1.0);
        let name_style = theme::role::row();
        let name_text = self.engine.elided(&catalog.name, name_style, room);
        let name_w = self
            .engine
            .simple_layout(&name_text, name_style)
            .width()
            .min(room);
        let name_line = name_style.metrics.line_height;
        let sub_line = theme::role::meta().metrics.line_height;
        let sub_text = catalog
            .authority_reason(crate::format::lang())
            .or_else(|| catalog.last_opened_at.map(|then| opened_line(then, now)));
        let block = name_line + sub_text.as_ref().map_or(0.0, |_| sub_line);
        let top = rect.y + (ROW_H - block) * 0.5;
        let name = Rect::new(text_x, top, name_w, name_line);
        let sub = sub_text.map(|text| {
            let text = self.engine.elided(&text, theme::role::meta(), room);
            (Rect::new(text_x, top + name_line, room, sub_line), text)
        });
        WorkspaceRow {
            id,
            rect,
            tile,
            icon: catalog.icon.clone(),
            color: catalog.color.clone(),
            name,
            name_text,
            full_name: catalog.name.clone(),
            sub,
            tags,
        }
    }

    // ---- input ---------------------------------------------------------------

    /// What a workspace control does when pressed. `None` for a control that
    /// is not the section's.
    pub(super) fn activate_workspace_control(&mut self, control: Control) -> Option<bool> {
        let profile = self.laid.profile?;
        let done = match control {
            Control::ImportWorkspaces => {
                self.requests.push(Request::ImportContent);
                true
            }
            Control::NewWorkspace => {
                self.requests.push(Request::NewWorkspace(profile));
                true
            }
            // A click selects; a double-click or Enter opens.
            Control::WorkspaceRow(id) => {
                self.w.borrow_mut().set_selected(Some(id));
                true
            }
            Control::WorkspaceMore(id) => {
                self.w.borrow_mut().set_selected(Some(id));
                let Some(rect) = self.laid.placed(control).map(|placed| placed.rect) else {
                    return Some(false);
                };
                self.requests.push(Request::WorkspaceMenu {
                    workspace: app_workspace(id),
                    at: (rect.x + rect.w, rect.y + rect.h - self.page_scroll),
                });
                true
            }
            Control::ShowArchived => {
                self.w.borrow_mut().perform(Action::ShowArchived);
                true
            }
            Control::ShowTrash => {
                self.w.borrow_mut().perform(Action::ShowTrash);
                true
            }
            Control::Unarchive(id) => {
                self.w.borrow_mut().perform(Action::Archive(id, false));
                true
            }
            Control::RestoreWorkspace(id) => {
                self.w.borrow_mut().perform(Action::Restore(id));
                true
            }
            Control::NoticeRetry => {
                self.w.borrow_mut().perform(Action::Retry);
                true
            }
            Control::NoticeDismiss => {
                self.w.borrow_mut().perform(Action::DismissNotice);
                true
            }
            _ => return None,
        };
        self.relayout();
        self.clamp_scroll();
        Some(done)
    }

    // ---- paint ---------------------------------------------------------------

    pub(super) fn paint_workspaces_section(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(section) = self.laid.workspaces.clone() else {
            return;
        };
        let laid = self.laid.clone();
        let p = theme::p();
        let r = section.rect;
        let _section = inspect::scope(
            "Manage workspaces",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("count", section.count.1.clone(), "rows_of(profile)");
                rec.property(
                    "show archived",
                    self.w.borrow().show_archived(),
                    "controller",
                );
                rec.property("show trash", self.w.borrow().show_trash(), "controller");
                rec.property(
                    "implementation",
                    "apps/koru manage::workspaces",
                    "app implementation",
                );
                rec.property(
                    "data",
                    "koru_workspaces_ui::WorkspacesController",
                    "shared controller",
                );
            },
        );
        let line = Rect::new(
            section.heading.x,
            section.heading.y,
            section.count.0.x + section.count.0.w - section.heading.x,
            section.heading.h,
        );
        self.paint_caption(canvas, root, line, TITLE);
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &count_label(&section.count.1),
            section.count.0.x,
            section.count.0.y,
            theme::role::meta(),
            "theme::role::meta() — the count",
            p.placeholder,
        );
        for control in [Control::ImportWorkspaces, Control::NewWorkspace] {
            if let Some(placed) = laid.placed(control) {
                self.paint_control(canvas, root, placed);
            }
        }
        if let Some((rect, text)) = &section.notice {
            KoruTextParagraph::new(text, theme::role::meta(), "theme::role::meta()", p.text).draw(
                canvas,
                root,
                &mut self.engine,
                *rect,
            );
            for control in [Control::NoticeRetry, Control::NoticeDismiss] {
                if let Some(placed) = laid.placed(control) {
                    self.paint_control(canvas, root, placed);
                }
            }
        }
        for (rect, text) in &section.quiet {
            KoruTextParagraph::new(text, theme::role::meta(), "theme::role::meta()", p.text_dim)
                .draw(canvas, root, &mut self.engine, *rect);
        }
        for control in [Control::ShowArchived, Control::ShowTrash] {
            if let Some(placed) = laid.placed(control) {
                self.paint_control(canvas, root, placed);
            }
        }
        for list in &section.lists {
            let part = match list.kind {
                ListKind::Active => None,
                ListKind::Archived => Some("Manage archived"),
                ListKind::Trash => Some("Manage trash"),
            };
            let whole = list.label.map_or(list.card, |(label, _)| {
                Rect::new(
                    label.x,
                    label.y,
                    list.card.w,
                    list.card.y + list.card.h - label.y,
                )
            });
            let bottom = list
                .notes
                .last()
                .map_or(whole.y + whole.h, |(rect, _)| rect.y + rect.h);
            let _list = part.map(|part| {
                inspect::scope(
                    part,
                    "part",
                    root,
                    scene::rect(whole.x, whole.y, whole.w, bottom - whole.y),
                    |rec| {
                        rec.property("rows", list.rows.len(), "rows_of(profile)");
                        rec.property("delete forever", false, "decision 5");
                    },
                )
            });
            if let Some((rect, text)) = list.label {
                koru_ui::text_paragraph::draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    text,
                    rect.x,
                    rect.y,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim,
                );
            }
            if !list.rows.is_empty() {
                let dividers: Vec<f32> = list.rows.iter().skip(1).map(|row| row.rect.y).collect();
                let _card =
                    Card::new(None).draw(canvas, root, &mut self.engine, list.card, &dividers, 1.0);
                for row in &list.rows {
                    self.paint_workspace_row(canvas, root, list.kind, row, &laid);
                }
            }
            for (rect, text) in &list.notes {
                KoruTextParagraph::new(
                    text,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim,
                )
                .draw(canvas, root, &mut self.engine, *rect);
            }
        }
    }

    fn paint_workspace_row(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        kind: ListKind,
        row: &WorkspaceRow,
        laid: &profile_page::Laid,
    ) {
        let p = theme::p();
        let r = row.rect;
        let control = Control::WorkspaceRow(row.id);
        let hovered = self.page_hover == Some(control);
        let focused = self.page_focus == Some(control);
        let selected = self.w.borrow().selected() == Some(row.id);
        let _row = inspect::scope(
            "Manage workspace row",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("row height", ROW_H, "workspaces::ROW_H");
                rec.property("list", format!("{kind:?}"), "rows_of(profile)");
                rec.property("selected", selected, "controller");
                rec.property(
                    "sub-line",
                    row.sub.as_ref().map_or("none", |(_, text)| text.as_str()),
                    "Row::last_opened_at (decision 8)",
                );
                rec.property("menu", "the window's workspace menu", "Request");
            },
        );
        let inner = scene::rrect(r.x + 4.0, r.y + 3.0, r.w - 8.0, r.h - 6.0, 7.0);
        if selected {
            scene::fill(canvas, root, p.row_selected, &inner);
        }
        if hovered {
            scene::fill(canvas, root, p.hover_tint, &inner);
        }
        if focused {
            scene::stroke(canvas, root, p.focus_ring, theme::BORDER_W * 2.0, &inner);
        }
        let dim = kind != ListKind::Active;
        Tile {
            icon: row.icon.as_deref(),
            color: row.color.as_deref(),
            name: &row.full_name,
        }
        .draw(canvas, root, &mut self.engine, row.tile);
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &row.name_text,
            row.name.x,
            row.name.y,
            theme::role::row(),
            "theme::role::row()",
            if dim { p.text_dim } else { p.text },
        );
        if let Some((rect, text)) = &row.sub {
            koru_ui::text_paragraph::draw_line(
                canvas,
                root,
                &mut self.engine,
                text,
                rect.x,
                rect.y,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
        for (rect, name) in &row.tags {
            let _tag = inspect::scope(
                "Manage workspace tag",
                "part",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |rec| rec.property("tag", *name, "the row's state"),
            );
            koru_ui::LabelChip::new(name).draw(
                canvas,
                root,
                &mut self.engine,
                *rect,
                koru_ui::label_chip::State::default(),
            );
        }
        for trailing in [
            Control::WorkspaceMore(row.id),
            Control::Unarchive(row.id),
            Control::RestoreWorkspace(row.id),
        ] {
            if let Some(placed) = laid.placed(trailing) {
                self.paint_control(canvas, root, placed);
            }
        }
    }
}
