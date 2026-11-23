// SPDX-License-Identifier: MPL-2.0

//! **The welcome modal's leaves**: Koru logo, semantic pill badge, selection
//! check circle, provider buttons (Google / Apple), and inline links — shared
//! between Screen 1 (welcome choices) and Screen 2 (setup eyebrow).

use koru_draw::el::{ElState, FrameCx, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::tile::Tile;
use koru_draw::{brand, inspect, scene, theme};
use koru_ui::avatar::KoruAvatar;
use koru_ui::toggle::Toggle;
use vello::Scene;
use vello::kurbo::{Affine, Arc as KurboArc, BezPath, Circle, Line, Point, Stroke, Vec2};

use super::tokens::{
    BROWSER_BADGE_DOT_GAP, BROWSER_BADGE_DOT_RADIUS, BROWSER_BADGE_DOT_STEP, BROWSER_BADGE_MARK,
    BROWSER_BADGE_RADIUS, BROWSER_BADGE_SIZE, BROWSER_BUTTON_H, BROWSER_BUTTON_ICON,
    BROWSER_BUTTON_ICON_GAP, BROWSER_STATUS_GAP, BROWSER_STATUS_H, BROWSER_STATUS_PAD_X,
    BROWSER_STATUS_RADIUS, BROWSER_STATUS_SPINNER, CHECKOUT_ICON_SIZE, WELCOME_DIVIDER_H,
    WELCOME_DIVIDER_STROKE, WELCOME_LINK_ARROW_GAP, WELCOME_LINK_ARROW_SIZE, WELCOME_LINK_H,
    WELCOME_LINK_PAD_X, WELCOME_LOGO_SIZE, WELCOME_PROVIDER_H, WELCOME_PROVIDER_MARK,
    WELCOME_PROVIDER_MARK_GAP, WELCOME_PROVIDER_RADIUS, WELCOME_RING_GAP, WELCOME_RING_W,
    WELCOME_SEGMENT_BADGE_GAP, WELCOME_SEGMENT_H, WELCOME_SEGMENT_UNDERLINE_H,
};
use super::super::setup::tokens::{
    SETUP_CHIP, SETUP_RING_GAP, SETUP_RING_RADIUS, SETUP_RING_W, SETUP_TILE_HOVER_ICON,
    SETUP_TILE_HOVER_SCRIM, SETUP_TILE_RADIUS_RATIO,
};
use super::super::BrowserProvider;

/// Koru's spiral logo mark, mipmapped by [`brand::draw_logo`].
pub(crate) struct LogoLeaf;

impl Leaf for LogoLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (WELCOME_LOGO_SIZE.value, WELCOME_LOGO_SIZE.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        brand::draw_logo(scene, root, at);
    }
}

/// Which third-party sign-in brand a provider button displays.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProviderKind {
    Google,
    Apple,
}

/// Subtle filled provider button (Google / Apple) with the brand mark and
/// centered label, filling its allocated half-row width.
pub(crate) struct ProviderButtonLeaf {
    pub(crate) kind: ProviderKind,
    pub(crate) label: String,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
}

impl Leaf for ProviderButtonLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, WELCOME_PROVIDER_H.value)
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
        let p = cx.palette();
        let _scope = inspect::scope(
            "ProviderButton",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("label", self.label.as_str(), "provider button");
                record.property("hovered", self.hovered, "view state");
                record.property("focused", self.focused, "view state");
            },
        );
        let shape = scene::rrect(at.x, at.y, at.w, at.h, WELCOME_PROVIDER_RADIUS.value);
        scene::fill(scene, root, p.code_bg, &shape);
        if self.hovered {
            scene::fill(scene, root, p.hover_tint, &shape);
        }
        if self.focused {
            let gap = WELCOME_RING_GAP.value;
            scene::stroke(
                scene,
                root,
                p.focus_ring,
                f64::from(WELCOME_RING_W.value),
                &scene::rrect(
                    at.x - gap,
                    at.y - gap,
                    at.w + gap * 2.0,
                    at.h + gap * 2.0,
                    WELCOME_PROVIDER_RADIUS.value + gap,
                ),
            );
        }
        let style = theme::TextStyle::new(theme::role::row().metrics, theme::FontWeight::Medium);
        let text = engine.simple_layout(&self.label, style);
        let mark = WELCOME_PROVIDER_MARK.value;
        let gap = WELCOME_PROVIDER_MARK_GAP.value;
        let content_w = mark + gap + text.width();
        let x = at.x + (at.w - content_w) * 0.5;
        let mark_rect = Rect::new(x, at.y + (at.h - mark) * 0.5, mark, mark);
        match self.kind {
            ProviderKind::Google => {
                let [blue, red, yellow, green] = theme::google_brand_colors();
                for (part, color) in [
                    (&brand::GOOGLE_BLUE, blue),
                    (&brand::GOOGLE_GREEN, green),
                    (&brand::GOOGLE_YELLOW, yellow),
                    (&brand::GOOGLE_RED, red),
                ] {
                    scene::draw_brand_icon(scene, root, mark_rect, color, part);
                }
            }
            ProviderKind::Apple => {
                scene::draw_brand_icon(scene, root, mark_rect, p.text, &brand::APPLE);
            }
        }
        let h = scene::first_line_height(&text);
        scene::draw_layout(
            scene,
            root,
            &text,
            x + mark + gap,
            at.y + (at.h - h) * 0.5,
            Some(p.text),
        );
    }
}

/// Inline accent link (*Use email instead*, *Start without Cloud*, *Sign in*,
/// *Not you?*, *Cancel*, *Copy the sign-in link*).
pub(crate) struct LinkLeaf {
    pub(crate) label: String,
    pub(crate) width: f32,
    pub(crate) external_arrow: bool,
    pub(crate) underlined: bool,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
}

impl LinkLeaf {
    pub(crate) fn new(
        engine: &mut TextEngine,
        label: &str,
        external_arrow: bool,
        underlined: bool,
        hovered: bool,
        focused: bool,
    ) -> Self {
        let style = link_style();
        let text_w = engine.simple_layout(label, style).width();
        let arrow_extra = if external_arrow {
            WELCOME_LINK_ARROW_GAP.value + WELCOME_LINK_ARROW_SIZE.value
        } else {
            0.0
        };
        Self {
            label: label.to_owned(),
            width: text_w + arrow_extra + WELCOME_LINK_PAD_X.value * 2.0,
            external_arrow,
            underlined,
            hovered,
            focused,
        }
    }
}

fn link_style() -> theme::TextStyle {
    theme::TextStyle::new(theme::role::meta().metrics, theme::FontWeight::Medium)
}

impl Leaf for LinkLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, WELCOME_LINK_H.value)
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
        let p = cx.palette();
        let layout = engine.simple_layout(&self.label, link_style());
        let h = scene::first_line_height(&layout);
        let arrow_extra = if self.external_arrow {
            WELCOME_LINK_ARROW_GAP.value + WELCOME_LINK_ARROW_SIZE.value
        } else {
            0.0
        };
        let content_w = layout.width() + arrow_extra;
        let text_x = at.x + (at.w - content_w) * 0.5;
        let text_y = at.y + (at.h - h) * 0.5;
        let color = p.accent;
        scene::draw_layout(scene, root, &layout, text_x, text_y, Some(color));
        if self.underlined || self.hovered || self.focused {
            let line_y = f64::from(text_y + h + 1.0);
            scene::stroke(
                scene,
                root,
                color,
                1.0,
                &Line::new(
                    (f64::from(text_x), line_y),
                    (f64::from(text_x + layout.width()), line_y),
                ),
            );
        }
        if self.external_arrow {
            let size = f64::from(WELCOME_LINK_ARROW_SIZE.value);
            let ax = f64::from(text_x + layout.width() + WELCOME_LINK_ARROW_GAP.value);
            let ay = f64::from(at.y + (at.h - WELCOME_LINK_ARROW_SIZE.value) * 0.5);
            let s = size / 12.0;
            let mut arrow = BezPath::new();
            arrow.move_to((ax + 4.5 * s, ay + 2.5 * s));
            arrow.line_to((ax + 9.5 * s, ay + 2.5 * s));
            arrow.line_to((ax + 9.5 * s, ay + 7.5 * s));
            arrow.move_to((ax + 9.5 * s, ay + 2.5 * s));
            arrow.line_to((ax + 2.5 * s, ay + 9.5 * s));
            scene.stroke(
                &Stroke::new(1.4)
                    .with_caps(vello::kurbo::Cap::Round)
                    .with_join(vello::kurbo::Join::Round),
                root,
                color,
                None,
                &arrow,
            );
        }
    }
}

/// One inline underline-tab option of the Monthly / Yearly billing selector,
/// with an optional inline teal accent label (e.g. *Save 16%*).
pub(crate) struct BillingSegmentLeaf {
    pub(crate) label: String,
    pub(crate) badge: Option<String>,
    pub(crate) width: f32,
    pub(crate) selected: bool,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) disabled: bool,
}

impl BillingSegmentLeaf {
    pub(crate) fn new(
        engine: &mut TextEngine,
        label: &str,
        badge: Option<&str>,
        selected: bool,
        hovered: bool,
        focused: bool,
        disabled: bool,
    ) -> Self {
        let style = segment_style(selected);
        let text_w = engine.simple_layout(label, style).width();
        let badge_w = badge.map_or(0.0, |b| {
            WELCOME_SEGMENT_BADGE_GAP.value
                + engine.simple_layout(b, segment_badge_style()).width()
        });
        Self {
            label: label.to_owned(),
            badge: badge.map(str::to_owned),
            width: text_w + badge_w,
            selected,
            hovered,
            focused,
            disabled,
        }
    }
}

fn segment_style(selected: bool) -> theme::TextStyle {
    theme::TextStyle::new(
        theme::role::meta().metrics,
        if selected {
            theme::FontWeight::Medium
        } else {
            theme::FontWeight::Regular
        },
    )
}

fn segment_badge_style() -> theme::TextStyle {
    theme::TextStyle::new(theme::role::meta().metrics, theme::FontWeight::Medium)
}

const BILLING_DISABLED_ALPHA: f32 = 0.45;

impl Leaf for BillingSegmentLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, WELCOME_SEGMENT_H.value)
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
        let p = cx.palette();
        let hovered = self.hovered && !self.disabled;
        let focused = self.focused && !self.disabled;
        let alpha = if self.disabled {
            BILLING_DISABLED_ALPHA
        } else {
            1.0
        };
        if focused {
            let pad_x = 4.0;
            let pad_y = 1.0;
            scene::stroke(
                scene,
                root,
                p.focus_ring,
                f64::from(WELCOME_RING_W.value),
                &scene::rrect(
                    at.x - pad_x,
                    at.y - pad_y,
                    at.w + pad_x * 2.0,
                    at.h + pad_y * 2.0,
                    4.0,
                ),
            );
        }
        let layout = engine.simple_layout(&self.label, segment_style(self.selected));
        let h = scene::first_line_height(&layout);
        let underline_h = WELCOME_SEGMENT_UNDERLINE_H.value;
        let text_area_h = (at.h - underline_h - 2.0).max(h);
        let text_x = at.x;
        let text_y = at.y + (text_area_h - h) * 0.5;
        let ink = if self.selected || hovered {
            p.text
        } else {
            p.text_dim
        }
        .multiply_alpha(alpha);
        scene::draw_layout(scene, root, &layout, text_x, text_y, Some(ink));
        if let Some(badge) = &self.badge {
            let b_layout = engine.simple_layout(badge, segment_badge_style());
            let b_x = text_x + layout.width() + WELCOME_SEGMENT_BADGE_GAP.value;
            let b_ink = p.accents[4].ink.multiply_alpha(alpha);
            scene::draw_layout(scene, root, &b_layout, b_x, text_y, Some(b_ink));
        }
        if self.selected {
            let line_y = f64::from(at.y + at.h - underline_h * 0.5);
            let line_color = p.text.multiply_alpha(if self.disabled { 0.35 } else { 1.0 });
            scene::stroke(
                scene,
                root,
                line_color,
                f64::from(underline_h),
                &Line::new((f64::from(at.x), line_y), (f64::from(at.x + at.w), line_y)),
            );
        }
    }
}

/// Full-width subtle secondary button (*Reopen checkout page*, *Continue offline*,
/// *Open the browser again*).
pub(crate) struct SecondaryFullButtonLeaf {
    pub(crate) label: String,
    pub(crate) external_icon: bool,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
}

impl Leaf for SecondaryFullButtonLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        let h = if self.external_icon {
            BROWSER_BUTTON_H.value
        } else {
            WELCOME_PROVIDER_H.value
        };
        (0.0, h)
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
        let p = cx.palette();
        let shape = scene::rrect(at.x, at.y, at.w, at.h, WELCOME_PROVIDER_RADIUS.value);
        scene::fill(scene, root, p.code_bg, &shape);
        if self.hovered {
            scene::fill(scene, root, p.hover_tint, &shape);
        }
        if self.focused {
            let gap = WELCOME_RING_GAP.value;
            scene::stroke(
                scene,
                root,
                p.focus_ring,
                f64::from(WELCOME_RING_W.value),
                &scene::rrect(
                    at.x - gap,
                    at.y - gap,
                    at.w + gap * 2.0,
                    at.h + gap * 2.0,
                    WELCOME_PROVIDER_RADIUS.value + gap,
                ),
            );
        }
        let style = theme::TextStyle::new(theme::role::row().metrics, theme::FontWeight::Medium);
        let layout = engine.simple_layout(&self.label, style);
        let h = scene::first_line_height(&layout);
        let icon_extra = if self.external_icon {
            BROWSER_BUTTON_ICON.value + BROWSER_BUTTON_ICON_GAP.value
        } else {
            0.0
        };
        let content_w = icon_extra + layout.width();
        let start_x = at.x + (at.w - content_w) * 0.5;
        if self.external_icon {
            let s = f64::from(BROWSER_BUTTON_ICON.value) / 16.0;
            let ix = f64::from(start_x);
            let iy = f64::from(at.y + (at.h - BROWSER_BUTTON_ICON.value) * 0.5);
            let mut icon = BezPath::new();
            // Open square box (top-right corner open for the arrow).
            icon.move_to((ix + 8.5 * s, iy + 3.0 * s));
            icon.line_to((ix + 4.0 * s, iy + 3.0 * s));
            icon.quad_to((ix + 2.5 * s, iy + 3.0 * s), (ix + 2.5 * s, iy + 4.5 * s));
            icon.line_to((ix + 2.5 * s, iy + 12.0 * s));
            icon.quad_to((ix + 2.5 * s, iy + 13.5 * s), (ix + 4.0 * s, iy + 13.5 * s));
            icon.line_to((ix + 11.5 * s, iy + 13.5 * s));
            icon.quad_to((ix + 13.0 * s, iy + 13.5 * s), (ix + 13.0 * s, iy + 12.0 * s));
            icon.line_to((ix + 13.0 * s, iy + 7.5 * s));
            // Diagonal arrow out to top-right.
            icon.move_to((ix + 9.5 * s, iy + 2.5 * s));
            icon.line_to((ix + 13.5 * s, iy + 2.5 * s));
            icon.line_to((ix + 13.5 * s, iy + 6.5 * s));
            icon.move_to((ix + 13.5 * s, iy + 2.5 * s));
            icon.line_to((ix + 7.5 * s, iy + 8.5 * s));
            scene.stroke(
                &Stroke::new(1.5)
                    .with_caps(vello::kurbo::Cap::Round)
                    .with_join(vello::kurbo::Join::Round),
                root,
                p.text,
                None,
                &icon,
            );
        }
        scene::draw_layout(
            scene,
            root,
            &layout,
            start_x + icon_extra,
            at.y + (at.h - h) * 0.5,
            Some(p.text),
        );
    }
}

/// Circular progress spinner at the top of *Finish checkout in your browser*.
pub(crate) struct SpinnerLeaf {
    pub(crate) angle_rad: f64,
}

impl Leaf for SpinnerLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (CHECKOUT_ICON_SIZE.value, CHECKOUT_ICON_SIZE.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let p = cx.palette();
        let center = Point::new(f64::from(at.x + at.w * 0.5), f64::from(at.y + at.h * 0.5));
        let radius = f64::from(at.w.min(at.h) * 0.5) - 3.0;
        scene::stroke(scene, root, p.panel_border, 3.0, &Circle::new(center, radius));
        let arc = KurboArc::new(
            center,
            Vec2::new(radius, radius),
            self.angle_rad,
            std::f64::consts::FRAC_PI_2,
            0.0,
        );
        scene.stroke(
            &Stroke::new(3.0).with_caps(vello::kurbo::Cap::Round),
            root,
            p.text,
            None,
            &arc,
        );
    }
}

/// Circular credit-card badge at the top of *Checkout was not completed*.
pub(crate) struct CardIconLeaf;

impl Leaf for CardIconLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (CHECKOUT_ICON_SIZE.value, CHECKOUT_ICON_SIZE.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let p = cx.palette();
        let center = Point::new(f64::from(at.x + at.w * 0.5), f64::from(at.y + at.h * 0.5));
        let radius = f64::from(at.w.min(at.h) * 0.5);
        scene::fill(scene, root, p.code_bg, &Circle::new(center, radius));
        let cw = 16.0_f32;
        let ch = 12.0_f32;
        let cx0 = at.x + (at.w - cw) * 0.5;
        let cy0 = at.y + (at.h - ch) * 0.5;
        scene::stroke(
            scene,
            root,
            p.text,
            1.4,
            &scene::rrect(cx0, cy0, cw, ch, 2.0),
        );
        let stripe_y = f64::from(cy0 + 4.0);
        scene::stroke(
            scene,
            root,
            p.text,
            1.4,
            &Line::new((f64::from(cx0), stripe_y), (f64::from(cx0 + cw), stripe_y)),
        );
        let dot_y = f64::from(cy0 + 8.5);
        scene::stroke(
            scene,
            root,
            p.text,
            1.4,
            &Line::new((f64::from(cx0 + 3.0), dot_y), (f64::from(cx0 + 6.5), dot_y)),
        );
    }
}

/// Top connector graphic on *Continue in your browser*: a solid Koru badge on
/// the left, three subtle dots in the middle, and a bordered provider badge on
/// the right (Google `G`, Apple logo, or Koru logo for email sign-in).
pub(crate) struct BrowserConnectBadgesLeaf {
    pub(crate) provider: BrowserProvider,
}

impl Leaf for BrowserConnectBadgesLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        let tile = BROWSER_BADGE_SIZE.value;
        let middle = BROWSER_BADGE_DOT_GAP.value * 2.0
            + BROWSER_BADGE_DOT_STEP.value * 2.0
            + BROWSER_BADGE_DOT_RADIUS.value * 2.0;
        (tile * 2.0 + middle, tile)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let p = cx.palette();
        let tile = BROWSER_BADGE_SIZE.value;
        let radius = BROWSER_BADGE_RADIUS.value;
        let mark = BROWSER_BADGE_MARK.value;
        let mark_inset = (tile - mark) * 0.5;
        let _scope = inspect::scope(
            "BrowserConnectBadges",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("provider", format!("{:?}", self.provider), "browser provider");
            },
        );

        // Left tile: solid inverted rounded square with monochrome Koru spiral.
        let left_shape = scene::rrect(at.x, at.y, tile, tile, radius);
        scene::fill(scene, root, p.text, &left_shape);
        let rgba = p.bg.to_rgba8();
        brand::draw_logo_tinted(
            scene,
            root,
            Rect::new(at.x + mark_inset, at.y + mark_inset, mark, mark),
            [rgba.r, rgba.g, rgba.b, rgba.a],
        );

        // Middle: three subtle dots connecting the two badges.
        let mid_x = at.x + at.w * 0.5;
        let mid_y = at.y + tile * 0.5;
        let step = BROWSER_BADGE_DOT_STEP.value;
        let dot_r = f64::from(BROWSER_BADGE_DOT_RADIUS.value);
        let dot_color = p.text_dim.multiply_alpha(0.6);
        for offset in [-step, 0.0, step] {
            scene::fill(
                scene,
                root,
                dot_color,
                &Circle::new(Point::new(f64::from(mid_x + offset), f64::from(mid_y)), dot_r),
            );
        }

        // Right tile: bordered surface square with the chosen provider's mark.
        let right_x = at.x + at.w - tile;
        let right_shape = scene::rrect(right_x, at.y, tile, tile, radius);
        scene::fill(scene, root, p.panel_bg, &right_shape);
        scene::stroke(scene, root, p.panel_border, 1.0, &right_shape);
        let provider_rect = Rect::new(right_x + mark_inset, at.y + mark_inset, mark, mark);
        match self.provider {
            BrowserProvider::Google => {
                let [blue, red, yellow, green] = theme::google_brand_colors();
                for (part, color) in [
                    (&brand::GOOGLE_BLUE, blue),
                    (&brand::GOOGLE_GREEN, green),
                    (&brand::GOOGLE_YELLOW, yellow),
                    (&brand::GOOGLE_RED, red),
                ] {
                    scene::draw_brand_icon(scene, root, provider_rect, color, part);
                }
            }
            BrowserProvider::Apple => {
                scene::draw_brand_icon(scene, root, provider_rect, p.text, &brand::APPLE);
            }
            BrowserProvider::Email => {
                brand::draw_logo(scene, root, provider_rect);
            }
        }
    }
}

/// Centered rounded status pill showing an animated spinner beside *Waiting for …*.
pub(crate) struct WaitingPillLeaf {
    pub(crate) label: String,
    pub(crate) width: f32,
    pub(crate) angle_rad: f64,
}

impl WaitingPillLeaf {
    pub(crate) fn new(engine: &mut TextEngine, label: &str, angle_rad: f64) -> Self {
        let style = waiting_pill_style();
        let text_w = engine.simple_layout(label, style).width();
        Self {
            label: label.to_owned(),
            width: BROWSER_STATUS_PAD_X.value * 2.0
                + BROWSER_STATUS_SPINNER.value
                + BROWSER_STATUS_GAP.value
                + text_w,
            angle_rad,
        }
    }
}

fn waiting_pill_style() -> theme::TextStyle {
    theme::TextStyle::new(theme::role::row().metrics, theme::FontWeight::Regular)
}

impl Leaf for WaitingPillLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, BROWSER_STATUS_H.value)
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
        let p = cx.palette();
        let _scope = inspect::scope(
            "WaitingPill",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("label", self.label.as_str(), "status pill");
            },
        );
        scene::fill(
            scene,
            root,
            p.code_bg,
            &scene::rrect(at.x, at.y, at.w, at.h, BROWSER_STATUS_RADIUS.value),
        );
        let spin = BROWSER_STATUS_SPINNER.value;
        let sx = at.x + BROWSER_STATUS_PAD_X.value;
        let sy = at.y + (at.h - spin) * 0.5;
        let center = Point::new(f64::from(sx + spin * 0.5), f64::from(sy + spin * 0.5));
        let radius = f64::from(spin * 0.5) - 1.5;
        scene::stroke(scene, root, p.panel_border, 1.8, &Circle::new(center, radius));
        let arc = KurboArc::new(
            center,
            Vec2::new(radius, radius),
            self.angle_rad,
            std::f64::consts::FRAC_PI_2,
            0.0,
        );
        scene.stroke(
            &Stroke::new(1.8).with_caps(vello::kurbo::Cap::Round),
            root,
            p.text_dim,
            None,
            &arc,
        );

        let layout = engine.simple_layout(&self.label, waiting_pill_style());
        let h = scene::first_line_height(&layout);
        scene::draw_layout(
            scene,
            root,
            &layout,
            sx + spin + BROWSER_STATUS_GAP.value,
            at.y + (at.h - h) * 0.5,
            Some(p.text_dim),
        );
    }
}

/// Multi-line wrapped paragraph whose lines are centered horizontally.
pub(crate) struct CenteredTextLeaf {
    pub(crate) text: String,
    pub(crate) style: theme::TextStyle,
    pub(crate) secondary: bool,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl CenteredTextLeaf {
    pub(crate) fn new(
        engine: &mut TextEngine,
        text: &str,
        style: theme::TextStyle,
        secondary: bool,
        max_w: f32,
    ) -> Self {
        let layout = engine.simple_layout_wrapped_centered(text, style, max_w);
        Self {
            text: text.to_owned(),
            style,
            secondary,
            width: max_w,
            height: layout.height().ceil(),
        }
    }
}

impl Leaf for CenteredTextLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, self.height)
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
        let p = cx.palette();
        let color = if self.secondary { p.text_dim } else { p.text };
        let layout = engine.simple_layout_wrapped_centered(&self.text, self.style, at.w);
        scene::draw_layout(scene, root, &layout, at.x, at.y, Some(color));
    }
}

/// Shared `KoruToggle` switch for the Koru Cloud opt-in card.
pub(crate) struct CloudToggleLeaf {
    pub(crate) on: bool,
    pub(crate) rtl: bool,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) disabled: bool,
}

impl Leaf for CloudToggleLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (Toggle::TRACK_W, Toggle::H)
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
        Toggle::new(self.on)
            .enabled(!self.disabled)
            .draw(scene, root, engine, at, self.rtl, self.hovered, self.focused);
    }
}

/// Thin horizontal divider spanning its container width.
pub(crate) struct DividerLeaf;

impl Leaf for DividerLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, WELCOME_DIVIDER_H.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let p = cx.palette();
        let y = f64::from(at.y + at.h * 0.5);
        scene::stroke(
            scene,
            root,
            p.panel_border,
            f64::from(WELCOME_DIVIDER_STROKE.value),
            &Line::new((f64::from(at.x), y), (f64::from(at.x + at.w), y)),
        );
    }
}

/// Profile tile on the Welcome modal: circular `KoruAvatar` once signed in (when
/// no custom emoji/color is chosen) and rounded square `Tile` otherwise, with the
/// same hover pencil affordance and focus ring as the setup card.
pub(crate) struct WelcomeProfileTileLeaf {
    pub(crate) icon: Option<String>,
    pub(crate) color: Option<String>,
    pub(crate) name: String,
    pub(crate) circular_avatar: bool,
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) disabled: bool,
}

impl Leaf for WelcomeProfileTileLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (SETUP_CHIP.value, SETUP_CHIP.value)
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
        let hovered = self.hovered && !self.disabled;
        let focused = self.focused && !self.disabled;
        let palette = cx.palette();
        let trimmed = self.name.trim();
        let use_circle = self.circular_avatar
            && self.icon.is_none()
            && self.color.is_none()
            && !trimmed.is_empty();
        let _scope = inspect::scope(
            "ProfileIcon",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property("hovered", hovered, "view state");
                record.property("focused", focused, "view state");
            },
        );
        if use_circle {
            KoruAvatar::initial(trimmed).draw_in(scene, root, engine, at, palette);
        } else {
            let icon = match self.icon.as_deref() {
                Some(icon) => Some(icon),
                None if trimmed.is_empty() => Some(""),
                None => None,
            };
            Tile {
                icon,
                color: self.color.as_deref(),
                name: trimmed,
            }
            .draw_in(scene, root, engine, at, palette);
        }
        if hovered {
            let radius = if use_circle {
                at.w.min(at.h) * 0.5
            } else {
                at.w.min(at.h) * SETUP_TILE_RADIUS_RATIO
            };
            scene::fill(
                scene,
                root,
                palette.bubble_bg.multiply_alpha(SETUP_TILE_HOVER_SCRIM),
                &scene::rrect(at.x, at.y, at.w, at.h, radius),
            );
            let pencil = engine.icon_layout(icons::PENCIL, SETUP_TILE_HOVER_ICON.value);
            scene::draw_layout(
                scene,
                root,
                &pencil,
                at.x + (at.w - pencil.width()) * 0.5,
                at.y + (at.h - pencil.height()) * 0.5,
                Some(palette.bubble_fg),
            );
        }
        if focused {
            let out = SETUP_RING_GAP.value;
            let radius = if use_circle {
                at.w.min(at.h) * 0.5 + out
            } else {
                SETUP_RING_RADIUS.value
            };
            scene::stroke(
                scene,
                root,
                palette.focus_ring,
                f64::from(SETUP_RING_W.value),
                &scene::rrect(
                    at.x - out,
                    at.y - out,
                    at.w + out * 2.0,
                    at.h + out * 2.0,
                    radius,
                ),
            );
        }
    }
}
