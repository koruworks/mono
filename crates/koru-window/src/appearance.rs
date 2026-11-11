// SPDX-License-Identifier: MPL-2.0

//! **What the app looks like, resolved from the authority's Global section.**
//!
//! This replaces `user_preferences.rs`, which held two independent parsers of
//! `preferences.conf` — `AppearancePreferences` and `CaretPreferences` — read
//! by the standalone AI chat window (since retired) and by the Agent's search
//! overlay. They
//! were the second of the three parsers ADR-266 removes: they disagreed with
//! the shell about what a value meant, they could not be pointed at a test
//! directory, and each read the file again at a moment nobody could predict.
//!
//! **Nothing here opens a file.** A section arrives from
//! `koru_api_client::GlobalLink`, and this is the projection of it onto the
//! process globals `koru-draw` actually draws from. It lives in this crate
//! because the windows that need it share it — the same argument that put
//! `DesktopPreferences` here.
//!
//! [`Appearance::default`] is the registry's defaults, which is what a window
//! shows before the authority has answered and what it goes on showing if the
//! authority never does.

use koru_draw::theme::{
    self, CaretColor, CaretHeight, CaretWidth, Density, FontChoice, ThemePreference,
};
use koru_settings::GlobalSettings;

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
    /// The AI chat's face, or `None` while it follows the app font — kept a
    /// state rather than a face, so a window that applies it goes on
    /// following an App font change.
    pub chat_font: Option<FontChoice>,
    pub density: Density,
    /// How many steps a wheel notch moves, 1 to 4.
    pub scroll_speed: u8,
    /// The accent chosen per theme, light then dark; `None` follows it.
    pub accent: [Option<u32>; 2],
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
            ui_font: theme::resolve_font(theme::FontSlot::Ui, &section.ui_font),
            body_font: theme::resolve_font(theme::FontSlot::Body, &section.body_font),
            headings_font: theme::resolve_font(theme::FontSlot::Headings, &section.headings_font),
            code_font: theme::resolve_font(theme::FontSlot::Code, &section.code_font),
            // A family missing here follows the app font too.
            chat_font: match &section.chat_font {
                koru_settings::FontSetting::FollowDefault => None,
                chosen @ koru_settings::FontSetting::Chosen(_) => {
                    Some(theme::resolve_font(theme::FontSlot::Chat, chosen))
                }
                koru_settings::FontSetting::Installed(name) => {
                    koru_draw::installed_fonts::resolve(name.as_str()).map(FontChoice::Installed)
                }
            },
            density: Density::parse(section.density.token()),
            scroll_speed: section.scroll_speed.factor(),
            accent: [
                section.accent_color_light.map(koru_settings::RgbColor::rgb),
                section.accent_color_dark.map(koru_settings::RgbColor::rgb),
            ],
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
        match self.chat_font {
            Some(face) => theme::set_chat_font(face),
            None => theme::set_font_setting(
                theme::FontSlot::Chat,
                koru_settings::FontSetting::FollowDefault,
            ),
        }
        theme::set_density(self.density);
        theme::set_wheel_speed(self.scroll_speed);
        theme::set_accent_color(false, self.accent[0]);
        theme::set_accent_color(true, self.accent[1]);
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
