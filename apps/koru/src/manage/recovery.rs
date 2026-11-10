// SPDX-License-Identifier: MPL-2.0

//! A Profile page's **recovery sections** (root-catalog TECHNICAL.final
//! §3.6, §3.7, §6.3), under its Workspaces and only while there is something
//! to recover:
//!
//! ```text
//! Unassigned (from deleted Profile)
//! ┌──────────────────────────────────────────────────────────────┐
//! │ [▣] Orphaned notes                 [Restore Profile]     ••• │
//! │     Unassigned (from deleted Profile)                         │
//! └──────────────────────────────────────────────────────────────┘
//! Deleted profiles
//! ┌──────────────────────────────────────────────────────────────┐
//! │ [▣] Work                                             [Restore] │
//! │     Deleted 11/09/2026 14:02 · 3 workspaces · 1 no longer available │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! **The data is the shared [`WorkspacesController`](koru_workspaces_ui::WorkspacesController)**,
//! read with its catalog: `unassigned()` and `deleted_profiles()`. This file
//! places what it says and turns a press into one of its `Action`s or a
//! [`Request`] for what only the window can do — an Unassigned row's `•••`
//! opens the window's workspace menu, whose *Move to Profile ▸* flyout is the
//! destination picker, the same control every other row has. *Restore
//! Profile* and *Restore* are the batch restore; the outcome is the
//! controller's notice, drawn in the Workspaces section above.
//!
//! On every Profile's page, because recovery must be reachable from any
//! active Profile (§6.3), and from none at all — that is the first-run
//! window's recovery screen.

use super::profile_page::{Control, Look, Placed};
use super::workspaces::{ROW_H, app_profile, app_workspace};
use super::*;
use koru_draw::geometry::Rect;
use koru_draw::tile::Tile;
use koru_i18n::{Key as Words, fill, number::decimal, t};
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::card::Card;
use koru_workspaces_ui::{Action, DeletedProfileRow, Row as Catalog, UnassignedRow};

const ROW_PAD: f32 = 12.0;
const ROW_TILE: f32 = 32.0;
const MORE: f32 = 28.0;
const MORE_INSET: f32 = 10.0;
const LINE_GAP: f32 = 10.0;

/// One row of either list, where everything on it goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecoveryRow {
    pub control: Control,
    pub rect: Rect,
    pub tile: Rect,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub name: Rect,
    pub name_text: String,
    pub full_name: String,
    pub sub: (Rect, String),
}

/// One list: its caption line, its card and its rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecoveryList {
    pub label: (Rect, String),
    pub card: Rect,
    pub rows: Vec<RecoveryRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecoverySection {
    pub rect: Rect,
    pub unassigned: Option<RecoveryList>,
    pub deleted: Option<RecoveryList>,
}

impl ManageView {
    /// Lay both lists under `y`, each only while it has rows; answers the
    /// section and where the page continues.
    pub(super) fn lay_recovery_sections(
        &mut self,
        column: Rect,
        y: f32,
        controls: &mut Vec<Placed>,
        disabled: &mut Vec<Control>,
    ) -> (Option<RecoverySection>, f32) {
        let (unassigned, deleted) = {
            let w = self.w.borrow();
            (w.unassigned().to_vec(), w.deleted_profiles().to_vec())
        };
        if unassigned.is_empty() && deleted.is_empty() {
            return (None, y);
        }
        let lang = crate::format::lang();
        let mut cy = y;
        let unassigned = (!unassigned.is_empty()).then(|| {
            let rows: Vec<RecoveryRow> = {
                let mut out = Vec::new();
                let top = cy + theme::role::meta().metrics.line_height + 6.0;
                for (i, row) in unassigned.iter().enumerate() {
                    let rect = Rect::new(column.x, top + i as f32 * ROW_H, column.w, ROW_H);
                    out.push(self.lay_unassigned_row(row, rect, lang, controls, disabled));
                }
                out
            };
            let list = self.recovery_list(
                t(lang, Words::WorkspacesUnassignedHeading).to_owned(),
                column,
                cy,
                rows,
            );
            cy = list.card.y + list.card.h + LINE_GAP;
            list
        });
        let deleted = (!deleted.is_empty()).then(|| {
            let rows: Vec<RecoveryRow> = {
                let mut out = Vec::new();
                let top = cy + theme::role::meta().metrics.line_height + 6.0;
                for (i, row) in deleted.iter().enumerate() {
                    let rect = Rect::new(column.x, top + i as f32 * ROW_H, column.w, ROW_H);
                    out.push(self.lay_deleted_row(row, rect, lang, controls, disabled));
                }
                out
            };
            let list = self.recovery_list(
                t(lang, Words::WorkspacesDeletedProfiles).to_owned(),
                column,
                cy,
                rows,
            );
            cy = list.card.y + list.card.h + LINE_GAP;
            list
        });
        let bottom = cy - LINE_GAP;
        (
            Some(RecoverySection {
                rect: Rect::new(column.x, y, column.w, bottom - y),
                unassigned,
                deleted,
            }),
            bottom,
        )
    }

    fn recovery_list(
        &mut self,
        label: String,
        column: Rect,
        y: f32,
        rows: Vec<RecoveryRow>,
    ) -> RecoveryList {
        let meta_line = theme::role::meta().metrics.line_height;
        let label_rect = Rect::new(column.x, y, column.w, meta_line);
        let top = y + meta_line + 6.0;
        let card = Rect::new(column.x, top, column.w, rows.len() as f32 * ROW_H);
        RecoveryList {
            label: (label_rect, label),
            card,
            rows,
        }
    }

    /// An Unassigned row: the name, the reason under it, *Restore Profile*
    /// when this device can restore the Profile it came from, and the `•••`
    /// that opens the window's workspace menu (*Move to Profile ▸*).
    fn lay_unassigned_row(
        &mut self,
        row: &UnassignedRow,
        rect: Rect,
        lang: koru_i18n::Lang,
        controls: &mut Vec<Placed>,
        disabled: &mut Vec<Control>,
    ) -> RecoveryRow {
        let id = row.id;
        let tile = Rect::new(
            rect.x + ROW_PAD,
            rect.y + (ROW_H - ROW_TILE) * 0.5,
            ROW_TILE,
            ROW_TILE,
        );
        let reason = row.hint_key();
        controls.push(Placed {
            control: Control::UnassignedRow(id),
            rect,
            label: format!("{}, {}", row.name, t(lang, reason)),
            look: Look::Row,
        });
        let more = Rect::new(
            rect.x + rect.w - MORE_INSET - MORE,
            rect.y + (ROW_H - MORE) * 0.5,
            MORE,
            MORE,
        );
        controls.push(Placed {
            control: Control::UnassignedMore(id),
            rect: more,
            label: format!("{}: {}", t(lang, Words::WorkspacesMoveTo), row.name),
            look: Look::More,
        });
        let mut right = more.x - 10.0;
        let restorable = row
            .former_profile
            .filter(|_| row.from_deleted_profile())
            .and_then(|former| self.w.borrow().deletion_of(former).map(|d| (former, d)));
        if let Some((former, deletion)) = restorable {
            let label = t(lang, Words::WorkspacesRestoreProfile);
            let w = ActionButton::new(label, ActionStyle::Secondary, None).width(&mut self.engine);
            right -= w;
            let button = Rect::new(
                right,
                rect.y + (ROW_H - ActionButton::H) * 0.5,
                w,
                ActionButton::H,
            );
            controls.push(Placed {
                control: Control::RestoreUnassignedProfile(id),
                rect: button,
                label: label.to_owned(),
                look: Look::Action(ActionStyle::Secondary),
            });
            if self
                .w
                .borrow()
                .reason_for(
                    &Action::RestoreProfile(former, deletion),
                    &Catalog::default(),
                )
                .is_some()
            {
                disabled.push(Control::RestoreUnassignedProfile(id));
            }
            right -= 10.0;
        }
        self.recovery_row_text(
            Control::UnassignedRow(id),
            rect,
            tile,
            row.icon.clone(),
            row.color.clone(),
            &row.name,
            t(lang, reason).to_owned(),
            right,
        )
    }

    /// A deleted Profile's row: its retained name, when and how many under
    /// it, and *Restore*.
    fn lay_deleted_row(
        &mut self,
        row: &DeletedProfileRow,
        rect: Rect,
        lang: koru_i18n::Lang,
        controls: &mut Vec<Placed>,
        disabled: &mut Vec<Control>,
    ) -> RecoveryRow {
        let profile = app_profile(row.profile);
        let tile = Rect::new(
            rect.x + ROW_PAD,
            rect.y + (ROW_H - ROW_TILE) * 0.5,
            ROW_TILE,
            ROW_TILE,
        );
        let name = row
            .name
            .clone()
            .unwrap_or_else(|| t(lang, Words::WorkspacesProfiles).to_owned());
        let label = t(lang, Words::WorkspacesRestoreDeleted);
        let w = ActionButton::new(label, ActionStyle::Secondary, None).width(&mut self.engine);
        let button = Rect::new(
            rect.x + rect.w - MORE_INSET - w,
            rect.y + (ROW_H - ActionButton::H) * 0.5,
            w,
            ActionButton::H,
        );
        controls.push(Placed {
            control: Control::RestoreDeletedProfile(profile),
            rect: button,
            label: label.to_owned(),
            look: Look::Action(ActionStyle::Secondary),
        });
        if self
            .w
            .borrow()
            .reason_for(
                &Action::RestoreProfile(row.profile, row.deletion),
                &Catalog::default(),
            )
            .is_some()
        {
            disabled.push(Control::RestoreDeletedProfile(profile));
        }
        self.recovery_row_text(
            Control::RestoreDeletedProfile(profile),
            rect,
            tile,
            None,
            None,
            &name,
            deleted_line(lang, row),
            button.x - 10.0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn recovery_row_text(
        &mut self,
        control: Control,
        rect: Rect,
        tile: Rect,
        icon: Option<String>,
        color: Option<String>,
        full_name: &str,
        sub_text: String,
        right: f32,
    ) -> RecoveryRow {
        let text_x = tile.x + ROW_TILE + ROW_PAD;
        let room = (right - 6.0 - text_x).max(1.0);
        let name_style = theme::role::row();
        let name_text = self.engine.elided(full_name, name_style, room);
        let name_w = self
            .engine
            .simple_layout(&name_text, name_style)
            .width()
            .min(room);
        let name_line = name_style.metrics.line_height;
        let sub_line = theme::role::meta().metrics.line_height;
        let top = rect.y + (ROW_H - name_line - sub_line) * 0.5;
        let sub_text = self.engine.elided(&sub_text, theme::role::meta(), room);
        RecoveryRow {
            control,
            rect,
            tile,
            icon,
            color,
            name: Rect::new(text_x, top, name_w, name_line),
            name_text,
            full_name: full_name.to_owned(),
            sub: (Rect::new(text_x, top + name_line, room, sub_line), sub_text),
        }
    }

    /// What a recovery control does when pressed. `None` for a control that
    /// is not the section's.
    pub(super) fn activate_recovery_control(&mut self, control: Control) -> Option<bool> {
        let done = match control {
            Control::UnassignedRow(_) => true,
            Control::UnassignedMore(id) => {
                let Some(rect) = self.laid.placed(control).map(|placed| placed.rect) else {
                    return Some(false);
                };
                self.requests.push(Request::WorkspaceMenu {
                    workspace: app_workspace(id),
                    at: (rect.x + rect.w, rect.y + rect.h - self.page_scroll),
                });
                true
            }
            Control::RestoreUnassignedProfile(id) => {
                let restore = {
                    let w = self.w.borrow();
                    w.unassigned_row(id)
                        .and_then(|row| row.former_profile)
                        .and_then(|former| w.deletion_of(former).map(|d| (former, d)))
                };
                if let Some((former, deletion)) = restore {
                    self.w
                        .borrow_mut()
                        .perform(Action::RestoreProfile(former, deletion));
                }
                true
            }
            Control::RestoreDeletedProfile(profile) => {
                let deletion = self
                    .w
                    .borrow()
                    .deletion_of(super::workspaces::catalog_profile(profile));
                if let Some(deletion) = deletion {
                    self.w.borrow_mut().perform(Action::RestoreProfile(
                        super::workspaces::catalog_profile(profile),
                        deletion,
                    ));
                }
                true
            }
            _ => return None,
        };
        self.relayout();
        self.clamp_scroll();
        Some(done)
    }

    pub(super) fn paint_recovery_sections(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(section) = self.laid.recovery.clone() else {
            return;
        };
        let laid = self.laid.clone();
        let p = theme::p();
        let r = section.rect;
        let _section = inspect::scope(
            "Manage recovery",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property(
                    "unassigned",
                    section
                        .unassigned
                        .as_ref()
                        .map_or(0, |list| list.rows.len()),
                    "controller.unassigned()",
                );
                rec.property(
                    "deleted profiles",
                    section.deleted.as_ref().map_or(0, |list| list.rows.len()),
                    "controller.deleted_profiles()",
                );
                rec.property(
                    "data",
                    "koru_workspaces_ui::WorkspacesController",
                    "shared controller",
                );
            },
        );
        for list in [section.unassigned.as_ref(), section.deleted.as_ref()]
            .into_iter()
            .flatten()
        {
            let (label_rect, label) = &list.label;
            self.paint_caption(canvas, root, *label_rect, label);
            let dividers: Vec<f32> = list.rows.iter().skip(1).map(|row| row.rect.y).collect();
            let _card =
                Card::new(None).draw(canvas, root, &mut self.engine, list.card, &dividers, 1.0);
            for row in &list.rows {
                let hovered = self.page_hover == Some(row.control);
                let focused = self.page_focus == Some(row.control);
                let inner = scene::rrect(
                    row.rect.x + 4.0,
                    row.rect.y + 3.0,
                    row.rect.w - 8.0,
                    row.rect.h - 6.0,
                    7.0,
                );
                if hovered && matches!(row.control, Control::UnassignedRow(_)) {
                    scene::fill(canvas, root, p.hover_tint, &inner);
                }
                if focused && matches!(row.control, Control::UnassignedRow(_)) {
                    scene::stroke(canvas, root, p.focus_ring, theme::BORDER_W * 2.0, &inner);
                }
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
                    p.text,
                );
                let (sub_rect, sub_text) = &row.sub;
                koru_ui::text_paragraph::draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    sub_text,
                    sub_rect.x,
                    sub_rect.y,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim,
                );
                for placed in laid.controls.iter().filter(|placed| {
                    placed.rect.y >= row.rect.y
                        && placed.rect.y + placed.rect.h <= row.rect.y + row.rect.h + 0.5
                        && placed.look != Look::Row
                        && matches!(
                            placed.control,
                            Control::UnassignedMore(_)
                                | Control::RestoreUnassignedProfile(_)
                                | Control::RestoreDeletedProfile(_)
                        )
                }) {
                    self.paint_control(canvas, root, placed);
                }
            }
        }
    }
}

/// A deleted Profile's line: when, how many workspaces, how many gone.
pub(crate) fn deleted_line(lang: koru_i18n::Lang, row: &DeletedProfileRow) -> String {
    let when = crate::format::absolute_local(lang, row.at_ms);
    let mut line = match row.targets {
        1 => fill(t(lang, Words::WorkspacesDeletedProfileLineOne), &[&when]),
        n => fill(
            t(lang, Words::WorkspacesDeletedProfileLineMany),
            &[&when, &decimal(lang, n as u64)],
        ),
    };
    match row.expired_or_purged {
        0 => {}
        1 => {
            line.push_str(" · ");
            line.push_str(t(lang, Words::WorkspacesDeletedProfileExpiredOne));
        }
        n => {
            line.push_str(" · ");
            line.push_str(&fill(
                t(lang, Words::WorkspacesDeletedProfileExpiredMany),
                &[&decimal(lang, n as u64)],
            ));
        }
    }
    line
}
