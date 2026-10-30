// SPDX-License-Identifier: MPL-2.0

//! **What the app looks like, resolved from the authority's Global section.**
//!
//! This replaces `user_preferences.rs`, which held two independent parsers of
//! `preferences.conf` — `AppearancePreferences` and `CaretPreferences` — read
//! by the standalone AI chat window and by the Agent's search overlay. They
//! were the second of the three parsers ADR-266 removes: they disagreed with
//! the shell about what a value meant, they could not be pointed at a test
//! directory, and each read the file again at a moment nobody could predict.
//!
//! **Nothing here opens a file.** A section arrives from
//! `koru_api_client::GlobalLink`, and this is the projection of it onto the
//! process globals `koru-draw` actually draws from. It lives in this crate
//! because two apps need it and this is the crate they share — the same
//! argument that put `DesktopPreferences` here.
//!
//! [`Appearance::default`] is the registry's defaults, which is what a window
//! shows before the authority has answered and what it goes on showing if the
//! authority never does.

use koru_draw::theme::{
    self, CaretColor, CaretHeight, CaretWidth, Density, FontChoice, ThemePreference,
};
use koru_settings::{FontSetting, GlobalSettings};

#[cfg(test)]
mod tests;

/// The caret, for a host that draws a text field and nothing else.
///
/// The Agent's search overlay is one: it has no document, no tabs and no
/// typography of its own, and the only thing it has to match the main app on
/// is the caret somebody is about to type against.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CaretAppearance {
    pub color: CaretColor,
    pub width: CaretWidth,
    pub height: CaretHeight,
}

impl CaretAppearance {
    pub fn from_section(section: &GlobalSettings) -> Self {
        Self {
            color: CaretColor::parse(section.caret_color.token()),
            width: CaretWidth::parse(section.caret_width.token()),
            height: CaretHeight::parse(section.caret_height.token()),
        }
    }

    pub fn apply(self) {
        theme::set_caret_color(self.color);
        theme::set_caret_width(self.width);
        theme::set_caret_height(self.height);
    }
}

/// Everything an independent window needs to look like Koru.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Appearance {
    pub theme: ThemePreference,
    /// The chosen icon pack, or `None` to follow the build's default.
    pub icon_pack: Option<koru_draw::icons::IconPack>,
    pub caret: CaretAppearance,
    pub ui_font: FontChoice,
    pub body_font: FontChoice,
    pub headings_font: FontChoice,
    pub code_font: FontChoice,
    pub density: Density,
    pub zoom: f32,
}

impl Default for Appearance {
    /// **The registry's defaults, spelled through the same conversion.** Not a
    /// second list: a window that has heard nothing and a window told the
    /// authority holds nothing must look identical, and deriving one from the
    /// other is the only way that stays true.
    fn default() -> Self {
        Self::from_section(&GlobalSettings::default())
    }
}

impl Appearance {
    pub fn from_section(section: &GlobalSettings) -> Self {
        Self {
            theme: ThemePreference::parse(section.theme.token()),
            icon_pack: section
                .icon_pack
                .map(|pack| koru_draw::icons::IconPack::parse(pack.token())),
            caret: CaretAppearance::from_section(section),
            ui_font: font(section.ui_font, theme::default_ui_font()),
            body_font: font(section.body_font, theme::default_body_font()),
            headings_font: font(section.headings_font, theme::default_headings_font()),
            code_font: font(section.code_font, theme::default_code_font()),
            density: Density::parse(section.density.token()),
            zoom: section.zoom.get(),
        }
    }

    /// Push all of it into the process globals every layout reads.
    pub fn apply(self) {
        theme::set_theme_preference(self.theme);
        koru_draw::icons::set_icon_pack_setting(self.icon_pack);
        theme::set_dark(self.effective_dark());
        self.caret.apply();
        theme::set_ui_font(self.ui_font);
        theme::set_body_font(self.body_font);
        theme::set_headings_font(self.headings_font);
        theme::set_code_font(self.code_font);
        theme::set_density(self.density);
        theme::set_zoom(self.zoom);
    }

    /// Whether this window still follows the OS, and so still reacts to a
    /// live light/dark change.
    pub fn follows_system(self) -> bool {
        self.theme == ThemePreference::System
    }

    pub fn effective_dark(self) -> bool {
        match self.theme {
            ThemePreference::System => crate::system_theme::system_is_dark(),
            ThemePreference::Light => false,
            ThemePreference::Dark => true,
        }
    }
}

/// The family a role resolves to. **`FollowDefault` is *this build's* default**,
/// resolved now — which is the whole reason the authority keeps it as a state
/// rather than writing today's default into a file where it would freeze.
fn font(setting: FontSetting, role_default: FontChoice) -> FontChoice {
    setting
        .family()
        .map_or(role_default, |family| FontChoice::parse(family.token()))
}
