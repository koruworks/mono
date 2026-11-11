// SPDX-License-Identifier: MPL-2.0

//! **The picker's leaves** (D64; QUESTIONS M7, approved by the user
//! 2026-11-03): a provider's mark, the search field, ↻, the effort strip's
//! segments, the pill toggles and the readout (D76) are each a type whose
//! `impl Leaf` draws its control in the allocation the frame gives it and
//! in the frame's palette, never the global one. This file and the tree are
//! on `ui_scan`'s migrated list.

use koru_draw::brand::ProfileMark;
use koru_draw::el::{ElState, FrameCx, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{FontWeight, IconSize, Ink, Line, Surface, roles};
use koru_draw::{icons, scene};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use vello::Scene;
use vello::kurbo::Affine;

use super::SharedInput;
use super::elements::{
    self, PILL_HEIGHT, PILL_ICON_GAP, PILL_ON_FILL_ALPHA, PILL_PAD, SEGMENT_RADIUS,
};
use super::mark::draw_mark;

/// **A provider's mark, as a leaf** (M1): the mark the host chose for the
/// entry ([`draw_mark`]) — on the rail inside its mark's square, and at a
/// row's start for a favorite or a search's heading — in an ink role.
pub struct ProviderMark {
    pub mark: ProfileMark,
    pub ink: Ink,
    /// How far inside its allocation the mark is drawn.
    pub inset: f32,
}

impl Leaf for ProviderMark {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let inner = Rect::new(
            at.x + self.inset,
            at.y + self.inset,
            (at.w - self.inset * 2.0).max(0.0),
            (at.h - self.inset * 2.0).max(0.0),
        );
        draw_mark(
            scene,
            root,
            engine,
            self.mark,
            inner,
            self.ink.of(cx.palette()),
        );
    }
}

/// The search field as it is drawn: its input, shared with the picker
/// (`SharedInput`, QUESTIONS M6), its placeholder and its focus.
pub struct SearchPaint {
    pub input: SharedInput,
    pub placeholder: String,
    pub focused: bool,
}

/// **The search field** (M7): the picker's `TextInput`, drawn in the
/// allocation the frame gives it; it fills the search box short of ↻.
pub struct SearchLeaf(pub Option<SearchPaint>);

impl Leaf for SearchLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(search) = &self.0 {
            search
                .input
                .draw(scene, root, engine, at, &search.placeholder, search.focused);
        }
    }
}

/// **↻** (M7): the shared `IconButton`, lit under the pointer; `disabled`
/// while the agent's list is being rebuilt. `None` keeps its room with
/// nothing drawn — a built-in provider has no ↻.
pub struct RefreshLeaf {
    pub disabled: Option<bool>,
}

impl Leaf for RefreshLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (elements::REFRESH_W, elements::REFRESH_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        if let Some(disabled) = self.disabled {
            IconButton::new(icons::ARROWS_CLOCKWISE, at)
                .with_state(IconButtonState {
                    hovered: state.hovered,
                    active: false,
                    disabled,
                })
                .draw(scene, root, engine);
        }
    }
}

/// **A level's segment in the effort strip** (D76, M7): its short word,
/// centred; the chat's level raised — the popover's own surface on a soft
/// shadow, its word set SemiBold in the primary ink — and the others in
/// the secondary ink, lit under the pointer. Read only, every word is
/// secondary. The keyboard's segment wears the accent ring.
pub struct SegmentLeaf {
    pub label: String,
    pub selected: bool,
    pub locked: bool,
}

impl Leaf for SegmentLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, super::SEGMENT_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let palette = cx.palette();
        let radius = SEGMENT_RADIUS.value;
        let plate = scene::rrect(at.x, at.y, at.w, at.h, radius);
        if self.selected {
            // The design's `0 1px 2px` lift: the shadow a pixel down, and
            // a fainter one half a pixel wider round it.
            scene::fill(
                scene,
                root,
                palette.panel_shadow.multiply_alpha(0.5),
                &scene::rrect(at.x - 0.5, at.y + 0.5, at.w + 1.0, at.h + 1.5, radius + 0.5),
            );
            scene::fill(
                scene,
                root,
                palette.panel_shadow,
                &scene::rrect(at.x, at.y + 1.0, at.w, at.h, radius),
            );
            scene::fill(scene, root, cx.surface(Surface::Raised), &plate);
        } else if state.hovered {
            scene::fill(scene, root, cx.surface(Surface::Hover), &plate);
        }
        let style = if self.selected {
            roles::HINT.style().with_weight(FontWeight::SemiBold)
        } else {
            roles::HINT.style()
        };
        let words = engine.simple_layout(&self.label, style);
        let ink = if self.selected && !self.locked {
            Ink::Primary
        } else {
            Ink::Secondary
        };
        scene::draw_layout_first_line(
            scene,
            root,
            &words,
            at.x + ((at.w - words.width()) * 0.5).max(0.0),
            scene::centre_y_in(&words, at),
            Some(ink.of(palette)),
        );
        if state.focused {
            scene::stroke(scene, root, palette.accent, 1.0, &plate);
        }
    }
}

/// **A pill toggle** (D76, M7): *⚡ Fast*, *1M context* — as wide as its
/// words, fully rounded, a hairline round it. On, it is the accent's: an
/// accent border, a light accent wash and accent words. Lit under the
/// pointer; a pill that is only shown is drawn at half strength. The
/// keyboard's pill wears the accent ring outside its border.
pub struct PillLeaf {
    pub label: String,
    pub icon: Option<char>,
    pub on: bool,
    /// Its words, inset and icon: measured as the tree was built.
    pub width: f32,
}

impl Leaf for PillLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, PILL_HEIGHT.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let palette = cx.palette();
        let strength = if state.disabled { 0.5 } else { 1.0 };
        let radius = at.h * 0.5;
        let shape = scene::rrect(at.x, at.y, at.w, at.h, radius);
        let (border, words_ink) = if self.on {
            (palette.accent, palette.accent)
        } else {
            (Line::Border.of(palette), Ink::Primary.of(palette))
        };
        if self.on {
            scene::fill(
                scene,
                root,
                palette.accent.multiply_alpha(PILL_ON_FILL_ALPHA * strength),
                &shape,
            );
        }
        if state.hovered {
            scene::fill(scene, root, cx.surface(Surface::Hover), &shape);
        }
        scene::stroke(
            scene,
            root,
            border.multiply_alpha(strength),
            1.0,
            &scene::rrect(at.x + 0.5, at.y + 0.5, at.w - 1.0, at.h - 1.0, radius - 0.5),
        );
        let mut x = at.x + PILL_PAD.token.px(cx.profile);
        if let Some(glyph) = self.icon {
            let size = IconSize::XS.px(cx.profile);
            let layout = engine.icon_layout(glyph, size);
            scene::draw_layout(
                scene,
                root,
                &layout,
                x + (size - layout.width()) * 0.5,
                at.y + (at.h - layout.height()) * 0.5,
                Some(words_ink.multiply_alpha(strength)),
            );
            x += size + PILL_ICON_GAP.token.px(cx.profile);
        }
        let words = engine.simple_layout(&self.label, roles::META.style());
        scene::draw_layout_first_line(
            scene,
            root,
            &words,
            x,
            scene::centre_y_in(&words, at),
            Some(words_ink.multiply_alpha(strength)),
        );
        if state.focused {
            scene::stroke(
                scene,
                root,
                palette.accent,
                1.0,
                &scene::rrect(at.x - 2.0, at.y - 2.0, at.w + 4.0, at.h + 4.0, radius + 2.0),
            );
        }
    }
}

/// **The readout** (D76, M7): *Effort: High (default)* in the secondary
/// ink at the end of the pills' row, the design's `margin-left: auto`. Its
/// longest form that fits the room the pills leave, the shortest whatever.
pub struct ReadoutLeaf {
    pub forms: Vec<String>,
}

impl Leaf for ReadoutLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, PILL_HEIGHT.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let style = roles::HINT.style();
        let count = self.forms.len();
        for (index, form) in self.forms.iter().enumerate() {
            let words = engine.simple_layout(form, style);
            if words.width() <= at.w || index + 1 == count {
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &words,
                    (at.x + at.w - words.width()).max(at.x),
                    scene::centre_y_in(&words, at),
                    Some(Ink::Secondary.of(cx.palette())),
                );
                return;
            }
        }
    }
}
