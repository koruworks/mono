// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeMap, sync::Arc};

use koru_draw::{scene::fill, text::TextEngine, theme};
use koru_session_observability::{Counter, SessionStats};
use koru_session_protocol::{
    ErrorCode, GridSize, MouseButton, RenderUpdate, RowIndex, RowRange, ScrollbackPage, TerminalRow,
};
use serde::Serialize;
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};

use crate::row::{CachedRow, DeviceGrid, build_row, draw_row_backgrounds, draw_terminal_shapes};
use crate::selection::selected_text;
use crate::{
    ApplyOutcome, CellSelection, GridGeometry, SessionSurfaceIntent, SessionSurfaceInteraction,
    StyleEpoch, TerminalAppearance, TerminalCursorPreferences, TerminalFace, TerminalIntent,
    TerminalModel, TerminalStyle, terminal_background,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderWork {
    pub rows_considered: u64,
    pub rows_shaped: u64,
    pub cache_hits: u64,
    pub style_runs: u64,
    pub rows_appended_to_scene: u64,
    pub cache_invalidations: u64,
    pub private_use_cells: u64,
    pub replacement_cells: u64,
    pub box_drawing_cells: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "layer", rename_all = "snake_case")]
pub enum PaintRecord {
    Background {
        row: u64,
        first_column: u16,
        columns: u16,
        color: [u8; 4],
    },
    Text {
        row: u64,
        first_column: u16,
        columns: u16,
        text: String,
        face: TerminalFace,
        foreground: [u8; 4],
    },
    Selection {
        row: u64,
        first_column: u16,
        columns: u16,
    },
    Cursor {
        row: u64,
        column: u16,
        shape: String,
    },
}

#[derive(Default)]
struct Renderer {
    style: Option<TerminalStyle>,
    rows: BTreeMap<RowIndex, CachedRow>,
}

/// Protocol-driven reusable terminal view. The host supplies the one shared
/// `TextEngine` when drawing and owns identity-bearing command delivery.
pub struct TerminalView<I: SessionSurfaceInteraction> {
    pub(crate) interaction: I,
    pub(crate) model: Option<TerminalModel>,
    renderer: Renderer,
    pub(crate) visible_rows: Option<RowRange>,
    pub(crate) tail_rows: Option<RowRange>,
    pub(crate) selection: Option<CellSelection>,
    pub(crate) history: crate::history::PageCache,
    pub(crate) follow_tail: bool,
    pub(crate) overscan_rows: u16,
    pub(crate) preedit: Option<String>,
    pub(crate) pressed_button: Option<MouseButton>,
    pub(crate) selecting: bool,
    pub(crate) last_resize: Option<(GridSize, RowRange, u16)>,
    visible: bool,
    cursor_preferences: TerminalCursorPreferences,
    appearance: Option<TerminalAppearance>,
    cursor_visible: bool,
    stats: Arc<SessionStats>,
    last_records: Vec<PaintRecord>,
}

impl<I: SessionSurfaceInteraction> TerminalView<I> {
    pub fn new(interaction: I) -> Self {
        Self::with_stats(interaction, Arc::new(SessionStats::new(false)))
    }

    pub fn with_stats(interaction: I, stats: Arc<SessionStats>) -> Self {
        Self {
            interaction,
            model: None,
            renderer: Renderer::default(),
            visible_rows: None,
            tail_rows: None,
            selection: None,
            history: crate::history::PageCache::default(),
            follow_tail: true,
            overscan_rows: 2,
            preedit: None,
            pressed_button: None,
            selecting: false,
            last_resize: None,
            visible: true,
            cursor_preferences: TerminalCursorPreferences::default(),
            appearance: None,
            cursor_visible: true,
            stats,
            last_records: Vec::new(),
        }
    }

    pub fn interaction_mut(&mut self) -> &mut I {
        &mut self.interaction
    }
    pub fn model(&self) -> Option<&TerminalModel> {
        self.model.as_ref()
    }
    pub fn paint_records(&self) -> &[PaintRecord] {
        &self.last_records
    }
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }
    pub fn set_cursor_preferences(&mut self, preferences: TerminalCursorPreferences) {
        self.cursor_preferences = preferences;
    }
    pub fn cursor_preferences(&self) -> TerminalCursorPreferences {
        self.cursor_preferences
    }

    pub fn set_appearance(&mut self, appearance: TerminalAppearance) {
        self.appearance = Some(appearance);
    }

    pub fn background(&self) -> peniko::Color {
        self.appearance.map_or_else(terminal_background, |a| {
            theme::terminal_palette_for(a.dark).background
        })
    }
    pub fn set_cursor_visible(&mut self, visible: bool) -> bool {
        let changed = self.cursor_visible != visible;
        self.cursor_visible = visible;
        changed
    }
    pub fn cursor_blinks(&self) -> bool {
        self.model.as_ref().is_some_and(|model| {
            model.cursor.shape != koru_session_protocol::CursorShape::Hidden
                && self.cursor_preferences.blink.enabled(model.cursor.blinking)
        })
    }
    /// Measure fixed-grid metrics even before a session model exists.
    ///
    /// Shells preserve primary-screen output across resize, so creating their
    /// PTY with provisional cells and correcting it after the first prompt is
    /// visible as a startup jump. Priming once during the window's first paint
    /// lets creation use the final font/density metrics instead.
    pub fn prepare_style(&mut self, engine: &mut TextEngine, scale: f32) -> bool {
        let epoch = self
            .appearance
            .map_or_else(|| StyleEpoch::current(scale), |a| a.epoch(scale));
        if self
            .renderer
            .style
            .is_some_and(|style| style.epoch == epoch)
        {
            return false;
        }
        self.renderer.rows.clear();
        self.renderer.style = Some(TerminalStyle::capture_epoch(engine, epoch));
        true
    }

    /// Refresh fixed-grid metrics after a discrete font change. This
    /// keeps the following PTY resize in the same input event instead of
    /// waiting for a paint to discover the new cell size.
    pub fn refresh_style(&mut self, engine: &mut TextEngine, scale: f32) {
        let _ = self.prepare_style(engine, scale);
    }

    /// The measured terminal cell, available before the first session exists.
    pub fn cell_size(&self) -> Option<(f32, f32)> {
        self.renderer
            .style
            .map(|style| (style.cell_width, style.cell_height))
    }
    pub fn set_visible_rows(&mut self, rows: RowRange) {
        self.visible_rows = Some(rows);
        if self.follow_tail {
            self.tail_rows = Some(rows);
        }
    }
    /// Choose the viewport range for a changed terminal height. Tail-following
    /// surfaces keep the same bottom row; scrollback keeps its explicit top.
    /// This prevents a taller window from requesting rows beyond retention
    /// while the PTY resize is still being applied.
    pub fn rows_for_resize(&self, count: u16) -> Option<RowRange> {
        let model = self.model.as_ref()?;
        let current = self
            .visible_rows
            .or(self.tail_rows)
            .unwrap_or(model.viewport);
        let tail = self.tail_rows.unwrap_or(current);
        Some(resized_rows(current, tail, self.follow_tail, count))
    }
    pub fn set_selection(&mut self, selection: Option<CellSelection>) {
        self.selection = selection;
    }

    /// Forget the active surface while a host switches identity or detaches.
    /// Process lifetime is outside this view; retaining rows here would show a
    /// stale session under the next selection (and could make a lease conflict
    /// look interactive).
    pub fn reset(&mut self) {
        self.model = None;
        self.renderer = Renderer::default();
        self.visible_rows = None;
        self.tail_rows = None;
        self.selection = None;
        self.history.clear();
        self.follow_tail = true;
        self.preedit = None;
        self.pressed_button = None;
        self.selecting = false;
        self.last_resize = None;
        self.last_records.clear();
    }

    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let model = self.model.as_ref()?;
        selected_text(selection, model.size.columns, |row| self.source_row(row))
    }

    pub fn apply_scrollback_page(&mut self, page: ScrollbackPage) -> Result<(), ErrorCode> {
        let model = self.model.as_ref().ok_or(ErrorCode::InvalidRequest)?;
        self.history
            .insert(page, model.session_id, model.terminal_generation)
    }

    /// Positive lines move toward older history; negative lines move to tail.
    pub fn scroll_lines(&mut self, lines: i32) -> bool {
        if lines == 0 {
            return false;
        }
        let Some(model) = self.model.as_ref() else {
            return false;
        };
        let current = self
            .visible_rows
            .or(self.tail_rows)
            .unwrap_or(model.viewport);
        let tail = self.tail_rows.unwrap_or(current);
        let next_first = if lines > 0 {
            current.first.0.saturating_sub(lines as u64)
        } else {
            current
                .first
                .0
                .saturating_add(u64::from(lines.unsigned_abs()))
                .min(tail.first.0)
        };
        if next_first == current.first.0 {
            return false;
        }
        if next_first == tail.first.0 {
            self.return_to_bottom();
            return true;
        }
        let range = RowRange {
            first: RowIndex(next_first),
            count: current.count,
        };
        self.follow_tail = false;
        self.visible_rows = Some(range);
        let end = range.first.0 + u64::from(range.count);
        let first_missing =
            (range.first.0..end).find(|index| self.source_row(RowIndex(*index)).is_none());
        let last_missing = (range.first.0..end)
            .rev()
            .find(|index| self.source_row(RowIndex(*index)).is_none());
        if let (Some(first), Some(last)) = (first_missing, last_missing) {
            let missing = RowRange {
                first: RowIndex(first),
                count: (last - first + 1) as u16,
            };
            if self
                .history
                .request_missing(model.terminal_generation, missing)
            {
                self.interaction.dispatch(SessionSurfaceIntent::Terminal(
                    TerminalIntent::RequestScrollback { range: missing },
                ));
            }
        }
        true
    }

    pub fn return_to_bottom(&mut self) {
        let Some(rows) = self.tail_rows else {
            return;
        };
        self.history.clear();
        self.follow_tail = true;
        self.visible_rows = Some(rows);
        if let Some(model) = &self.model {
            self.interaction.dispatch(SessionSurfaceIntent::Terminal(
                TerminalIntent::SetViewport {
                    rows,
                    columns: model.size.columns,
                    overscan_rows: self.overscan_rows,
                },
            ));
        }
    }

    pub fn apply(&mut self, update: RenderUpdate) -> Result<ApplyOutcome, ErrorCode> {
        let had_model = self.model.is_some();
        let outcome = match (&mut self.model, update) {
            (None, RenderUpdate::Snapshot { snapshot }) => {
                self.model = Some(TerminalModel::from_snapshot(snapshot)?);
                ApplyOutcome::Snapshot {
                    generation_changed: true,
                }
            }
            (None, RenderUpdate::Gap { .. }) => ApplyOutcome::RecoveryRequested,
            (None, RenderUpdate::Delta { .. }) => {
                self.interaction.dispatch(SessionSurfaceIntent::Terminal(
                    TerminalIntent::RecoverSnapshot,
                ));
                return Err(ErrorCode::StreamGap);
            }
            (Some(model), update) => match model.apply(update) {
                Ok(outcome) => outcome,
                Err(ErrorCode::StreamGap) => {
                    self.interaction.dispatch(SessionSurfaceIntent::Terminal(
                        TerminalIntent::RecoverSnapshot,
                    ));
                    return Err(ErrorCode::StreamGap);
                }
                Err(error) => return Err(error),
            },
        };
        if matches!(outcome, ApplyOutcome::RecoveryRequested) {
            self.interaction.dispatch(SessionSurfaceIntent::Terminal(
                TerminalIntent::RecoverSnapshot,
            ));
        }
        let snapshot_applied = matches!(outcome, ApplyOutcome::Snapshot { .. });
        let generation_changed = matches!(
            outcome,
            ApplyOutcome::Snapshot {
                generation_changed: true
            }
        );
        if generation_changed {
            self.renderer.rows.clear();
            self.history.clear();
            self.follow_tail = true;
            // An explicit pre-snapshot viewport belongs to this first surface
            // and must survive. A viewport retained from a different session
            // belongs to neither its row space nor its terminal generation.
            if had_model {
                self.visible_rows = None;
                self.tail_rows = None;
            }
        }
        if let Some(model) = &self.model
            && (self.tail_rows.is_none() || (had_model && snapshot_applied && self.follow_tail))
        {
            let count = model.size.rows.min(model.viewport.count);
            let tail = RowRange {
                first: RowIndex(model.viewport.first.0 + u64::from(model.viewport.count - count)),
                count,
            };
            self.tail_rows = Some(tail);
            if self.follow_tail {
                self.visible_rows = Some(tail);
                // A snapshot is the Host's acknowledgement of the actual PTY
                // grid and visible tail. Recording it prevents the next paint
                // from issuing the same resize again with pre-repaint rows.
                self.last_resize = Some((model.size, tail, self.overscan_rows));
            }
        }
        Ok(outcome)
    }

    /// Fill the cursor where the model says it is, when it is visible.
    #[allow(clippy::too_many_arguments)]
    fn paint_cursor(
        visible: bool,
        preferences: TerminalCursorPreferences,
        records: &mut Vec<PaintRecord>,
        scene: &mut Scene,
        root: Affine,
        geometry: &GridGeometry,
        model: &TerminalModel,
        style: TerminalStyle,
    ) {
        if !visible {
            return;
        }
        let Some(cursor) = geometry.cursor_rect_with(
            crate::CellPoint {
                row: model.cursor.row,
                column: model.cursor.column,
            },
            model.cursor.shape,
            preferences.width,
            preferences.height,
        ) else {
            return;
        };
        let color = match preferences.color {
            theme::CaretColor::Default => style.palette().cursor,
            color => color.color_for(style.epoch.dark),
        };
        fill(scene, root, color, &cursor);
        records.push(PaintRecord::Cursor {
            row: model.cursor.row.0,
            column: model.cursor.column.0,
            shape: format!("{:?}", model.cursor.shape).to_ascii_lowercase(),
        });
    }

    pub fn geometry(&self) -> Option<GridGeometry> {
        let model = self.model.as_ref()?;
        let style = self.renderer.style?;
        let rows = self.visible_rows.unwrap_or(model.viewport);
        Some(GridGeometry::new(
            Rect::new(
                0.0,
                0.0,
                f64::from(model.size.columns) * f64::from(style.cell_width),
                f64::from(rows.count) * f64::from(style.cell_height),
            ),
            rows,
            model.size.columns,
            style.cell_width,
            style.cell_height,
        ))
    }

    pub fn draw(
        &mut self,
        engine: &mut TextEngine,
        scene: &mut Scene,
        root: Affine,
        bounds: Rect,
        scale: f32,
    ) -> RenderWork {
        self.last_records.clear();
        if !self.visible {
            return RenderWork::default();
        }
        let mut work = RenderWork::default();
        if self.prepare_style(engine, scale) {
            work.cache_invalidations = 1;
        }
        let Some(model) = self.model.as_ref() else {
            return work;
        };
        let style = self.renderer.style.expect("captured above");
        let epoch = style.epoch;
        let rows = self.visible_rows.unwrap_or(model.viewport);
        let geometry = GridGeometry::new(
            bounds,
            rows,
            model.size.columns,
            style.cell_width,
            style.cell_height,
        );
        // **The grid's ground, painted once.** Every cell whose background is
        // the default shows this rect rather than one of its own, so the
        // commonest boundary in a terminal — one run of ordinary text meeting
        // the next — has no edge to tear along at any scale.
        fill(scene, root, style.palette().background, &bounds);
        let grid = DeviceGrid::new(root, scale);
        work.rows_considered = u64::from(rows.count);

        for offset in 0..rows.count {
            let index = RowIndex(rows.first.0 + u64::from(offset));
            let source = self
                .history
                .row(model.terminal_generation, index)
                .or_else(|| model.rows.get(&index))
                .cloned()
                .unwrap_or(TerminalRow {
                    index,
                    cells: Vec::new(),
                    wrapped_to_next: false,
                });
            let hit = self
                .renderer
                .rows
                .get(&index)
                .is_some_and(|cached| cached.epoch == epoch && cached.source == source);
            if hit {
                work.cache_hits += 1;
            } else {
                let cached = build_row(engine, source, style, model.size.columns);
                work.rows_shaped += 1;
                work.style_runs += cached.style_runs;
                self.renderer.rows.insert(index, cached);
            }
        }
        let end = rows.first.0 + u64::from(rows.count);
        self.renderer
            .rows
            .retain(|index, _| index.0 >= rows.first.0 && index.0 < end);

        // Complete frames paint every visible row background, then overlays,
        // then every visible row's cached text. Only cache misses shaped above.
        //
        // The backgrounds are filled here rather than appended as a cached
        // fragment because their edges have to land on device pixels, and that
        // is only knowable once the row's real origin and the frame's real
        // transform are: two fills that meet mid-pixel each antialias against
        // what is under them and leave a seam of it showing.
        for offset in 0..rows.count {
            let index = RowIndex(rows.first.0 + u64::from(offset));
            let cached = &self.renderer.rows[&index];
            let row_y = bounds.y0 + f64::from(offset) * f64::from(style.cell_height);
            draw_row_backgrounds(scene, root, grid, cached, style, bounds.x0, row_y);
            self.last_records.extend(
                cached
                    .records
                    .iter()
                    .filter(|record| matches!(record, PaintRecord::Background { .. }))
                    .cloned(),
            );
        }
        if let Some(selection) = self.selection {
            for selection_rect in geometry.selection_rects(selection) {
                fill(scene, root, style.palette().selection, &selection_rect);
                let row = rows.first.0
                    + ((selection_rect.y0 - bounds.y0) / f64::from(style.cell_height)) as u64;
                let first_column =
                    ((selection_rect.x0 - bounds.x0) / f64::from(style.cell_width)) as u16;
                let columns = (selection_rect.width() / f64::from(style.cell_width)).round() as u16;
                self.last_records.push(PaintRecord::Selection {
                    row,
                    first_column,
                    columns,
                });
            }
        }
        // **A block cursor is a ground, not a lid.** Painted over the text it
        // hid the letter under it (a prompt's first character vanished into a
        // purple square); a terminal shows that letter inside the block. So the
        // block goes under the text, and only the thin shapes go over it.
        let block_under_text = model.cursor.shape == koru_session_protocol::CursorShape::Block;
        if block_under_text {
            Self::paint_cursor(
                self.cursor_visible,
                self.cursor_preferences,
                &mut self.last_records,
                scene,
                root,
                &geometry,
                model,
                style,
            );
        }
        for offset in 0..rows.count {
            let index = RowIndex(rows.first.0 + u64::from(offset));
            let cached = &self.renderer.rows[&index];
            let row_y = bounds.y0 + f64::from(offset) * f64::from(style.cell_height);
            let transform = root * Affine::translate((bounds.x0, row_y));
            scene.append(&cached.text, Some(transform));
            draw_terminal_shapes(scene, root, grid, cached, style, bounds.x0, row_y);
            work.private_use_cells += cached.private_use_cells;
            work.replacement_cells += cached.replacement_cells;
            work.box_drawing_cells += cached.box_drawing_cells;
            self.last_records.extend(
                cached
                    .records
                    .iter()
                    .filter(|record| matches!(record, PaintRecord::Text { .. }))
                    .cloned(),
            );
            work.rows_appended_to_scene += 1;
        }
        // Beam and underline sit beside the glyphs, so they go on top; a block
        // went under them above, so the letter it covers is still drawn.
        if !block_under_text {
            Self::paint_cursor(
                self.cursor_visible,
                self.cursor_preferences,
                &mut self.last_records,
                scene,
                root,
                &geometry,
                model,
                style,
            );
        }

        self.stats.add(Counter::RowsShaped, work.rows_shaped);
        self.stats.add(Counter::CachedRowHits, work.cache_hits);
        self.stats
            .add(Counter::RowsAppendedToScene, work.rows_appended_to_scene);
        self.stats.add(Counter::RenderRuns, work.style_runs);
        work
    }
}

fn resized_rows(current: RowRange, tail: RowRange, follow_tail: bool, count: u16) -> RowRange {
    if !follow_tail {
        return RowRange {
            first: current.first,
            count,
        };
    }
    let end = tail.first.0 + u64::from(tail.count);
    RowRange {
        first: RowIndex(end.saturating_sub(u64::from(count))),
        count,
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    #[test]
    fn resizing_preserves_tail_or_scrollback_anchor() {
        let current = RowRange {
            first: RowIndex(100),
            count: 20,
        };
        assert_eq!(
            resized_rows(current, current, true, 40),
            RowRange {
                first: RowIndex(80),
                count: 40,
            }
        );
        assert_eq!(
            resized_rows(current, current, false, 40),
            RowRange {
                first: RowIndex(100),
                count: 40,
            }
        );
    }
}
