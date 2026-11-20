// SPDX-License-Identifier: MPL-2.0

//! One syncing Profile's upload progress, open under its row on Koru Cloud
//! (maintainers' decision 20): the summary line, then one row per workspace — a held
//! row opens onto the documents it is waiting for.
//!
//! What the rows say, and the poll behind them, are the controller's
//! (`manage::profiles::progress`), word for word what Manage Profiles' section
//! said. This file places them in the page — they scroll with it, so the
//! section needs no scroll of its own — and paints them. A row opens only when
//! [`openable`](crate::manage::profiles) says so; the layout places a control
//! for exactly those rows, so paint, the pointer and Tab cannot disagree.

use super::*;
use crate::manage::profiles::{WAITING_CAP, waiting_lines};

/// The summary line and the gap under it.
const SUMMARY_H: f32 = 28.0;
/// A workspace's name, then its one line of counts.
const ROW_H: f32 = 44.0;
const NAME_TO_DETAIL: f32 = 20.0;
/// One waiting document under an open row.
const LINE_H: f32 = 22.0;
/// How far a waiting document sits in from its workspace's name.
const LINE_INDENT: f32 = 16.0;
/// The disclosure triangle's lane, left of the text it opens.
pub(crate) const DISCLOSURE_W: f32 = 14.0;

/// The open progress, unscrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProgressLaid {
    pub rect: Rect,
    pub summary: Rect,
    pub summary_text: String,
    pub rows: Vec<ProgressRow>,
}

/// One workspace's row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProgressRow {
    pub id: workspaces::Wid,
    pub rect: Rect,
    pub openable: bool,
    pub open: bool,
    pub name: Rect,
    pub name_text: String,
    pub detail: Rect,
    pub detail_text: String,
    /// Under an open row: each waiting document, or *reading…*.
    pub lines: Vec<(Rect, String)>,
}

impl ManageView {
    /// Lay the open progress out from `(x, y)`, `w` wide. `None` while the
    /// controller has no section — the Profile stopped syncing.
    pub(super) fn lay_progress(
        &mut self,
        x: f32,
        w: f32,
        y: f32,
        controls: &mut Vec<Placed>,
    ) -> Option<ProgressLaid> {
        // Beside *held* on the summary line: what the account keeps under
        // this Profile that is in the Trash here (trash-recovery P4).
        let trashed_here = self.c.cloud_progress_profile().map_or(0, |profile| {
            self.w
                .borrow()
                .trashed_in_account_of(crate::manage::workspaces::catalog_profile(profile))
                .len()
        });
        let lines = self.c.cloud_progress_lines(trashed_here)?;
        let lang = crate::format::lang();
        let text_x = x + DISCLOSURE_W;
        let text_w = (w - DISCLOSURE_W).max(1.0);
        let summary_text = self
            .engine
            .elided(&lines.summary, theme::role::row(), text_w);
        let summary = Rect::new(text_x, y, text_w, theme::role::row().metrics.line_height);
        let mut cy = y + SUMMARY_H;
        let mut rows = Vec::new();
        for row in &lines.rows {
            let room = row
                .waiting
                .as_ref()
                .map_or(0, |waiting| waiting_lines(lang, waiting, WAITING_CAP).len());
            let rect = Rect::new(x - 4.0, cy - 4.0, w + 8.0, ROW_H - 2.0);
            if row.openable {
                controls.push(Placed {
                    control: Control::ProgressRow(row.id),
                    rect,
                    label: format!("{}, {}", row.name, row.detail),
                    look: Look::Row,
                });
            }
            let name_text = self.engine.elided(&row.name, theme::role::row(), text_w);
            let detail_text = self.engine.elided(&row.detail, theme::role::meta(), text_w);
            let name = Rect::new(text_x, cy, text_w, theme::role::row().metrics.line_height);
            let detail = Rect::new(
                text_x,
                cy + NAME_TO_DETAIL,
                text_w,
                theme::role::meta().metrics.line_height,
            );
            cy += ROW_H;
            let mut waiting = Vec::new();
            if let Some(open) = &row.waiting {
                let line_w = (text_w - LINE_INDENT).max(1.0);
                for line in waiting_lines(lang, open, room) {
                    let line = self.engine.elided(&line, theme::role::meta(), line_w);
                    waiting.push((Rect::new(text_x + LINE_INDENT, cy, line_w, LINE_H), line));
                    cy += LINE_H;
                }
            }
            rows.push(ProgressRow {
                id: row.id,
                rect,
                openable: row.openable,
                open: row.waiting.is_some(),
                name,
                name_text,
                detail,
                detail_text,
                lines: waiting,
            });
        }
        Some(ProgressLaid {
            rect: Rect::new(x, y, w, cy - y),
            summary,
            summary_text,
            rows,
        })
    }

    pub(super) fn paint_progress(&mut self, canvas: &mut Scene, root: Affine, laid: &ProgressLaid) {
        let p = theme::p();
        let r = laid.rect;
        let _progress = inspect::scope(
            "Manage cloud progress",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("workspaces", laid.rows.len(), "the controller's poll");
                rec.property(
                    "open",
                    laid.rows.iter().filter(|row| row.open).count(),
                    "held rows opened onto their waiting documents",
                );
                rec.property(
                    "refresh",
                    "1 s, only while Koru Cloud is in front",
                    "profiles::progress::REFRESH_EVERY",
                );
            },
        );
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &laid.summary_text,
            laid.summary.x,
            laid.summary.y,
            theme::role::row(),
            "theme::role::row()",
            p.text,
        );
        for row in &laid.rows {
            let control = Control::ProgressRow(row.id);
            if row.openable {
                let rect = row.rect;
                let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0);
                if self.page_hover == Some(control) {
                    scene::fill(canvas, root, p.hover_tint, &shape);
                }
                if self.page_focus == Some(control) {
                    scene::stroke(canvas, root, p.focus_ring, theme::BORDER_W * 2.0, &shape);
                }
                disclosure(
                    canvas,
                    root,
                    row.name.x - DISCLOSURE_W,
                    row.name.y + 4.0,
                    row.open,
                    p.text_dim,
                );
            }
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
            koru_ui::text_paragraph::draw_line(
                canvas,
                root,
                &mut self.engine,
                &row.detail_text,
                row.detail.x,
                row.detail.y,
                theme::role::meta(),
                "theme::role::meta() — today's progress vocabulary",
                p.text_dim,
            );
            for (rect, line) in &row.lines {
                koru_ui::text_paragraph::draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    line,
                    rect.x,
                    rect.y,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim,
                );
            }
        }
    }
}

/// A small triangle: pointing right when closed, down when open — the
/// disclosure Manage Profiles' progress rows drew.
pub(in crate::manage) fn disclosure(
    canvas: &mut Scene,
    root: Affine,
    x: f32,
    y: f32,
    open: bool,
    color: vello::peniko::Color,
) {
    let (x, y, s) = (f64::from(x), f64::from(y), 8.0_f64);
    let mut path = vello::kurbo::BezPath::new();
    if open {
        path.move_to((x, y + s * 0.25));
        path.line_to((x + s, y + s * 0.25));
        path.line_to((x + s * 0.5, y + s * 0.85));
    } else {
        path.move_to((x + s * 0.2, y));
        path.line_to((x + s * 0.8, y + s * 0.5));
        path.line_to((x + s * 0.2, y + s));
    }
    path.close_path();
    scene::fill(canvas, root, color, &path);
}
