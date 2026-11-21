// SPDX-License-Identifier: MPL-2.0

//! Shared labelled buttons: dialog answers and compact panel actions.
//!
//! It was private to `error_dialog`, and `CloseDialog` had already copied it
//! once. A third copy is how a set of buttons ends up disagreeing about what
//! "primary" looks like, so it lives here instead.
//!
//! Both recipes are functions of their arguments with no interaction state of
//! their own. Dialog buttons keep the original free-function API; compact
//! actions carry their label, style and optional icon in [`ActionButton`].

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// The smallest a button gets, so a row of them lines up even when one says
/// "OK" and another says "Move to Trash".
pub const MIN_W: f32 = 88.0;
/// The height every dialog button shares.
pub const H: f32 = 32.0;
/// Between two buttons in a row.
pub const GAP: f32 = 10.0;
const RADIUS: f32 = 6.0;
/// Air either side of the label.
const PAD_X: f32 = 24.0;

/// The label tier. The same `SM`/`Medium` the dialogs' body text uses, so a
/// button reads as part of the sentence above it rather than as chrome.
pub const LABEL: theme::TextStyle = theme::role::row();

/// What a button is *for*, which decides how it looks.
///
/// Not a colour — a colour would put the decision at the call site, and the
/// point of this type is that "the destructive one is outlined and red" is
/// decided once.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ButtonRole {
    /// Outlined. The safe answer, and the one a confirmation dialog defaults to.
    #[default]
    Secondary,
    /// Filled in the accent colour. The obvious next step.
    Primary,
    /// Outlined and red. **Never made `Primary`**: filling a destructive button
    /// in the accent colour would make "delete everything" look like the
    /// suggested route through the dialog.
    Destructive,
}

/// A compact labelled action inside a toolbar or floating panel.
///
/// This is deliberately separate from [`ButtonRole`]: dialog secondary
/// buttons are outlined surfaces, while a quiet toolbar action has no fill or
/// outline until hover. Both are buttons, but pretending they are the same
/// style is how one of them accumulates call-site exceptions.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ActionStyle {
    /// Filled in the accent colour.
    Primary,
    /// Resting outline for the safe alternative beside a primary action.
    Secondary,
    /// No resting fill or outline.
    #[default]
    Ghost,
    /// A quiet destructive action. It keeps the compact action geometry, but
    /// uses the destructive colour instead of making deletion look primary.
    Destructive,
}

/// The reusable compact text-button specification.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ActionButton<'a> {
    pub label: &'a str,
    pub style: ActionStyle,
    pub trailing_icon: Option<char>,
}

impl<'a> ActionButton<'a> {
    /// Four pixels above and below the 18 px label line.
    pub const PAD_Y: f32 = 4.0;
    pub const H: f32 = theme::text_metrics::SM.line_height + Self::PAD_Y * 2.0;
    /// Six pixels before and after the button content.
    pub const PAD_X: f32 = 6.0;
    pub const GAP: f32 = 6.0;
    pub const ICON_SIZE: f32 = 12.6;
    pub const RADIUS: f32 = 6.0;

    pub const fn new(label: &'a str, style: ActionStyle, trailing_icon: Option<char>) -> Self {
        Self {
            label,
            style,
            trailing_icon,
        }
    }

    /// Content-measured width with the component's horizontal air included.
    pub fn width(self, engine: &mut TextEngine) -> f32 {
        let text = engine.simple_layout(self.label, theme::role::row());
        let icon = self
            .trailing_icon
            .map(|glyph| engine.icon_layout(glyph, Self::ICON_SIZE));
        text.width()
            + icon.as_ref().map_or(0.0, |icon| Self::GAP + icon.width())
            + Self::PAD_X * 2.0
    }

    /// Draw the action in a caller-owned hit rectangle.
    #[track_caller]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        hovered: bool,
    ) {
        self.draw_focused(canvas, root, engine, rect, hovered, false);
    }

    /// Draw the action with an optional keyboard-focus ring.
    ///
    /// Compact actions originally appeared only in pointer-first toolbars.
    /// Form navigation also needs their borderless `Ghost` role, so focus is a
    /// shared capability rather than a one-off ring painted by the form.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw_focused(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        hovered: bool,
        focused: bool,
    ) {
        let _inspection = koru_draw::inspect::scope(
            "KoruButton",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::button::ActionButton",
                    "shared implementation",
                );
                r.property("variant", format!("{:?}", self.style), "ActionStyle");
                r.property(
                    "hovered / focused",
                    format!("{hovered} / {focused}"),
                    "instance state",
                );
                r.property(
                    "padding x / y",
                    format!("{} / {}", Self::PAD_X, Self::PAD_Y),
                    "ActionButton::PAD_X / PAD_Y; minimum recipe air, caller rectangle may expand",
                );
                r.property("icon gap", Self::GAP, "ActionButton::GAP");
                inspect_label(r, theme::role::row(), "theme::role::row()");
            },
        );
        let p = theme::p();
        let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, Self::RADIUS);
        match self.style {
            ActionStyle::Primary => {
                let fill = if hovered {
                    darkened(p.accent, 0.90)
                } else {
                    p.accent
                };
                scene::fill(canvas, root, fill, &shape);
            }
            ActionStyle::Secondary => {
                scene::fill(canvas, root, p.panel_bg, &shape);
                scene::stroke(canvas, root, p.panel_border, theme::BORDER_W, &shape);
                if hovered {
                    scene::fill(canvas, root, p.hover_tint, &shape);
                }
            }
            ActionStyle::Ghost | ActionStyle::Destructive if hovered => {
                scene::fill(canvas, root, p.hover_tint, &shape);
            }
            ActionStyle::Ghost | ActionStyle::Destructive => {}
        }
        if focused {
            scene::stroke(
                canvas,
                root,
                p.accent,
                theme::BORDER_W,
                &scene::rrect(
                    rect.x - 2.0,
                    rect.y - 2.0,
                    rect.w + 4.0,
                    rect.h + 4.0,
                    Self::RADIUS + 2.0,
                ),
            );
        }

        let colour = match self.style {
            ActionStyle::Primary => p.on_accent,
            ActionStyle::Secondary => p.text,
            ActionStyle::Ghost => p.text,
            ActionStyle::Destructive => p.close_hover,
        };
        let text = engine.simple_layout(self.label, theme::role::row());
        let icon = self
            .trailing_icon
            .map(|glyph| engine.icon_layout(glyph, Self::ICON_SIZE));
        let icon_w = icon.as_ref().map_or(0.0, |icon| icon.width());
        let gap = icon.as_ref().map_or(0.0, |_| Self::GAP);
        let content_w = text.width() + gap + icon_w;
        let content_x = rect.x + (rect.w - content_w) * 0.5;
        let label_y = crate::text_field::baseline_centred_y(&text, rect.y, rect.h);
        crate::typography::line(
            "Button label",
            canvas,
            root,
            &text,
            content_x,
            label_y,
            theme::role::row(),
            "theme::role::row()",
            colour,
        );
        if let Some(icon) = icon {
            scene::draw_layout(
                canvas,
                root,
                &icon,
                content_x + text.width() + gap,
                rect.y + (rect.h - icon.height()) * 0.5,
                Some(colour),
            );
        }
    }

    /// Draw a compact action whose label fits its allocated rectangle. The
    /// caller keeps the original label for accessibility; only paint elides it.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw_focused_elided(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        hovered: bool,
        focused: bool,
    ) {
        let (label, icon) = self.elided_content(engine, rect.w);
        ActionButton::new(&label, self.style, icon)
            .draw_focused(canvas, root, engine, rect, hovered, focused);
    }

    fn elided_content(self, engine: &mut TextEngine, width: f32) -> (String, Option<char>) {
        let available = (width - Self::PAD_X * 2.0).max(0.0);
        let icon = self
            .trailing_icon
            .filter(|glyph| engine.icon_layout(*glyph, Self::ICON_SIZE).width() <= available);
        let icon_width = icon.map_or(0.0, |glyph| {
            engine.icon_layout(glyph, Self::ICON_SIZE).width() + Self::GAP
        });
        let room = (available - icon_width).max(0.0);
        let label = engine.elided(self.label, theme::role::row(), room);
        // The text engine returns an ellipsis even when that single glyph is
        // wider than the room. Tiny controls must still respect their bounds.
        let label = if engine.simple_layout(&label, theme::role::row()).width() <= room {
            label
        } else {
            String::new()
        };
        let icon = icon.filter(|_| !label.is_empty() || icon_width <= available);
        (label, icon)
    }

    /// Draw the same stable geometry without presenting it as actionable.
    #[track_caller]
    pub fn draw_disabled(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
    ) {
        let _inspection = koru_draw::inspect::scope(
            "KoruButton",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::button::ActionButton::draw_disabled",
                    "shared implementation",
                );
                r.property("disabled", true, "instance state");
                inspect_label(r, theme::role::row(), "theme::role::row()");
            },
        );
        let text = engine.simple_layout(self.label, theme::role::row());
        crate::typography::line(
            "Button label",
            canvas,
            root,
            &text,
            rect.x + (rect.w - text.width()) * 0.5,
            crate::text_field::baseline_centred_y(&text, rect.y, rect.h),
            theme::role::row(),
            "theme::role::row()",
            theme::p().text_dim,
        );
    }
}

/// **The keyboard-focus mark a shared control wears on its own shape**: the
/// palette's `focus_ring`, twice a border's weight, on the control's rounded
/// rectangle. The mark [`crate::chip::Chip`] and [`crate::toggle::Toggle`]
/// have always drawn; the segmented control, the dropdown and the icon
/// button draw theirs through this, so the three cannot drift apart.
pub(crate) fn focus_stroke(
    canvas: &mut Scene,
    root: Affine,
    palette: &theme::Palette,
    rect: Rect,
    radius: f32,
    alpha: f32,
) {
    scene::stroke(
        canvas,
        root,
        palette.focus_ring.multiply_alpha(alpha),
        theme::BORDER_W * 2.0,
        &scene::rrect(rect.x, rect.y, rect.w, rect.h, radius),
    );
}

fn darkened(colour: vello::peniko::Color, amount: f32) -> vello::peniko::Color {
    let [r, g, b, a] = colour.components;
    vello::peniko::Color::new([r * amount, g * amount, b * amount, a])
}

/// A button wide enough for its label, never narrower than [`MIN_W`].
pub fn button_width(engine: &mut TextEngine, label: &str) -> f32 {
    engine
        .simple_layout(label, LABEL)
        .width()
        .max(MIN_W - PAD_X)
        + PAD_X
}

/// Draw one.
///
/// `hot` is the pointer being over it; `focused` is the keyboard being on it,
/// and draws the ring rather than the hover tint — the two are different facts
/// and a dialog can have both at once on different buttons.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_button(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    r: Rect,
    label: &str,
    role: ButtonRole,
    hot: bool,
    focused: bool,
) {
    draw_button_enabled(scene, root, engine, r, label, role, hot, focused, true);
}

/// Draw a button whose availability is part of the form state.
///
/// Keeping availability separate from ButtonRole matters: disabled is a
/// transient interaction state, not what an action is for.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_button_enabled(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    r: Rect,
    label: &str,
    role: ButtonRole,
    hot: bool,
    focused: bool,
    enabled: bool,
) {
    let _inspection = inspect_dialog(root, r, role, hot, focused, enabled);
    let fg = draw_button_surface(scene, root, r, role, hot, focused, enabled);
    let text = engine.simple_layout(label, LABEL);
    crate::typography::line(
        "Button label",
        scene,
        root,
        &text,
        r.x + (r.w - text.width()) * 0.5,
        scene::centre_y_in(&text, r),
        LABEL,
        "theme::role::row()",
        fg,
    );
}

/// Outline a button that was just pressed and did not work, in the same red
/// an invalid field's border uses. Drawn over the button, so any button —
/// provider buttons included — can say "this one failed" without a variant.
pub fn draw_failed_outline(scene: &mut Scene, root: Affine, r: Rect) {
    scene::stroke(
        scene,
        root,
        theme::p().close_hover,
        theme::CONTROL_BORDER_W,
        &scene::rrect(r.x, r.y, r.w, r.h, RADIUS),
    );
}

/// Draw the provider-neutral secondary button with Google's required colored
/// `G` mark. The provider decoration lives with the shared button recipe so an
/// onboarding or account surface cannot grow its own almost-matching copy.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_google_button(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    r: Rect,
    label: &str,
    hot: bool,
    focused: bool,
) {
    let _inspection = inspect_dialog(root, r, ButtonRole::Secondary, hot, focused, true);
    let fg = draw_button_surface(scene, root, r, ButtonRole::Secondary, hot, focused, true);
    const MARK: f32 = 18.0;
    const GAP: f32 = 10.0;
    let text = engine.simple_layout(label, LABEL);
    let content_w = MARK + GAP + text.width();
    let x = r.x + (r.w - content_w) * 0.5;
    draw_google_mark(
        scene,
        root,
        Rect::new(x, r.y + (r.h - MARK) * 0.5, MARK, MARK),
    );
    crate::typography::line(
        "Button label",
        scene,
        root,
        &text,
        x + MARK + GAP,
        scene::centre_y_in(&text, r),
        LABEL,
        "theme::role::row()",
        fg,
    );
}

/// A provider button for Apple, drawn in the Google button's shape.
#[track_caller]
#[allow(clippy::too_many_arguments)]
pub fn draw_apple_button(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    r: Rect,
    label: &str,
    hot: bool,
    focused: bool,
    enabled: bool,
) {
    let _inspection = inspect_dialog(root, r, ButtonRole::Secondary, hot, focused, enabled);
    let fg = draw_button_surface(scene, root, r, ButtonRole::Secondary, hot, focused, enabled);
    const MARK: f32 = 18.0;
    const GAP: f32 = 10.0;
    let text = engine.simple_layout(label, LABEL);
    let content_w = MARK + GAP + text.width();
    let x = r.x + (r.w - content_w) * 0.5;
    draw_apple_mark(
        scene,
        root,
        Rect::new(x, r.y + (r.h - MARK) * 0.5, MARK, MARK),
        fg,
    );
    crate::typography::line(
        "Button label",
        scene,
        root,
        &text,
        x + MARK + GAP,
        scene::centre_y_in(&text, r),
        LABEL,
        "theme::role::row()",
        fg,
    );
}

/// Draw the compact secondary Help button used by acquisition and account
/// surfaces. Its `ph-question` mark is part of the shared recipe, so callers
/// do not have to reproduce provider-style content layout themselves.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_help_button(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    r: Rect,
    hot: bool,
    focused: bool,
) {
    let _inspection = inspect_dialog(root, r, ButtonRole::Secondary, hot, focused, true);
    let fg = draw_button_surface(scene, root, r, ButtonRole::Secondary, hot, focused, true);
    const MARK: f32 = 16.0;
    const GAP: f32 = 7.0;
    let text = engine.simple_layout("Help", LABEL);
    let question = engine.icon_layout(koru_draw::icons::QUESTION, MARK);
    let content_w = MARK + GAP + text.width();
    let x = r.x + (r.w - content_w) * 0.5;
    scene::draw_layout_first_line(
        scene,
        root,
        &question,
        x,
        r.y + (r.h - question.height()) * 0.5,
        Some(fg),
    );
    crate::typography::line(
        "Button label",
        scene,
        root,
        &text,
        x + MARK + GAP,
        scene::centre_y_in(&text, r),
        LABEL,
        "theme::role::row()",
        fg,
    );
}

fn draw_button_surface(
    scene: &mut Scene,
    root: Affine,
    r: Rect,
    role: ButtonRole,
    hot: bool,
    focused: bool,
    enabled: bool,
) -> vello::peniko::Color {
    let p = theme::p();
    let (fill, fg) = if enabled {
        match role {
            ButtonRole::Primary => (p.accent, p.on_accent),
            ButtonRole::Secondary => (p.panel_bg, p.text),
            // The one "this is bad" colour in the palette, on the ordinary panel
            // fill — red text on a red button would not survive either theme.
            ButtonRole::Destructive => (p.panel_bg, p.close_hover),
        }
    } else {
        (p.chip_bg, p.text_dim)
    };
    let shape = scene::rrect(r.x, r.y, r.w, r.h, RADIUS);
    koru_draw::inspect::current(|r| {
        r.property(
            "foreground color",
            koru_draw::inspect::color(fg),
            "resolved shared button state",
        );
        r.property(
            "base fill color",
            koru_draw::inspect::color(fill),
            "resolved shared button state",
        );
        let mut background = fill;
        if enabled && hot {
            let [hr, hg, hb, alpha] = p.hover_tint.components;
            let [br, bg, bb, ba] = fill.components;
            background = vello::peniko::Color::new([
                hr * alpha + br * (1.0 - alpha),
                hg * alpha + bg * (1.0 - alpha),
                hb * alpha + bb * (1.0 - alpha),
                alpha + ba * (1.0 - alpha),
            ]);
        }
        r.property(
            "contrast",
            koru_draw::inspect::contrast(fg, background)
                .map_or_else(|| "Unknown composition".into(), |v| format!("{v:.2}:1")),
            "opaque dialog fill plus normal hover tint; ratio only, not accessibility acceptance",
        );
    });
    scene::fill(scene, root, fill, &shape);
    if enabled && role != ButtonRole::Primary {
        scene::stroke(
            scene,
            root,
            p.control_border,
            theme::CONTROL_BORDER_W,
            &shape,
        );
    }
    if enabled && hot {
        scene::fill(scene, root, p.hover_tint, &shape);
    }
    if enabled && focused {
        // Outside the button, so it reads as a ring around it rather than as a
        // second border on it — and so it does not eat a pixel of the fill.
        scene::stroke(
            scene,
            root,
            p.focus_ring,
            theme::BORDER_W * 2.0,
            &scene::rrect(r.x - 2.0, r.y - 2.0, r.w + 4.0, r.h + 4.0, RADIUS + 2.0),
        );
    }
    fg
}

fn inspect_label(
    r: &mut koru_draw::inspect::Record,
    style: theme::TextStyle,
    origin: &'static str,
) {
    r.property("requested family", theme::ui_stack(), "theme::ui_stack()");
    r.property(
        "font size / line height",
        format!("{} / {}", style.metrics.size, style.metrics.line_height),
        origin,
    );
    r.property("requested weight", style.weight.value(), origin);
    r.property(
        "layout",
        "horizontal content, centered in caller rectangle",
        "shared button implementation",
    );
    r.property(
        "accessibility",
        "Host semantics required; visual button role does not prove native role",
        "coverage incomplete until host association",
    );
    r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
}

#[track_caller]
fn inspect_dialog(
    root: Affine,
    bounds: Rect,
    role: ButtonRole,
    hovered: bool,
    focused: bool,
    enabled: bool,
) -> koru_draw::inspect::Scope {
    koru_draw::inspect::scope(
        "KoruButton",
        "component",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |r| {
            r.property(
                "implementation",
                "koru_ui::button::draw_button_enabled",
                "shared dialog recipe",
            );
            r.property("variant", format!("{role:?}"), "ButtonRole");
            r.property(
                "hovered / focused / enabled",
                format!("{hovered} / {focused} / {enabled}"),
                "instance state",
            );
            r.property(
                "minimum width / recipe height",
                format!("{MIN_W} / {H}"),
                "button::MIN_W / H; actual rectangle above",
            );
            r.property(
                "minimum horizontal air",
                PAD_X / 2.0,
                "button::PAD_X / 2; centered, not fixed padding",
            );
            r.property("radius", RADIUS, "shared dialog recipe");
            r.property(
                "background token",
                if !enabled {
                    "theme::p().chip_bg"
                } else if role == ButtonRole::Primary {
                    "theme::p().accent"
                } else {
                    "theme::p().panel_bg"
                },
                "shared palette; hover tint may composite",
            );
            inspect_label(r, LABEL, "button::LABEL = theme::role::row()");
        },
    )
}

/// Google's `G`, in its own four colours. The parts are the design's SVG,
/// held in [`koru_draw::brand`] beside every other vector mark.
fn draw_google_mark(scene: &mut Scene, root: Affine, r: Rect) {
    use koru_draw::brand::{GOOGLE_BLUE, GOOGLE_GREEN, GOOGLE_RED, GOOGLE_YELLOW};
    let [blue, red, yellow, green] = theme::google_brand_colors();
    for (part, color) in [
        (&GOOGLE_BLUE, blue),
        (&GOOGLE_GREEN, green),
        (&GOOGLE_YELLOW, yellow),
        (&GOOGLE_RED, red),
    ] {
        scene::draw_brand_icon(scene, root, r, color, part);
    }
}

/// The apple, in the label's own colour.
///
/// One silhouette rather than a brand palette: unlike Google's mark, Apple's
/// is monochrome, so it takes the foreground the surface already chose and
/// greys out with the rest of a disabled button.
fn draw_apple_mark(scene: &mut Scene, root: Affine, r: Rect, color: vello::peniko::Color) {
    scene::draw_brand_icon(scene, root, r, color, &koru_draw::brand::APPLE);
}

/// Lay a row of buttons out **from the right**, so the answer that ends the
/// dialog owns the corner however wide the others turn out to be.
///
/// `widths` is in reading order (left to right); so is the result.
pub fn row_from_right(right: f32, y: f32, widths: &[f32]) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(widths.len());
    let mut x = right;
    for w in widths.iter().rev() {
        x -= w;
        rects.push(Rect::new(x, y, *w, H));
        x -= GAP;
    }
    rects.reverse();
    rects
}

#[cfg(test)]
mod tests;
