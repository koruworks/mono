// SPDX-License-Identifier: MPL-2.0

//! **The typed tokens an element is styled with** — ELEMENTS.final §3.4.
//!
//! A style setter takes one of these, never a bare number or colour, so the
//! frame can say where a value came from: `Space::S2` resolves to 8 and still
//! reads `S2` in the inspector, and a tagged alias keeps its own name beside
//! the step it points at.
//!
//! Spacing, radii, icon sizes and metrics are the **profile's** (one fixed
//! desktop profile today). Colour roles are the **theme's**, resolved through
//! the theme's table at the frame's contrast (`themes.rs`). Only roles with a
//! verified mapping are exported; the rest of §3.4's vocabulary stays in the
//! specification until a consumer and a complete mapping exist.

use peniko::Color;

use super::themes::{Contrast, Theme};
use super::{Palette, TextStyle, role};

/// Which resolution of the dimension tokens applies. Fixed desktop now;
/// density variants arrive later without changing any token's identity.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Profile {
    #[default]
    Desktop,
}

impl Profile {
    pub const ALL: [Self; 1] = [Self::Desktop];

    pub fn key(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
        }
    }
}

/// A spacing step. `Px` is the escape path — an authored raw length the
/// source scanner rejects outside an approved allowance.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Space {
    S0,
    S0_5,
    S1,
    S1_5,
    S2,
    S2_5,
    S3,
    S3_5,
    S4,
    S5,
    S6,
    S7,
    S8,
    S10,
    S12,
    Px(f32),
}

impl Space {
    /// Every exported step: what the registry test resolves.
    pub const ALL: [Self; 15] = [
        Self::S0,
        Self::S0_5,
        Self::S1,
        Self::S1_5,
        Self::S2,
        Self::S2_5,
        Self::S3,
        Self::S3_5,
        Self::S4,
        Self::S5,
        Self::S6,
        Self::S7,
        Self::S8,
        Self::S10,
        Self::S12,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::S0 => "S0",
            Self::S0_5 => "S0_5",
            Self::S1 => "S1",
            Self::S1_5 => "S1_5",
            Self::S2 => "S2",
            Self::S2_5 => "S2_5",
            Self::S3 => "S3",
            Self::S3_5 => "S3_5",
            Self::S4 => "S4",
            Self::S5 => "S5",
            Self::S6 => "S6",
            Self::S7 => "S7",
            Self::S8 => "S8",
            Self::S10 => "S10",
            Self::S12 => "S12",
            Self::Px(_) => "Px",
        }
    }

    /// Logical units under `profile`.
    pub fn px(self, profile: Profile) -> f32 {
        match profile {
            Profile::Desktop => match self {
                Self::S0 => 0.0,
                Self::S0_5 => 2.0,
                Self::S1 => 4.0,
                Self::S1_5 => 6.0,
                Self::S2 => 8.0,
                Self::S2_5 => 10.0,
                Self::S3 => 12.0,
                Self::S3_5 => 14.0,
                Self::S4 => 16.0,
                Self::S5 => 20.0,
                Self::S6 => 24.0,
                Self::S7 => 28.0,
                Self::S8 => 32.0,
                Self::S10 => 40.0,
                Self::S12 => 48.0,
                Self::Px(v) => v,
            },
        }
    }
}

/// A corner radius. `Full` is half the box's smaller side; `Metric` is a
/// tagged legacy value the inspector names (the gear's 5); `Px` is the escape
/// path, as for [`Space`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Radius {
    None,
    S,
    M,
    L,
    XL,
    XXL,
    Full,
    Metric(Metric),
    Px(f32),
}

impl Radius {
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::S,
        Self::M,
        Self::L,
        Self::XL,
        Self::XXL,
        Self::Full,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::S => "S",
            Self::M => "M",
            Self::L => "L",
            Self::XL => "XL",
            Self::XXL => "XXL",
            Self::Full => "Full",
            Self::Metric(metric) => metric.name,
            Self::Px(_) => "Px",
        }
    }

    /// Logical units for a box `w` × `h` under `profile`.
    pub fn px(self, profile: Profile, w: f32, h: f32) -> f32 {
        let Profile::Desktop = profile;
        match self {
            Self::None => 0.0,
            Self::S => 4.0,
            Self::M => 6.0,
            Self::L => 8.0,
            Self::XL => 10.0,
            Self::XXL => 16.0,
            Self::Full => (w.min(h) * 0.5).max(0.0),
            Self::Metric(metric) => metric.value,
            Self::Px(v) => v,
        }
    }
}

/// An icon's box, square. The survey's 11/13 and optical 12.6/13.5 are not
/// rounded into these; they stay their recipes' own sizes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IconSize {
    XS,
    S,
    Caption,
    M,
    L,
    XL,
}

impl IconSize {
    pub const ALL: [Self; 6] = [Self::XS, Self::S, Self::Caption, Self::M, Self::L, Self::XL];

    pub fn name(self) -> &'static str {
        match self {
            Self::XS => "XS",
            Self::S => "S",
            Self::Caption => "Caption",
            Self::M => "M",
            Self::L => "L",
            Self::XL => "XL",
        }
    }

    pub fn px(self, profile: Profile) -> f32 {
        let Profile::Desktop = profile;
        match self {
            Self::XS => 12.0,
            Self::S => 14.0,
            Self::Caption => super::CAPTION_ICON,
            Self::M => 16.0,
            Self::L => 18.0,
            Self::XL => 28.0,
        }
    }
}

/// A tagged dimension: a named value whose name survives resolution, for a
/// size the design owns but no spacing step describes — a row's minimum, a
/// tile, the gear's legacy plate.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Metric {
    pub name: &'static str,
    pub value: f32,
}

impl Metric {
    pub const fn new(name: &'static str, value: f32) -> Self {
        Self { name, value }
    }
}

/// The named control minima and the pilot's legacy dimensions (§3.4,
/// "Typography and other dimensions"). Each is its own consumer's value; two
/// that are equal today are still two metrics.
pub mod metrics {
    use super::Metric;

    pub const NAV_ROW_MIN: Metric = Metric::new("NAV_ROW_MIN", 24.0);
    /// A popover's one-line row: `theme::ROW_H`.
    pub const ROW_MIN: Metric = Metric::new("ROW_MIN", crate::theme::ROW_H);
    pub const BUTTON_MIN: Metric = Metric::new("BUTTON_MIN", 32.0);
    pub const CAPTION_BUTTON_MIN: Metric = Metric::new("CAPTION_BUTTON_MIN", 44.0);
    /// The Profiles page's two-line row (`PROFILE_ROW_H` today).
    pub const PROFILE_ROW_MIN: Metric = Metric::new("PROFILE_ROW_MIN", 44.0);
    /// The Profiles page's foot (`PROFILE_FOOT_H` today).
    pub const PROFILE_FOOT_MIN: Metric = Metric::new("PROFILE_FOOT_MIN", 44.0);
    /// A floating panel's width: `theme::PANEL_W`.
    pub const PANEL_W: Metric = Metric::new("PANEL_W", crate::theme::PANEL_W);
    /// A workspace's tile in a popover row.
    pub const WORKSPACE_TILE: Metric = Metric::new("WORKSPACE_TILE", 24.0);
    pub const RAIL_SQUARE: Metric = Metric::new("RAIL_SQUARE", 36.0);
    pub const RAIL_GLYPH: Metric = Metric::new("RAIL_GLYPH", 18.0);
    pub const RAIL_RULE_W: Metric = Metric::new("RAIL_RULE_W", 20.0);
    /// A Profile's tile in its two-line row.
    pub const PROFILE_TILE: Metric = Metric::new("PROFILE_TILE", 24.0);
    /// The trailing lane every popover row reserves for its tick or gear.
    pub const TRAILING_LANE_W: Metric = Metric::new("TRAILING_LANE_W", 20.0);
    /// The gear's painted plate: 22 high, and the lane plus 2 a side wide.
    pub const WORKSPACE_GEAR_PLATE: Metric = Metric::new("WORKSPACE_GEAR_PLATE", 22.0);
    pub const WORKSPACE_GEAR_PLATE_W: Metric = Metric::new("WORKSPACE_GEAR_PLATE_W", 24.0);
    /// Approved legacy radius of the gear's plate and the row's lit plate.
    pub const WORKSPACE_GEAR_RADIUS: Metric = Metric::new("WORKSPACE_GEAR_RADIUS", 5.0);

    /// Every metric, for the registry test and the inspector's legend.
    pub const ALL: [Metric; 16] = [
        NAV_ROW_MIN,
        ROW_MIN,
        BUTTON_MIN,
        CAPTION_BUTTON_MIN,
        PROFILE_ROW_MIN,
        PROFILE_FOOT_MIN,
        PANEL_W,
        WORKSPACE_TILE,
        RAIL_SQUARE,
        RAIL_GLYPH,
        RAIL_RULE_W,
        PROFILE_TILE,
        TRAILING_LANE_W,
        WORKSPACE_GEAR_PLATE,
        WORKSPACE_GEAR_PLATE_W,
        WORKSPACE_GEAR_RADIUS,
    ];
}

/// A component's name for a token: both names survive, so the inspector can
/// say `PANEL_PAD → S1_5 → 6`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tagged<T> {
    pub name: &'static str,
    pub token: T,
}

impl<T> Tagged<T> {
    pub const fn new(name: &'static str, token: T) -> Self {
        Self { name, token }
    }
}

/// The aliases the pilot pages' recipes name.
pub mod alias {
    use super::{Space, Tagged};

    /// A floating panel's inset above its first row and below its last.
    pub const PANEL_PAD: Tagged<Space> = Tagged::new("PANEL_PAD", Space::S1_5);
    /// Where every navigation row's content starts.
    pub const NAV_PAD: Tagged<Space> = Tagged::new("NAV_PAD", Space::S2);
    /// The lit plate's inset from its row.
    pub const ROW_PLATE_INSET: Tagged<Space> = Tagged::new("ROW_PLATE_INSET", Space::S1);
}

/// A typography role by name: the identity survives even though the role
/// functions return a resolved [`TextStyle`].
#[derive(Copy, Clone, Debug)]
pub struct TextRole {
    pub name: &'static str,
    style: fn() -> TextStyle,
}

impl TextRole {
    pub const fn new(name: &'static str, style: fn() -> TextStyle) -> Self {
        Self { name, style }
    }

    pub fn style(self) -> TextStyle {
        (self.style)()
    }
}

impl PartialEq for TextRole {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

/// The roles an element names, beside the functions that return their styles.
pub mod roles {
    use super::{TextRole, role};

    pub const ROW: TextRole = TextRole::new("row", role::row);
    pub const HINT: TextRole = TextRole::new("hint", role::hint);
    pub const META: TextRole = TextRole::new("meta", role::meta);
    pub const UI: TextRole = TextRole::new("ui", role::ui);
    pub const POPOVER_HEADING: TextRole = TextRole::new("popover_heading", role::popover_heading);
    pub const PANE_TITLE: TextRole = TextRole::new("pane_title", role::pane_title);
    /// A page's heading on a list surface — the Tasks page's *Today* — and a
    /// document's subtitle: `role::subtitle`, 18/26.
    pub const SUBTITLE: TextRole = TextRole::new("subtitle", role::subtitle);

    pub const ALL: [TextRole; 7] = [ROW, HINT, META, UI, POPOVER_HEADING, SUBTITLE, PANE_TITLE];
}

macro_rules! colour_roles {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident => $field:ident,)+ }) => {
        $(#[$doc])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vdoc])* $variant,)+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!(stringify!($name), "::", stringify!($variant)),)+
                }
            }

            /// The palette field this role reads — the mapping §3.4 requires
            /// to be verified before a role is exported.
            pub fn field(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($field),)+
                }
            }

            /// This role's colour in `palette`.
            pub fn of(self, palette: &Palette) -> Color {
                match self {
                    $(Self::$variant => palette.$field,)+
                }
            }
        }
    };
}

colour_roles! {
    /// What a box is filled with.
    Surface {
        /// A raised surface's body: the popover, the menu, a tile without a colour.
        Raised => panel_bg,
        /// A neutral chip, and a tile whose stored colour no longer exists.
        Chip => chip_bg,
        /// The lit plate under a hovered or keyboard-lit control.
        Hover => hover_tint,
        /// The plate under the selected or current item: the rail's chosen
        /// entry, the list's current model. Verified in every registered
        /// theme (the user approved it on 2026-11-03, QUESTIONS M3).
        Selected => row_selected,
        /// **The window's ground**: its rails, title bar and lanes, and the
        /// model picker's agent rail (D45). The theme's `ground`, or the
        /// person's base colour for the theme's appearance once they chose
        /// one — which [`Surface::resolve`] applies and `of` cannot (the
        /// user approved it on 2026-11-03, QUESTIONS M2).
        Ground => ground,
    }
}

impl Surface {
    /// This role's colour under `theme` at `contrast`: the palette's own,
    /// except [`Surface::Ground`], which is the person's base colour for
    /// the theme's appearance when they chose one — what
    /// `theme::base_fill()` is for the root theme. What an element paints.
    pub fn resolve(self, theme: &Theme, contrast: Contrast) -> Color {
        let palette = theme.palette(contrast);
        match self {
            Self::Ground => {
                super::base_color(theme.is_dark()).map_or(palette.ground, super::rgb_hex)
            }
            other => other.of(palette),
        }
    }
}

colour_roles! {
    /// What text and glyphs are painted in.
    Ink {
        Primary => text,
        Secondary => text_dim,
        Hint => row_hint,
        /// Words that are an offer: the Profiles page's *Subscribe to Koru
        /// Cloud*.
        Accent => accent,
        /// Words and glyphs that warn: the chat's *Full access* approval
        /// mode (koru-ai D7, D105). The status foreground §3.4 of
        /// ELEMENTS.final names, verified in every registered theme.
        Notice => notice_fg,
    }
}

colour_roles! {
    /// Hairlines, dividers and the focus ring.
    Line {
        Border => panel_border,
        /// A rule between a list and what is fixed under it.
        Divider => panel_border,
        Focus => focus_ring,
    }
}

colour_roles! {
    /// The product accent.
    Accent {
        Base => accent,
    }
}
