// SPDX-License-Identifier: MPL-2.0

//! Text raster preferences, independent of layout and of the GPU backend.
use serde::{Deserialize, Serialize};

pub const DEFAULT_HEADING_WEIGHT: u16 = 600;
pub fn default_heading_weight() -> u16 {
    DEFAULT_HEADING_WEIGHT
}

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextAntialiasing {
    Area,
    Msaa8,
    Msaa16,
}

impl TextAntialiasing {
    pub const ALL: [Self; 3] = [Self::Area, Self::Msaa8, Self::Msaa16];
    pub const fn token(self) -> &'static str {
        match self {
            Self::Area => "area",
            Self::Msaa8 => "msaa8",
            Self::Msaa16 => "msaa16",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.token() == value)
    }
}

impl Default for TextAntialiasing {
    fn default() -> Self {
        TextRendering::DEFAULT.antialiasing
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRendering {
    pub hinting: bool,
    pub antialiasing: TextAntialiasing,
    /// Physical pixels, in both axes, applied after DPI and document zoom.
    pub offset: f32,
    pub gamma: f32,
    /// Fraction of geometric pixel coverage, before brush opacity.
    pub cutoff: f32,
}

impl TextRendering {
    // Windows values selected in the reference editor. Other platforms retain
    // their baseline until the user chooses; the same controls are available.
    pub const DEFAULT: Self = Self {
        hinting: true,
        antialiasing: if cfg!(target_os = "windows") {
            TextAntialiasing::Msaa16
        } else {
            TextAntialiasing::Area
        },
        offset: 0.0,
        gamma: if cfg!(target_os = "windows") {
            1.75
        } else {
            1.0
        },
        cutoff: if cfg!(target_os = "windows") {
            0.05
        } else {
            0.0
        },
    };
    pub const DARK_DEFAULT: Self = Self {
        offset: 0.0,
        gamma: if cfg!(target_os = "windows") {
            0.75
        } else {
            1.0
        },
        cutoff: if cfg!(target_os = "windows") {
            0.15
        } else {
            0.0
        },
        ..Self::DEFAULT
    };
    pub const fn defaults(dark: bool) -> Self {
        if dark {
            Self::DARK_DEFAULT
        } else {
            Self::DEFAULT
        }
    }
    pub fn from_section(section: &crate::GlobalSettings) -> Self {
        Self {
            hinting: section.text_hinting,
            antialiasing: section.text_antialiasing,
            offset: section.text_offset,
            gamma: section.text_gamma,
            cutoff: section.text_cutoff,
        }
    }
    pub fn patch(self) -> crate::GlobalPatch {
        crate::GlobalPatch {
            text_hinting: Some(self.hinting),
            text_antialiasing: Some(self.antialiasing),
            text_offset: Some(self.offset),
            text_gamma: Some(self.gamma),
            text_cutoff: Some(self.cutoff),
            ..Default::default()
        }
    }
}

/// Independent overrides for the two resolved themes. System follows the OS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRenderingPreferences {
    pub light: TextRendering,
    pub dark: TextRendering,
}
impl TextRenderingPreferences {
    pub const DEFAULT: Self = Self {
        light: TextRendering::DEFAULT,
        dark: TextRendering::DARK_DEFAULT,
    };
    pub fn from_section(section: &crate::GlobalSettings) -> Self {
        Self {
            light: TextRendering::from_section(section),
            dark: TextRendering {
                hinting: section.text_hinting_dark,
                antialiasing: section.text_antialiasing_dark,
                offset: section.text_offset_dark,
                gamma: section.text_gamma_dark,
                cutoff: section.text_cutoff_dark,
            },
        }
    }
    pub fn for_theme(self, dark: bool) -> TextRendering {
        if dark { self.dark } else { self.light }
    }
}
impl Default for TextRenderingPreferences {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl Default for TextRendering {
    fn default() -> Self {
        Self::DEFAULT
    }
}
pub(crate) fn default_hinting() -> bool {
    TextRendering::DEFAULT.hinting
}
pub(crate) fn default_offset() -> f32 {
    TextRendering::DEFAULT.offset
}
pub(crate) fn default_gamma() -> f32 {
    TextRendering::DEFAULT.gamma
}
pub(crate) fn default_cutoff() -> f32 {
    TextRendering::DEFAULT.cutoff
}
pub(crate) fn default_dark_gamma() -> f32 {
    TextRendering::DARK_DEFAULT.gamma
}
pub(crate) fn default_dark_cutoff() -> f32 {
    TextRendering::DARK_DEFAULT.cutoff
}
