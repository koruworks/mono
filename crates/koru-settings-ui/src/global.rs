// SPDX-License-Identifier: MPL-2.0

//! **The Appearance, Editor and System pages** — the Global section's rows,
//! grouped into cards.
//!
//! Every setting here existed before the sidebar did, and every value it
//! offers is the one the registry defines; the pages changed how they are
//! grouped and which control each gets, not what can be chosen. The
//! redesign's brief was *fewer things on the screen*: a font picker is one
//! field showing the font in force rather than a wall of thirteen chips, a
//! yes/no is a switch rather than two words, and a card title replaces the
//! heading every row used to wear.
//!
//! Settings wording uses the shared eight-language catalog.

use koru_draw::theme;
use koru_i18n::{Key, Lang};
use koru_settings::{
    CaretColorChoice, CaretHeightChoice, CaretWidthChoice, CloseBehaviorChoice, DensityChoice,
    DictationGesture, DictationInput, DictationLanguage, DictationTier, GlobalSettings,
    LanguageChoice, ThemeChoice, ViewModeChoice,
};
use koru_ui::DropdownOption;

use crate::action::{Action, FontRole};
use crate::page::{Anchor, Builder, TilePreview};
use crate::state::{AiSlot, DictionaryRow, RendererRow, VoiceFacts};

#[cfg(test)]
mod tests;

/// What the Global pages need. A struct rather than fourteen arguments, and
/// borrowed rather than cloned: the layout pass runs on every draw.
pub(crate) struct Rows<'a> {
    pub(crate) lang: Lang,
    pub(crate) section: &'a GlobalSettings,
    pub(crate) pinned_show_title: bool,
    pub(crate) close_behavior: CloseBehaviorChoice,
    pub(crate) renderer: &'a RendererRow,
    pub(crate) voice: &'a VoiceFacts,
    pub(crate) voice_language: &'a DictationLanguage,
    pub(crate) voice_input: &'a DictationInput,
    pub(crate) voice_tier: DictationTier,
    pub(crate) voice_gesture: DictationGesture,
    pub(crate) spelling_enabled: bool,
    pub(crate) spelling_language: &'a LanguageChoice,
    pub(crate) spelling_languages: &'a [(LanguageChoice, String)],
    pub(crate) spelling_note: Option<&'a str>,
    pub(crate) dictionary: Option<&'a DictionaryRow>,
    /// How many words the personal dictionary holds, once the host has read it.
    pub(crate) personal_words: Option<usize>,
    /// *Apply to all* was pressed this session; the link says so.
    pub(crate) layout_applied: bool,
    /// The Advanced card is unfolded.
    pub(crate) advanced_open: bool,
    /// This device's telemetry answer; `None` draws no Privacy card.
    pub(crate) telemetry: Option<bool>,
    /// Whether Koru's app-menu entry is there; `None` draws no row.
    pub(crate) app_menu: Option<bool>,
    pub(crate) ai: &'a AiSlot,
}

// ------------------------------------------------------------- Appearance

pub(crate) fn appearance(b: &mut Builder<'_>, rows: &Rows<'_>) {
    let lang = rows.lang;
    let now = crate::theme_choice(theme::theme_preference());
    b.tiles(
        Some(koru_i18n::t(lang, Key::SettingsTheme)),
        ThemeChoice::ALL
            .iter()
            .map(|choice| {
                let preview = match choice {
                    ThemeChoice::System => TilePreview::System,
                    ThemeChoice::Light => TilePreview::Light,
                    ThemeChoice::Dark => TilePreview::Dark,
                };
                (
                    Action::Theme(*choice),
                    theme_label(lang, *choice).to_owned(),
                    preview,
                    *choice == now,
                )
            })
            .collect(),
    );

    b.card(Some(koru_i18n::t(lang, Key::SettingsInterface)));
    let density_now = theme::density();
    // Tightest to loosest, not the registry's order.
    b.row_choice(
        koru_i18n::t(lang, Key::SettingsDensity),
        Some(koru_i18n::t(lang, Key::SettingsSpacingOfTextAndControls)),
        [
            DensityChoice::Compact,
            DensityChoice::Normal,
            DensityChoice::Comfortable,
        ]
        .into_iter()
        .map(|choice| {
            (
                Action::Density(choice),
                density_label(lang, choice).to_owned(),
                crate::density(choice) == density_now,
            )
        })
        .collect(),
    );
    b.row_choice(
        koru_i18n::t(lang, Key::SettingsPinnedTabs),
        None,
        vec![
            (
                Action::PinnedShowTitle(false),
                koru_i18n::t(lang, Key::SettingsIconOnly).to_owned(),
                !rows.pinned_show_title,
            ),
            (
                Action::PinnedShowTitle(true),
                koru_i18n::t(lang, Key::SettingsIconAndTitle).to_owned(),
                rows.pinned_show_title,
            ),
        ],
    );
    font_row(
        b,
        lang,
        FontRole::App,
        Some(koru_i18n::t(lang, Key::SettingsMenusSidebarAndControls)),
    );

    b.card_with_font_preview(koru_i18n::t(lang, Key::SettingsDocumentFonts));
    font_row(b, lang, FontRole::Body, None);
    font_row(b, lang, FontRole::Headings, None);
    font_row(b, lang, FontRole::Code, None);

    b.card(Some(koru_i18n::t(lang, Key::SettingsCaret)));
    let color_now = crate::caret_color_choice(theme::caret_color());
    b.row_swatches(
        koru_i18n::t(lang, Key::TerminalCursorColor),
        None,
        CaretColorChoice::ALL
            .iter()
            .map(|choice| {
                (
                    Action::CaretColor(*choice),
                    Some(crate::caret_color(*choice).color()),
                    *choice == color_now,
                )
            })
            .collect(),
    );
    let width_now = theme::caret_width();
    let height_now = theme::caret_height();
    b.row_pair(
        koru_i18n::t(lang, Key::SettingsShape),
        Some(koru_i18n::t(lang, Key::SettingsCaretShapeAlsoTerminal)),
        CaretWidthChoice::ALL
            .iter()
            .map(|choice| {
                let width = crate::caret_width(*choice);
                let named = koru_settings::TerminalCursorWidth::parse(choice.token());
                (
                    Action::CaretWidth(*choice),
                    DropdownOption::new(
                        crate::terminal::width_label(rows.lang, named),
                        width == width_now,
                    ),
                )
            })
            .collect(),
        CaretHeightChoice::ALL
            .iter()
            .map(|choice| {
                let height = crate::caret_height(*choice);
                let named = koru_settings::TerminalCursorHeight::parse(choice.token());
                (
                    Action::CaretHeight(*choice),
                    DropdownOption::new(
                        crate::terminal::height_label(rows.lang, named),
                        height == height_now,
                    ),
                )
            })
            .collect(),
    );
}

/// One font role: a dropdown of every family, each set in its own face.
fn font_row(b: &mut Builder<'_>, lang: Lang, role: FontRole, hint: Option<&str>) {
    let current = role.current();
    b.row_dropdown(
        role.row_label(lang),
        hint,
        theme::FontChoice::ALL
            .iter()
            .map(|family| {
                (
                    Action::Font(role, *family),
                    DropdownOption::new(family.label(), *family == current).in_face(family.stack()),
                )
            })
            .collect(),
    );
}

// ----------------------------------------------------------------- Editor

pub(crate) fn editor(b: &mut Builder<'_>, rows: &Rows<'_>) {
    let lang = rows.lang;
    documents(b, lang, rows.layout_applied);
    spelling(b, rows);
    voice(b, rows);
}

/// Page width for new documents, *Apply to all*, and the Page-only background.
///
/// The segments read the process globals a new document opens with, not the
/// host's copy of `Settings`: a Settings tab reopened from a stale copy would
/// otherwise show Full while the live default was already Page.
fn documents(b: &mut Builder<'_>, lang: Lang, applied: bool) {
    b.card(Some(koru_i18n::t(lang, Key::DocumentsHeading)));
    let mode = theme::default_view_mode();
    let now = crate::view_mode_choice(mode);
    // *Apply to all* is a link ending the sentence that says what it would
    // change, rather than a row of its own.
    b.next_row_link(
        koru_i18n::t(
            lang,
            if applied {
                Key::SettingsAppliedToAllDocuments
            } else {
                Key::SettingsApplyToAllLink
            },
        ),
        Action::ApplyLayoutToAll,
    );
    b.row_choice(
        koru_i18n::t(lang, Key::SettingsPageWidthForNewDocuments),
        Some(koru_i18n::t(lang, Key::SettingsExistingDocumentsKeepTheirs)),
        ViewModeChoice::ALL
            .iter()
            .map(|choice| {
                (
                    Action::ViewMode(*choice),
                    view_mode_label(lang, *choice).to_owned(),
                    *choice == now,
                )
            })
            .collect(),
    );
    // The background is a Page concept: in Narrow or Full it would be a
    // control that does nothing, so it is absent rather than dead.
    if mode == theme::ViewMode::Page {
        b.row_swatches(
            koru_i18n::t(lang, Key::SettingsPageBackground),
            Some(koru_i18n::t(
                lang,
                Key::SettingsPressToStepThroughThePageColors,
            )),
            vec![(
                Action::CanvasBackground,
                Some(crate::canvas_swatch_color(theme::default_canvas_bg())),
                false,
            )],
        );
    }
}

/// Spell-check. Keyed wording, because it had none to carry across.
fn spelling(b: &mut Builder<'_>, rows: &Rows<'_>) {
    let lang = rows.lang;
    b.card(Some(koru_i18n::t(rows.lang, Key::SpellingSectionTitle)));
    b.row_toggle(
        koru_i18n::t(rows.lang, Key::SpellingCheckWhileTyping),
        // The D6 sentence: automatic resolved to no dictionary. It names the
        // language below as the way out.
        rows.spelling_note,
        rows.spelling_enabled,
        Action::SpellcheckEnabled(!rows.spelling_enabled),
    );
    // The files the check needs are missing: say which, and offer them —
    // under the switch, because without them the switch does nothing.
    if let Some(row) = rows.dictionary.filter(|_| rows.spelling_enabled) {
        match row.action.as_deref() {
            Some(action) => b.row_button(
                koru_i18n::t(lang, Key::SettingsDictionary),
                Some(&row.line),
                Action::DownloadDictionary,
                action,
            ),
            None => b.row_text(koru_i18n::t(lang, Key::SettingsDictionary), Some(&row.line)),
        }
    }
    if rows.spelling_enabled && !rows.spelling_languages.is_empty() {
        b.row_dropdown(
            koru_i18n::t(rows.lang, Key::Language),
            None,
            rows.spelling_languages
                .iter()
                .map(|(language, label)| {
                    (
                        Action::SpellcheckLanguage(language.clone()),
                        DropdownOption::new(label.clone(), language == rows.spelling_language),
                    )
                })
                .collect(),
        );
    }
    // How many words, once the host has read the dictionary; the quiet
    // *Clear…* asks before it empties anything.
    let count = rows.personal_words.map(|words| match words {
        0 => koru_i18n::t(lang, Key::SettingsPersonalDictionaryEmpty).to_owned(),
        1 => koru_i18n::t(lang, Key::SettingsOneWordYouToldKoruToAccept).to_owned(),
        n => koru_i18n::fill(
            koru_i18n::t(lang, Key::SettingsWordsYouToldKoruToAccept),
            &[&n.to_string()],
        ),
    });
    b.row_text_button(
        koru_i18n::t(rows.lang, Key::SpellingPersonalDictionary),
        count.as_deref(),
        Action::ClearPersonalDictionary,
        koru_i18n::t(rows.lang, Key::SettingsClearEllipsis),
    );
}

/// **Voice input** — and the fourth place a person can find out dictation
/// exists.
///
/// Every reading it draws was taken by the host: the kill switch, the verified
/// model cache, the microphones the worker last saw, the size out of a signed
/// descriptor. A surface that took them itself would have to link the crates
/// that own them.
fn voice(b: &mut Builder<'_>, rows: &Rows<'_>) {
    let lang = rows.lang;
    let voice = rows.voice;
    if voice.is_absent() {
        return;
    }
    b.card_with_aside(
        Some(koru_i18n::t(lang, Key::SettingsVoiceInput)),
        Some(koru_i18n::t(lang, Key::SettingsTranscribedOnThisDevice)),
    );
    b.anchor(Anchor::Voice);
    if let Some(blocked) = voice.blocked.as_deref() {
        b.row_text(
            koru_i18n::t(lang, Key::ReminderAuthorizationUnavailable),
            Some(blocked),
        );
        return;
    }
    b.row_dropdown(
        koru_i18n::t(lang, Key::Language),
        None,
        voice
            .languages
            .iter()
            .map(|(language, label)| {
                (
                    Action::VoiceLanguage(language.clone()),
                    DropdownOption::new(label.clone(), language == rows.voice_language),
                )
            })
            .collect(),
    );
    // A remembered device that is not plugged in keeps its selection; the
    // sentence says what is being used instead. Never as an error, and never
    // silently.
    b.row_dropdown(
        koru_i18n::t(lang, Key::SettingsMicrophone),
        voice.input_note.as_deref(),
        voice
            .inputs
            .iter()
            .map(|(input, label)| {
                (
                    Action::VoiceInput(input.clone()),
                    DropdownOption::new(label.clone(), input == rows.voice_input),
                )
            })
            .collect(),
    );
    // The chord in a key chip beside the row's name, from the shortcut
    // registry; the gesture it starts beside it.
    if let Some(chord) = voice.chord.as_deref() {
        b.next_row_chip(chord);
    }
    b.row_choice(
        koru_i18n::t(lang, Key::VoiceGlobalShortcut),
        None,
        vec![
            (
                Action::VoiceGesture(DictationGesture::PushToTalk),
                koru_i18n::t(lang, Key::SettingsHoldToTalk).to_owned(),
                rows.voice_gesture == DictationGesture::PushToTalk,
            ),
            (
                Action::VoiceGesture(DictationGesture::Toggle),
                koru_i18n::t(lang, Key::SettingsToggle).to_owned(),
                rows.voice_gesture == DictationGesture::Toggle,
            ),
        ],
    );
    // The sentence changes with the selection rather than appearing, so
    // nothing below it moves when somebody presses.
    b.row_choice(
        koru_i18n::t(lang, Key::SettingsQuality),
        Some(voice.tier_note(rows.voice_tier)),
        voice
            .tiers
            .iter()
            .map(|(tier, label)| {
                (
                    Action::VoiceTier(*tier),
                    label.clone(),
                    *tier == rows.voice_tier,
                )
            })
            .collect(),
    );
    // Directly under Quality: the quality picks which model is needed, so
    // whether that model is here — and the way to get it — sits beside it.
    b.row_button(
        koru_i18n::t(lang, Key::SettingsSpeechModel),
        Some(&voice.model_line),
        Action::VoiceModels,
        &voice.model_action,
    );
    // **Consent is a record, not a preference** (ADR-251), so the row is here
    // only while there is a promise to withdraw.
    if voice.consent_granted {
        b.row_button(
            koru_i18n::t(lang, Key::SettingsConsent),
            Some(&voice.consent_line),
            Action::RevokeVoiceConsent,
            &voice.consent_action,
        );
    }
}

// ----------------------------------------------------------------- System

pub(crate) fn system(b: &mut Builder<'_>, rows: &Rows<'_>) {
    let lang = rows.lang;
    b.card(Some(koru_i18n::t(lang, Key::SettingsStartupAndWindows)));
    b.row_toggle(
        koru_i18n::t(rows.lang, Key::SettingsStartOnLogin),
        None,
        rows.section.start_on_login,
        Action::StartOnLogin(!rows.section.start_on_login),
    );
    if let Some(on) = rows.app_menu {
        b.row_toggle(
            koru_i18n::t(lang, Key::SettingsShowInAppMenu),
            Some(koru_i18n::t(lang, Key::SettingsShowInAppMenuHint)),
            on,
            Action::AppMenuEntry(!on),
        );
    }
    // *Keep in background* first because it is the default.
    b.row_choice(
        koru_i18n::t(lang, Key::SettingsWhenTheLastWindowCloses),
        Some(close_hint(lang, rows.close_behavior)),
        [
            CloseBehaviorChoice::Background,
            CloseBehaviorChoice::Ask,
            CloseBehaviorChoice::Quit,
        ]
        .into_iter()
        .map(|choice| {
            (
                Action::CloseBehavior(choice),
                close_label(lang, choice).to_owned(),
                choice == rows.close_behavior,
            )
        })
        .collect(),
    );

    // **The withdrawal the opt-in contract requires**: discoverable, and in
    // effect the moment it is pressed — the record's own write deletes
    // whatever was queued. Present whenever there is a record to show, so the
    // way back out is never hidden behind having said yes.
    if let Some(on) = rows.telemetry {
        b.card(Some(koru_i18n::t(lang, Key::SettingsPrivacy)));
        b.row_toggle(
            koru_i18n::t(lang, Key::SettingsShareUsageData),
            Some(koru_i18n::t(lang, Key::SettingsShareUsageDataHint)),
            on,
            Action::TelemetryConsent(!on),
        );
    }

    if !rows.ai.is_empty() {
        b.card(Some(koru_i18n::t(rows.lang, Key::SettingsKoruAi)));
        let note = (!rows.ai.note.is_empty()).then_some(rows.ai.note.as_str());
        match rows.ai.action.as_deref() {
            Some(action) => b.row_button(
                koru_i18n::t(lang, Key::SettingsProvidersAndModels),
                note,
                Action::OpenAiSetup,
                action,
            ),
            None => b.row_text(koru_i18n::t(lang, Key::SettingsProvidersAndModels), note),
        }
    }

    // The renderer passthrough: the one control here that does not take
    // effect as you press it, and the row says so.
    if rows.renderer.is_offered() {
        // Folded by default: nothing in it needs attention unless something
        // looks wrong, and the folded line says what is in it and its value.
        b.caption_with_link(
            koru_i18n::t(lang, Key::SettingsAdvanced),
            koru_i18n::t(
                lang,
                if rows.advanced_open {
                    Key::SettingsHide
                } else {
                    Key::SettingsShow
                },
            ),
            Action::ShowAdvanced(!rows.advanced_open),
        );
        let current = rows.renderer.current.as_str();
        if !rows.advanced_open {
            let value = rows
                .renderer
                .options
                .iter()
                .find(|(token, _)| token == current)
                .map_or(current, |(_, label)| label.as_str());
            b.note(&koru_i18n::fill(
                koru_i18n::t(lang, Key::SettingsAdvancedSummary),
                &[value],
            ));
            return;
        }
        b.card(None);
        b.row_dropdown(
            &rows.renderer.label,
            Some(&rows.renderer.note),
            rows.renderer
                .options
                .iter()
                .map(|(token, label)| {
                    (
                        Action::Renderer(token.clone()),
                        DropdownOption::new(label.clone(), token == current),
                    )
                })
                .collect(),
        );
    }
}

// ------------------------------------------------------------------- labels
//
// Shared choices use the same localized names throughout Settings.

pub(crate) fn theme_label(lang: Lang, choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::System => koru_i18n::t(lang, Key::SettingsSystem),
        ThemeChoice::Light => koru_i18n::t(lang, Key::SettingsLight),
        ThemeChoice::Dark => koru_i18n::t(lang, Key::SettingsDark),
    }
}

fn view_mode_label(lang: Lang, choice: ViewModeChoice) -> &'static str {
    match choice {
        ViewModeChoice::Page => koru_i18n::t(lang, Key::SettingsPage),
        ViewModeChoice::Narrow => koru_i18n::t(lang, Key::SettingsNarrow),
        ViewModeChoice::Full => koru_i18n::t(lang, Key::SettingsFull),
    }
}

fn density_label(lang: Lang, choice: DensityChoice) -> &'static str {
    match choice {
        DensityChoice::Compact => koru_i18n::t(lang, Key::SettingsCompact),
        DensityChoice::Normal => koru_i18n::t(lang, Key::SettingsNormal),
        DensityChoice::Comfortable => koru_i18n::t(lang, Key::SettingsComfortable),
    }
}

fn close_label(lang: Lang, choice: CloseBehaviorChoice) -> &'static str {
    match choice {
        CloseBehaviorChoice::Background => koru_i18n::t(lang, Key::SettingsKeepInBackground),
        CloseBehaviorChoice::Ask => koru_i18n::t(lang, Key::SettingsAsk),
        CloseBehaviorChoice::Quit => koru_i18n::t(lang, Key::AgentQuitConfirm),
    }
}

fn close_hint(lang: Lang, choice: CloseBehaviorChoice) -> &'static str {
    match choice {
        CloseBehaviorChoice::Background => koru_i18n::t(lang, Key::SettingsCloseBackgroundHint),
        CloseBehaviorChoice::Ask => koru_i18n::t(lang, Key::SettingsCloseAskHint),
        CloseBehaviorChoice::Quit => koru_i18n::t(lang, Key::SettingsCloseQuitHint),
    }
}
