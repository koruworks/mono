// SPDX-License-Identifier: MPL-2.0

//! **What this window's preferences mean, in its own vocabulary.**
//!
//! The link itself is [`koru_api_client::GlobalLink`] — every window in the
//! product uses the same one. What lives here is the pair of conversions only
//! a workspace window can make: an authoritative section folded onto
//! [`Settings`], and the per-field patch that carries this window's changes
//! back to the authority.
//!
//! Until SP-6b the shell parsed `preferences.conf` itself and wrote the whole
//! file back on a tick. Two windows saving at the same moment each serialized
//! the entire body, so one of them lost a value it had never touched — survey
//! note 4, and the reason [`patch`] names only the fields that moved.

use koru_draw::theme::{CaretColor, CaretHeight, CaretWidth, Density, ThemePreference, ViewMode};
use koru_settings::{
    CanvasBackground, CaretColorChoice, CaretHeightChoice, CaretWidthChoice, CloseBehaviorChoice,
    DictationGesture, DictationInput, DictationLanguage, DictationTier, Field, FontSetting,
    GlobalPatch, GlobalSettings, LanguageChoice, PopoutCloseChoice, RgbColor, ThemeChoice,
    ViewModeChoice,
};

use crate::settings::Settings;

#[cfg(test)]
mod tests;

// ------------------------------------------------------ the two conversions

/// Fold an authoritative section onto this window's [`Settings`].
///
/// Returns whether anything moved, so the caller can repaint only when there
/// is something to repaint.
pub fn adopt(section: &GlobalSettings, into: &mut Settings) -> bool {
    let before = snapshot(into);
    into.theme = ThemePreference::parse(section.theme.token());
    into.icon_pack = section
        .icon_pack
        .map(|pack| koru_draw::icons::IconPack::parse(pack.token()));
    into.caret_color = CaretColor::parse(section.caret_color.token());
    into.caret_width = CaretWidth::parse(section.caret_width.token());
    into.caret_height = CaretHeight::parse(section.caret_height.token());
    into.font = section.ui_font.clone();
    into.body_font = section.body_font.clone();
    into.headings_font = section.headings_font.clone();
    into.code_font = section.code_font.clone();
    into.chat_font = section.chat_font.clone();
    into.doc_text_size = section.document_text_size.chosen().map(|size| size.px());
    into.base_color_light = section.base_color_light.map(RgbColor::rgb);
    into.base_color_dark = section.base_color_dark.map(RgbColor::rgb);
    into.accent_color_light = section.accent_color_light.map(RgbColor::rgb);
    into.accent_color_dark = section.accent_color_dark.map(RgbColor::rgb);
    into.density = Density::parse(section.density.token());
    into.scroll_speed = section.scroll_speed.factor();
    into.zoom = section.zoom.get();
    into.view_mode = view_mode_of(section.default_view_mode);
    into.canvas_bg = canvas_of(section.default_canvas_background);
    into.pinned_show_title = section.pinned_show_title;
    into.tabs_per_workspace = section.tabs_per_workspace;
    into.popout_close = crate::settings::PopoutClose::parse(section.popout_close.token());
    into.emoji_skin_tone = section.emoji_skin_tone.clone();
    into.close_behavior = close_behavior_of(section.close_behavior);
    into.dictation_toggle = section.dictation_gesture == DictationGesture::Toggle;
    into.dictation_language =
        crate::voice_prefs::parse_language(section.dictation_language.token());
    into.dictation_tier = crate::voice_prefs::parse_tier(section.dictation_tier.token());
    into.dictation_input = crate::voice_prefs::parse_input(section.dictation_input.token());
    into.spellcheck_enabled = section.spellcheck_enabled;
    into.spellcheck_language = crate::spelling::parse_language(section.spellcheck_language.token());
    snapshot(into) != before
}

/// The patch that carries this window's preference fields back.
///
/// **Only what moved.** A field equal to what the authority last said is not
/// named, so two windows changing two different settings both land — which is
/// the whole of what the per-field shape buys.
pub fn patch(authoritative: &GlobalSettings, settings: &Settings) -> GlobalPatch {
    let mut patch = GlobalPatch::default();
    // A choice is named when it differs from what is stored. Following the
    // default is never sent: it is the authority's state, and no control here
    // takes a choice back.
    let icon_pack = settings
        .icon_pack
        .map(|pack| koru_settings::IconPackChoice::parse(pack.token()));
    if icon_pack.is_some() && icon_pack != authoritative.icon_pack {
        patch.icon_pack = icon_pack;
    }

    let theme = ThemeChoice::parse(settings.theme.key());
    if theme != authoritative.theme {
        patch.theme = Some(theme);
    }
    let caret_color = CaretColorChoice::parse(settings.caret_color.key());
    if caret_color != authoritative.caret_color {
        patch.caret_color = Some(caret_color);
    }
    let caret_width = CaretWidthChoice::parse(settings.caret_width.key());
    if caret_width != authoritative.caret_width {
        patch.caret_width = Some(caret_width);
    }
    let caret_height = CaretHeightChoice::parse(settings.caret_height.key());
    if caret_height != authoritative.caret_height {
        patch.caret_height = Some(caret_height);
    }
    for (chosen, stored, slot) in [
        (&settings.font, &authoritative.ui_font, &mut patch.ui_font),
        (
            &settings.body_font,
            &authoritative.body_font,
            &mut patch.body_font,
        ),
        (
            &settings.headings_font,
            &authoritative.headings_font,
            &mut patch.headings_font,
        ),
        (
            &settings.code_font,
            &authoritative.code_font,
            &mut patch.code_font,
        ),
        // Back to following the app font is a change like any other, and
        // is sent as `follow_default`.
        (
            &settings.chat_font,
            &authoritative.chat_font,
            &mut patch.chat_font,
        ),
    ] {
        // **The setting, compared as a setting.** Not the face it draws: a
        // role that chose the default's family, one following the default and
        // one set to a family missing here can all draw the same face and
        // still save three different things. A chosen family is written even
        // when it equals today's default — follow-the-app is a state.
        if chosen != stored {
            *slot = Some(chosen.clone());
        }
    }
    // A chosen size is named when it differs from what is stored. Following
    // the default is never sent: no control here takes a choice back.
    let size = settings
        .doc_text_size
        .and_then(koru_settings::DocTextSize::new);
    if size.is_some() && size != authoritative.document_text_size.chosen() {
        patch.document_text_size = size.map(koru_settings::DocTextSizeSetting::Chosen);
    }
    // **Unlike the size, a base color can be taken back**: the theme's own
    // swatch comes first in the row, so going back to the theme is a change
    // and is sent as a clear.
    for (chosen, stored, slot) in [
        (
            settings.base_color_light,
            authoritative.base_color_light,
            &mut patch.base_color_light,
        ),
        (
            settings.base_color_dark,
            authoritative.base_color_dark,
            &mut patch.base_color_dark,
        ),
        (
            settings.accent_color_light,
            authoritative.accent_color_light,
            &mut patch.accent_color_light,
        ),
        (
            settings.accent_color_dark,
            authoritative.accent_color_dark,
            &mut patch.accent_color_dark,
        ),
    ] {
        let chosen = chosen.and_then(RgbColor::new);
        if chosen != stored {
            *slot = chosen.map_or(Field::Clear, Field::Set);
        }
    }
    let density = koru_settings::DensityChoice::parse(settings.density.key());
    if density != authoritative.density {
        patch.density = Some(density);
    }
    let scroll_speed = koru_settings::ScrollSpeedChoice::from_factor(settings.scroll_speed);
    if scroll_speed != authoritative.scroll_speed {
        patch.scroll_speed = Some(scroll_speed);
    }
    // **Compared as it persists, not as it floats.** `0.1 * 7` and the `0.70`
    // read back out of the file are different `f32`s, and an `!=` on them made
    // every idle tick look like a zoom change — a write storm from a value
    // nobody touched.
    if zoom_token(settings.zoom) != zoom_token(authoritative.zoom.get()) {
        patch.zoom = Some(settings.zoom);
    }
    let view_mode = view_mode_to(settings.view_mode);
    if view_mode != authoritative.default_view_mode {
        patch.default_view_mode = Some(view_mode);
    }
    let canvas = canvas_to(settings.canvas_bg);
    if canvas != authoritative.default_canvas_background {
        patch.default_canvas_background = Some(canvas);
    }
    if settings.pinned_show_title != authoritative.pinned_show_title {
        patch.pinned_show_title = Some(settings.pinned_show_title);
    }
    if settings.tabs_per_workspace != authoritative.tabs_per_workspace {
        patch.tabs_per_workspace = Some(settings.tabs_per_workspace);
    }
    let popout = PopoutCloseChoice::parse(settings.popout_close.to_storage());
    if popout != authoritative.popout_close {
        patch.popout_close = Some(popout);
    }
    if settings.emoji_skin_tone != authoritative.emoji_skin_tone {
        patch.emoji_skin_tone = Some(settings.emoji_skin_tone.clone());
    }
    let gesture = if settings.dictation_toggle {
        DictationGesture::Toggle
    } else {
        DictationGesture::PushToTalk
    };
    if gesture != authoritative.dictation_gesture {
        patch.dictation_gesture = Some(gesture);
    }
    let language = DictationLanguage::parse(crate::voice_prefs::language_token(
        settings.dictation_language,
    ));
    if language != authoritative.dictation_language {
        patch.dictation_language = Some(language);
    }
    let tier = DictationTier::parse(settings.dictation_tier.as_str());
    if tier != authoritative.dictation_tier {
        patch.dictation_tier = Some(tier);
    }
    let input = DictationInput::parse(crate::voice_prefs::input_token(&settings.dictation_input));
    if input != authoritative.dictation_input {
        patch.dictation_input = Some(input);
    }
    if settings.spellcheck_enabled != authoritative.spellcheck_enabled {
        patch.spellcheck_enabled = Some(settings.spellcheck_enabled);
    }
    let spellcheck = LanguageChoice::parse(crate::spelling::language_token(
        settings.spellcheck_language,
    ));
    if spellcheck != authoritative.spellcheck_language {
        patch.spellcheck_language = Some(spellcheck);
    }
    let close_behavior = close_behavior_to(settings.close_behavior);
    if close_behavior != authoritative.close_behavior {
        patch.close_behavior = Some(close_behavior);
    }
    patch
}

fn close_behavior_of(choice: CloseBehaviorChoice) -> koru_window::CloseBehavior {
    match choice {
        CloseBehaviorChoice::Ask => koru_window::CloseBehavior::Ask,
        CloseBehaviorChoice::Background => koru_window::CloseBehavior::Background,
        CloseBehaviorChoice::Quit => koru_window::CloseBehavior::Quit,
    }
}

fn close_behavior_to(behavior: koru_window::CloseBehavior) -> CloseBehaviorChoice {
    match behavior {
        koru_window::CloseBehavior::Ask => CloseBehaviorChoice::Ask,
        koru_window::CloseBehavior::Background => CloseBehaviorChoice::Background,
        koru_window::CloseBehavior::Quit => CloseBehaviorChoice::Quit,
    }
}

/// Every preference field, in one comparable value.
///
/// One tuple rather than eighteen `!=`s: a field added to [`adopt`] and
/// forgotten here would make a real change look like no change, which is a
/// window that does not repaint until something else happens to move.
type Preferences = (
    (
        ThemePreference,
        CaretColor,
        CaretWidth,
        CaretHeight,
        Option<koru_draw::icons::IconPack>,
    ),
    (
        FontSetting,
        FontSetting,
        FontSetting,
        FontSetting,
        FontSetting,
        Option<u8>,
    ),
    (Density, String, ViewMode, Option<[u8; 4]>),
    (
        bool,
        crate::settings::PopoutClose,
        String,
        koru_window::CloseBehavior,
    ),
    (
        bool,
        koru_voice::Requested,
        koru_voice_models::ModelTier,
        koru_voice::InputChoice,
    ),
    (bool, Option<koru_spell::SpellLang>, bool),
    (Option<u32>, Option<u32>, u8),
    (Option<u32>, Option<u32>),
);

fn snapshot(settings: &Settings) -> Preferences {
    (
        (
            settings.theme,
            settings.caret_color,
            settings.caret_width,
            settings.caret_height,
            settings.icon_pack,
        ),
        (
            settings.font.clone(),
            settings.body_font.clone(),
            settings.headings_font.clone(),
            settings.code_font.clone(),
            settings.chat_font.clone(),
            settings.doc_text_size,
        ),
        (
            settings.density,
            zoom_token(settings.zoom),
            settings.view_mode,
            settings.canvas_bg,
        ),
        (
            settings.pinned_show_title,
            settings.popout_close,
            settings.emoji_skin_tone.clone(),
            settings.close_behavior,
        ),
        (
            settings.dictation_toggle,
            settings.dictation_language,
            settings.dictation_tier,
            settings.dictation_input.clone(),
        ),
        (
            settings.spellcheck_enabled,
            settings.spellcheck_language,
            settings.tabs_per_workspace,
        ),
        (
            settings.base_color_light,
            settings.base_color_dark,
            settings.scroll_speed,
        ),
        (settings.accent_color_light, settings.accent_color_dark),
    )
}

/// How a zoom factor persists — the only comparison of two of them that is
/// stable across a round trip through the file.
fn zoom_token(zoom: f32) -> String {
    format!("{zoom:.2}")
}

fn view_mode_of(choice: ViewModeChoice) -> ViewMode {
    match choice {
        ViewModeChoice::Page => ViewMode::Page,
        ViewModeChoice::Narrow => ViewMode::Narrow,
        ViewModeChoice::Full => ViewMode::Full,
    }
}

fn view_mode_to(mode: ViewMode) -> ViewModeChoice {
    match mode {
        ViewMode::Page => ViewModeChoice::Page,
        ViewMode::Narrow => ViewModeChoice::Narrow,
        ViewMode::Full => ViewModeChoice::Full,
    }
}

/// A canvas colour is stored opaque, exactly as it always was: the alpha
/// channel is presentation, never a preference.
fn canvas_of(background: CanvasBackground) -> Option<[u8; 4]> {
    match background {
        CanvasBackground::Auto => None,
        CanvasBackground::Color([r, g, b]) => Some([r, g, b, 255]),
    }
}

fn canvas_to(background: Option<[u8; 4]>) -> CanvasBackground {
    match background {
        None => CanvasBackground::Auto,
        Some([r, g, b, _]) => CanvasBackground::Color([r, g, b]),
    }
}
