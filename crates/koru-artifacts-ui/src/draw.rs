// SPDX-License-Identifier: MPL-2.0

//! What the column puts on screen.
//!
//! Every rect comes from [`ArtifactsSurface::rows`] and
//! [`ArtifactsSurface::row_parts`] — the same two functions the hit test
//! uses — so a row cannot be drawn anywhere other than where it is
//! clickable. Nothing here measures anything the geometry does not already
//! know: a label that will not fit is **elided, not wrapped**, because a
//! result called `2026-Q3-consolidated-findings.md` has no word boundary in
//! it and a wrapped line would run past the column with nothing to say it
//! had been cut.
//!
//! **Two sections, visibly different** (D19). Results are ordinary nav rows
//! in the library's ink — dim at rest, full-strength on the card. What is not
//! a result yet is dim and stays dim (pressing one never selects it), with
//! its own heading, and carries the one warm thing on the column: *Register*,
//! in the accent that means *an offer* rather than *a statement*.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_i18n::strings::{Key, t};
use koru_shell_chrome::tabs::draw_icon;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use crate::rows::{Notice, Row};
use crate::{ArtifactsSurface, model::Registering};

/// The icon the navigator's mode row draws for this column, and the glyph a
/// run's heading wears. Named here so the container and the column cannot
/// disagree about which picture means *artifacts*.
pub const MODE_ICON: char = icons::TRAY;

impl ArtifactsSurface {
    /// The whole column below the heading.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        top: f32,
    ) {
        self.work.layouts = 0;
        self.clamp_scroll(area, top);
        let p = theme::p();
        let bottom = area.y + area.h;
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(area.x, top, area.w, (bottom - top).max(0.0)),
        );
        let rows = self.rows(area, top);
        for (row, rect) in rows {
            self.draw_row(scene, root, engine, area, row, rect, p);
        }
        scene.pop_layer();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        row: Row,
        rect: Rect,
        p: &theme::Palette,
    ) {
        let parts = self.row_parts(row, rect);
        let lang = self.lang;
        let treatment = self.row_treatment(row);
        // One row, one named node — the same ExploreRow the library and the
        // rail answer to, in the same treatment vocabulary.
        let _row_scope = koru_draw::inspect::scope(
            "ExploreRow",
            "container",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |record| {
                record.property("treatment", treatment, "row state");
            },
        );
        // The treatments are the library's, in the library's order: the
        // active row's card first — the fill that gives the open look — then
        // the hover's tint, then the ring that says *here, not yet chosen*
        // in the same border grey every other explore row wears. A ring on
        // a card would read as two things open, so the card wins.
        if treatment == "Card" {
            let chip = chip(rect, area);
            scene::fill(scene, root, p.panel_bg, &chip);
            scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &chip);
        } else if row.pressable() && self.hover == Some(row) {
            scene::fill(scene, root, p.hover_tint, &chip(rect, area));
        }
        if treatment != "Card" && row.pressable() && self.focus == Some(row) {
            scene::stroke(
                scene,
                root,
                p.panel_border,
                theme::BORDER_W,
                &chip(rect, area),
            );
        }
        match row {
            Row::Notice(notice) => {
                let key = match notice {
                    Notice::Loading => Key::ArtifactsLoading,
                    Notice::Unavailable => Key::ArtifactsUnavailable,
                    Notice::Empty => Key::ArtifactsEmpty,
                };
                let text = t(lang, key);
                let layout = engine.simple_layout_wrapped(
                    text,
                    theme::role::meta(),
                    (area.w - theme::NAV_PAD * 2.0).max(1.0),
                );
                self.work.layouts += 1;
                scene::draw_layout(
                    scene,
                    root,
                    &layout,
                    area.x + theme::NAV_PAD,
                    rect.y,
                    Some(p.text_dim),
                );
            }
            Row::ResultsHeading => self.heading(
                scene,
                root,
                engine,
                parts.label,
                t(lang, Key::ArtifactsResults),
            ),
            Row::UnregisteredHeading => self.heading(
                scene,
                root,
                engine,
                parts.label,
                t(lang, Key::ArtifactsNotYetResults),
            ),
            Row::Family(index) => {
                let Some(entry) = self.catalog.families.get(index) else {
                    return;
                };
                let Some(latest) = entry.latest() else {
                    return;
                };
                let expanded = self.expanded.contains(&entry.family.id);
                let title = crate::text::artifact_row(lang, entry.family.title.as_str(), latest);
                let size = crate::text::size(lang, latest.size_bytes);
                let elsewhere = !self.catalog.holds(latest);
                let ink = row_ink(p, elsewhere, treatment);
                draw_caret(scene, root, engine, parts.caret, expanded, p.text_dim);
                draw_icon(scene, root, engine, icons::FILE, parts.icon, p.text_dim);
                self.label(scene, root, engine, parts.label, &title, ink);
                self.trailing(scene, root, engine, parts.trailing, &size, p.text_dim);
            }
            Row::Version { family, version } => {
                let Some(found) = self
                    .catalog
                    .families
                    .get(family)
                    .and_then(|entry| entry.version(version))
                else {
                    return;
                };
                // **D27 is visible before it is read.** A version this
                // device cannot reach is drawn in the dim ink every
                // unreachable thing in this window wears.
                let elsewhere = !self.catalog.holds(found);
                let ink = row_ink(p, elsewhere, treatment);
                let label = crate::text::version_row(lang, found, self.now_ms);
                let size = crate::text::size(lang, found.size_bytes);
                draw_icon(scene, root, engine, icons::FILE, parts.icon, p.text_dim);
                self.label(scene, root, engine, parts.label, &label, ink);
                self.trailing(scene, root, engine, parts.trailing, &size, p.text_dim);
            }
            Row::Provenance {
                family,
                version,
                clause,
            } => {
                let Some(text) = self.clause(family, version, clause) else {
                    return;
                };
                self.note(scene, root, engine, parts.label, &text, p.text_dim);
            }
            Row::Earlier(index) => {
                let Some(entry) = self.catalog.families.get(index) else {
                    return;
                };
                let hidden = entry
                    .versions
                    .len()
                    .saturating_sub(self.shown(entry.family.id));
                let text = crate::text::earlier_versions(lang, hidden);
                self.label(scene, root, engine, parts.label, &text, p.accent);
            }
            Row::Run(index) => {
                let Some(entry) = self.catalog.runs.get(index) else {
                    return;
                };
                let text = crate::text::run_heading(lang, entry.run);
                draw_icon(scene, root, engine, icons::ROBOT, parts.icon, p.text_dim);
                self.label(scene, root, engine, parts.label, &text, p.text_dim);
            }
            Row::RunNote { run, note } => {
                let Some(entry) = self.catalog.runs.get(run) else {
                    return;
                };
                let Some(text) = crate::text::run_notes(lang, entry).into_iter().nth(note) else {
                    return;
                };
                self.note(scene, root, engine, parts.label, &text, p.text_dim);
            }
            Row::File { run, file } => {
                let Some(entry) = self.catalog.runs.get(run) else {
                    return;
                };
                let Some(found) = entry.files.get(file) else {
                    return;
                };
                let name = crate::text::file_label(found).to_owned();
                let state = self
                    .registering
                    .get(&(entry.run, found.path.clone()))
                    .cloned();
                draw_icon(scene, root, engine, icons::FILE, parts.icon, p.text_dim);
                self.label(scene, root, engine, parts.label, &name, p.text_dim);
                // The offer, in the one colour that means *an offer*.
                let (action, ink) = match state {
                    Some(Registering::Sending) => (t(lang, Key::ArtifactsRegistering), p.text_dim),
                    _ => (t(lang, Key::ArtifactsRegister), p.accent),
                };
                self.trailing(scene, root, engine, parts.trailing, action, ink);
            }
            Row::Open { .. } => self.note(
                scene,
                root,
                engine,
                parts.label,
                t(lang, Key::ArtifactsOpen),
                p.accent,
            ),
            Row::FileRefusal { run, file } => {
                let Some(code) = self
                    .catalog
                    .runs
                    .get(run)
                    .and_then(|entry| {
                        entry
                            .files
                            .get(file)
                            .map(|found| (entry.run, found.path.clone()))
                    })
                    .and_then(|key| self.registering.get(&key))
                    .and_then(|state| match state {
                        Registering::Refused(code) => Some(code.clone()),
                        Registering::Sending => None,
                    })
                else {
                    return;
                };
                let text = crate::text::refusal(lang, &code);
                self.note(scene, root, engine, parts.label, text, warm(p));
            }
        }
    }

    /// What row is wearing, in the library's vocabulary: the selected
    /// version's card (or its collapsed family's), the pointer's hover, a
    /// heading's furniture, or nothing.
    fn row_treatment(&self, row: Row) -> &'static str {
        let active = match row {
            Row::Family(index) => self
                .catalog
                .families
                .get(index)
                .and_then(|entry| entry.latest())
                .is_some_and(|latest| self.selected == Some(latest.id)),
            Row::Version { family, version } => self
                .catalog
                .families
                .get(family)
                .and_then(|entry| entry.version(version))
                .is_some_and(|found| self.selected == Some(found.id)),
            _ => false,
        };
        if active {
            "Card"
        } else if row.pressable() && self.hover == Some(row) {
            "Hover"
        } else if matches!(
            row,
            Row::ResultsHeading | Row::UnregisteredHeading | Row::Run(_)
        ) {
            "Heading"
        } else {
            "Idle"
        }
    }

    /// One clause of the selected version's provenance sentence.
    fn clause(&self, family: usize, version: usize, clause: usize) -> Option<String> {
        let found = self
            .catalog
            .families
            .get(family)
            .and_then(|entry| entry.version(version))?;
        crate::text::provenance_clauses(
            self.lang,
            found,
            self.provenances.get(found.id),
            self.holder(found),
            self.now_ms,
        )
        .into_iter()
        .nth(clause)
    }

    fn heading(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        text: &str,
    ) {
        let layout = engine.simple_layout(text, theme::role::section_heading());
        self.work.layouts += 1;
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            rect.x,
            rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
            Some(theme::p().text_dim),
        );
    }

    fn label(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        text: &str,
        ink: Color,
    ) {
        // **Elided, not wrapped**: measured, because `WWW` and `iii` are
        // not the same width.
        let text = engine.elided(text, theme::role::row(), rect.w);
        let layout = engine.simple_layout(&text, theme::role::row());
        self.work.layouts += 1;
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            rect.x,
            rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
            Some(ink),
        );
    }

    /// A sentence line — a provenance clause, a run's note, a refusal.
    fn note(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        text: &str,
        ink: Color,
    ) {
        let text = engine.elided(text, theme::role::meta(), rect.w);
        let layout = engine.simple_layout(&text, theme::role::meta());
        self.work.layouts += 1;
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            rect.x,
            rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
            Some(ink),
        );
    }

    /// The trailing lane, right-aligned so a number and a word cannot
    /// overlap however wide either becomes.
    fn trailing(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        text: &str,
        ink: Color,
    ) {
        if rect.w <= 0.0 {
            return;
        }
        let text = engine.elided(text, theme::role::meta(), rect.w);
        let layout = engine.simple_layout(&text, theme::role::meta());
        self.work.layouts += 1;
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            rect.x + rect.w - layout.width(),
            rect.y + (rect.h - scene::first_line_height(&layout)) * 0.5,
            Some(ink),
        );
    }
}

/// The shape a hover tint and a focus ring are drawn on — inset from the
/// column's edges the way every other nav row in this window is.
fn chip(rect: Rect, area: Rect) -> vello::kurbo::RoundedRect {
    scene::rrect(
        area.x + 4.0,
        rect.y + 1.0,
        (area.w - 8.0).max(0.0),
        (rect.h - 2.0).max(0.0),
        6.0,
    )
}

/// The disclosure caret — `CARET_RIGHT` turned a quarter turn rather than
/// swapped for `CARET_DOWN`, because an `Affine` can be partway round and a
/// glyph swap can only snap (`DESIGN.md`).
fn draw_caret(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    rect: Rect,
    open: bool,
    ink: Color,
) {
    if rect.w <= 0.0 {
        return;
    }
    let root = if open {
        let (cx, cy) = rect.centre();
        root * Affine::translate((cx as f64, cy as f64))
            * Affine::rotate(std::f64::consts::FRAC_PI_2)
            * Affine::translate((-cx as f64, -cy as f64))
    } else {
        root
    };
    draw_icon(scene, root, engine, icons::CARET_RIGHT, rect, ink);
}

/// A result row's label ink — **the library's rule, one column over**: dim at
/// rest, full-strength only while the row wears the card, so the column does
/// not tell the same story in two greys. A version whose content is not on
/// this device stays dim either way, the same way a remote file keeps the
/// placeholder ink on the Files rows — a row that looks ordinary and does
/// nothing is a coin toss (D27).
fn row_ink(p: &theme::Palette, elsewhere: bool, treatment: &str) -> Color {
    if !elsewhere && treatment == "Card" {
        p.text
    } else {
        p.text_dim
    }
}

/// The one warm tone this column uses, for a refusal. The accent set's own
/// red, never a new `Palette` field.
fn warm(p: &theme::Palette) -> Color {
    theme::accent_tone("red")
        .map(|tone| tone.ink)
        .unwrap_or(p.text)
}
