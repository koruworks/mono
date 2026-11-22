// SPDX-License-Identifier: MPL-2.0

//! **The shared pickers on the stage**: the color swatch, the color picker
//! row, the stepper and the preview tile.
//!
//! ```text
//! Component under test
//!   <color>…                          Swatch: a row of four, one chosen
//!   <color>…                          ColorPickerRow: its optional label, then its swatches
//!   less · value · more               Stepper: two buttons round a readout
//!   <theme>…                          PreviewTile: two tiles, one chosen
//! ```
//!
//! The colors are sample content: no color at all, then three of the
//! theme's own, named as the palette spells them. A swatch's rings reach
//! outside its square, so those two stories keep that reach clear inside
//! the slot (`Swatch::RING_REACH`), as a caller must.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::themes;
use koru_draw::{scene, theme};
use koru_i18n::Key;
use koru_ui::color_picker_row::ColorPickerRow;
use koru_ui::preview_tile::PreviewTile;
use koru_ui::stepper::{Step, Stepper};
use koru_ui::swatch::Swatch;
use vello::peniko::Color;

use super::{Canvas, Part, PartRole, Pointed, Widget, chosen_line, text};
use crate::ui_lab::registry::{self, Control};
use crate::ui_lab::story::{Held, WidgetStory};

/// The sample colors, as the palette names them. The first is no color:
/// the "automatic" value, drawn hollow.
const COLORS: [&str; 4] = ["none", "accent", "success", "notice_fg"];

fn colors() -> [Option<Color>; 4] {
    let p = theme::p();
    [None, Some(p.accent), Some(p.success), Some(p.notice_fg)]
}

/// `rect` less a swatch's ring reach on every side.
fn inside_rings(rect: Rect) -> Rect {
    let reach = Swatch::RING_REACH;
    Rect::new(
        rect.x + reach,
        rect.y + reach,
        (rect.w - 2.0 * reach).max(0.0),
        (rect.h - 2.0 * reach).max(0.0),
    )
}

fn swatch_parts(story: &WidgetStory, rects: Vec<Rect>) -> Vec<Part> {
    rects
        .into_iter()
        .enumerate()
        .map(|(at, rect)| {
            Part::button(COLORS[at], rect, COLORS[at])
                .role(PartRole::Radio(story.held.chosen == Some(at)))
        })
        .collect()
}

fn choose(story: &mut WidgetStory, part: usize) -> bool {
    let changed = story.held.chosen != Some(part);
    story.held.chosen = Some(part);
    changed
}

fn first_chosen() -> Held {
    Held {
        chosen: Some(0),
        ..Held::default()
    }
}

// ----------------------------------------------------------------- swatch

/// `koru_ui::swatch::Swatch`: a row of four (`Swatch::row`), one chosen.
pub(super) struct SwatchStory;

impl Widget for SwatchStory {
    fn id(&self) -> &'static str {
        registry::SWATCH_STORY
    }

    fn held(&self) -> Held {
        first_chosen()
    }

    fn size(&self, _: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        let reach = 2.0 * Swatch::RING_REACH;
        (
            Swatch::row_width(COLORS.len()) + reach,
            Swatch::SIZE + reach,
        )
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let inner = inside_rings(rect);
        swatch_parts(story, Swatch::row(inner.x, inner.y, COLORS.len()))
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let inner = inside_rings(rect);
        let rects = Swatch::row(inner.x, inner.y, COLORS.len());
        for (at, (rect, color)) in rects.into_iter().zip(colors()).enumerate() {
            Swatch::draw(
                canvas.scene,
                canvas.root,
                rect,
                color,
                story.held.chosen == Some(at),
                pointed.hovered == Some(at),
                pointed.focused == Some(at),
                1.0,
            );
        }
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        choose(story, part)
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        story.held.chosen.map(|at| chosen_line(COLORS[at]))
    }
}

// ------------------------------------------------------- color picker row

/// `koru_ui::color_picker_row::ColorPickerRow`: the row every place that
/// offers a color draws, with its optional label.
pub(super) struct ColorPickerRowStory;

impl ColorPickerRowStory {
    /// The row as the production builder makes it. An empty label is no
    /// label: the row is then just its swatches.
    fn row<'a>(
        story: &'a WidgetStory,
        colors: &'a [Option<Color>],
        pointed: Pointed,
    ) -> ColorPickerRow<'a> {
        let row = ColorPickerRow::new(colors)
            .selected(story.held.chosen)
            .hovered(pointed.hovered)
            .focused(pointed.focused);
        match story.text(Control::Label) {
            "" => row,
            label => row.label(label),
        }
    }
}

impl Widget for ColorPickerRowStory {
    fn id(&self) -> &'static str {
        registry::COLOR_PICKER_ROW_STORY
    }

    fn sample(&self, control: Control) -> Option<Key> {
        (control == Control::Label).then_some(Key::A11yTaskColorPicker)
    }

    fn held(&self) -> Held {
        first_chosen()
    }

    fn size(&self, story: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        let colors = colors();
        let row = Self::row(story, &colors, Pointed::default());
        let reach = 2.0 * Swatch::RING_REACH;
        (row.width() + reach, row.height() + reach)
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let colors = colors();
        let row = Self::row(story, &colors, Pointed::default());
        swatch_parts(story, row.swatches(inside_rings(rect)))
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let colors = colors();
        Self::row(story, &colors, pointed).draw(
            canvas.scene,
            canvas.root,
            canvas.engine,
            inside_rings(rect),
        );
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        choose(story, part)
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        story.held.chosen.map(|at| chosen_line(COLORS[at]))
    }
}

// ---------------------------------------------------------------- stepper

/// `koru_ui::stepper::Stepper`: a value on a scale of five, nudged by its
/// two buttons, each dimmed at its end of the scale.
pub(super) struct StepperStory;

impl StepperStory {
    /// The scale's highest step: the steps are 1 to this.
    const TOP: usize = 4;

    fn value(level: usize) -> String {
        koru_i18n::number::decimal(koru_i18n::current(), level as u64 + 1)
    }

    fn layout(engine: &mut TextEngine, x: f32, y: f32) -> Stepper {
        Stepper::layout(engine, &Self::value(Self::TOP), x, y)
    }
}

impl Widget for StepperStory {
    fn id(&self) -> &'static str {
        registry::STEPPER_STORY
    }

    /// The middle of the scale, so both buttons do something.
    fn held(&self) -> Held {
        Held {
            level: Self::TOP / 2,
            ..Held::default()
        }
    }

    fn size(&self, _: &WidgetStory, engine: &mut TextEngine, _: f32) -> (f32, f32) {
        (Self::layout(engine, 0.0, 0.0).bounds.w, Stepper::H)
    }

    fn parts(&self, story: &WidgetStory, engine: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let stepper = Self::layout(engine, rect.x, rect.y);
        let level = story.held.level;
        // The stepper draws no keyboard-focus state: the window rings the
        // button the keyboard is on.
        vec![
            Part::button("less", stepper.less, text(Key::UiLabStepLess))
                .disabled(level == 0)
                .ringed(),
            Part::button("value", stepper.readout, text(Key::UiLabStepperName))
                .role(PartRole::Readout)
                .value(Self::value(level)),
            Part::button("more", stepper.more, text(Key::UiLabStepMore))
                .disabled(level == Self::TOP)
                .ringed(),
        ]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let level = story.held.level;
        let hovered = match pointed.hovered {
            Some(0) => Some(Step::Less),
            Some(2) => Some(Step::More),
            _ => None,
        };
        Self::layout(canvas.engine, rect.x, rect.y).draw(
            canvas.scene,
            canvas.root,
            canvas.engine,
            &Self::value(level),
            level > 0,
            level < Self::TOP,
            hovered,
            1.0,
        );
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        let level = story.held.level;
        let next = match part {
            0 => level.saturating_sub(1),
            2 => (level + 1).min(Self::TOP),
            _ => level,
        };
        story.held.level = next;
        next != level
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(koru_i18n::fill(
            text(Key::UiLabResultValue),
            &[&Self::value(story.held.level)],
        ))
    }
}

// ----------------------------------------------------------- preview tile

/// `koru_ui::preview_tile::PreviewTile`: two tiles (`PreviewTile::row`),
/// one chosen. Each previews a built-in theme by filling its picture with
/// that theme's ground, as the appearance settings' tiles do.
pub(super) struct PreviewTileStory;

impl PreviewTileStory {
    /// The most the two tiles take: a settings card's width.
    const WIDTH: f32 = 320.0;
    const TILES: [(&'static str, Key); 2] = [
        ("light", Key::UiLabThemeLight),
        ("dark", Key::UiLabThemeDark),
    ];

    fn ground(at: usize) -> Color {
        let theme = if at == 0 {
            &themes::LIGHT_THEME
        } else {
            &themes::DARK_THEME
        };
        theme.palette(themes::contrast()).ground
    }
}

impl Widget for PreviewTileStory {
    fn id(&self) -> &'static str {
        registry::PREVIEW_TILE_STORY
    }

    fn held(&self) -> Held {
        first_chosen()
    }

    fn size(&self, _: &WidgetStory, _: &mut TextEngine, room: f32) -> (f32, f32) {
        (Self::WIDTH.min(room), PreviewTile::H)
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        PreviewTile::row(rect.x, rect.y, rect.w, Self::TILES.len())
            .into_iter()
            .zip(Self::TILES)
            .enumerate()
            .map(|(at, (rect, (part, name)))| {
                // The tile lights its border under the pointer and has no
                // keyboard-focus state: the window rings it.
                Part::button(part, rect, text(name))
                    .role(PartRole::Radio(story.held.chosen == Some(at)))
                    .ringed()
            })
            .collect()
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let rects = PreviewTile::row(rect.x, rect.y, rect.w, Self::TILES.len());
        for (at, (rect, (_, name))) in rects.into_iter().zip(Self::TILES).enumerate() {
            PreviewTile::new(text(name), story.held.chosen == Some(at)).draw(
                canvas.scene,
                canvas.root,
                canvas.engine,
                rect,
                pointed.hovered == Some(at),
                1.0,
                |canvas, root, area| {
                    scene::fill(
                        canvas,
                        root,
                        Self::ground(at),
                        &scene::rect(area.x, area.y, area.w, area.h),
                    );
                },
            );
        }
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        choose(story, part)
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        story
            .held
            .chosen
            .map(|at| chosen_line(text(Self::TILES[at].1)))
    }
}
