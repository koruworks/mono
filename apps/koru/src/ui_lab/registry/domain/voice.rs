// SPDX-License-Identifier: MPL-2.0

//! **`koru-voice-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-voice-ui";

const HUD: &str = "crates/koru-voice-ui/src/hud.rs";
const LIB: &str = "crates/koru-voice-ui/src/lib.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: new(lang, model) stores a host-built view model; the
    // host owns settings receipts, hotkeys, workers and consent
    record(
        "C-VOICE-SURFACE",
        "VoiceSurface",
        Key::UiLabVoiceSurfaceName,
        Key::UiLabVoiceSurfacePurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_voice_ui", LIB),
        DESKTOP_SHARED,
        &[
            painter("koru_voice_ui::VoiceSurface"),
            painter("koru_voice_ui::VoiceSurface::new"),
            painter("koru_voice_ui::VoiceSurface::draw"),
            support("koru_voice_ui::VoiceSurface::key"),
            support("koru_voice_ui::VoiceSurface::shortcut_key"),
            support("koru_voice_ui::VoiceSurface::click"),
            support_with(
                "koru_voice_ui::ViewModel",
                &[
                    values(
                        Key::UiLabControlState,
                        "koru_voice_ui::Saved",
                        LIB,
                        Declared::Enum,
                        &[
                            literal("NeverEnabled"),
                            literal("Disabled"),
                            literal("Enabled"),
                        ],
                    ),
                    values(
                        Key::UiLabControlState,
                        "koru_voice_ui::Phase",
                        LIB,
                        Declared::Enum,
                        &[
                            literal("Idle"),
                            literal("Preparing"),
                            literal("Listening"),
                            literal("Finalising"),
                        ],
                    ),
                    values(
                        Key::UiLabControlValues,
                        "koru_voice_ui::Pending",
                        LIB,
                        Declared::Enum,
                        &[
                            literal("Enable"),
                            literal("Disable"),
                            literal("Startup"),
                            literal("Shortcut"),
                        ],
                    ),
                    values(
                        Key::UiLabControlValues,
                        "koru_voice_ui::Problem",
                        LIB,
                        Declared::Enum,
                        &[
                            literal("ShortcutTaken"),
                            literal("ProtectedTarget"),
                            literal("TargetChanged"),
                            literal("TargetUnavailable"),
                            literal("ConsentRequired"),
                            literal("WorkerUnavailable"),
                            literal("ModelUnavailable"),
                            literal("Disconnected"),
                            literal("TimedOut"),
                            literal("Refused"),
                            literal("SettingRefused"),
                            literal("UnsavedRuntime"),
                        ],
                    ),
                ],
            ),
            support("koru_voice_ui::Action"),
            support("koru_voice_ui::InputOutcome"),
            support("koru_voice_ui::WIDTH"),
            support("koru_voice_ui::HEIGHT"),
        ],
    ),
    // Construction: pure: new(lang, state); content-free by type (level and
    // lifecycle only)
    record(
        "C-VOICE-HUD",
        "VoiceHud",
        Key::UiLabVoiceHudName,
        Key::UiLabVoiceHudPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_voice_ui", HUD),
        DESKTOP_SHARED,
        &[
            painter("koru_voice_ui::VoiceHud"),
            painter_with(
                "koru_voice_ui::VoiceHud::new",
                &[values(
                    Key::UiLabControlState,
                    "koru_voice_ui::HudState",
                    HUD,
                    Declared::Enum,
                    &[
                        labelled("Recording", Key::UiLabOptionRecording),
                        labelled("Finalising", Key::UiLabOptionFinishing),
                        labelled("Refused", Key::UiLabOptionRefused),
                        labelled("Failed", Key::UiLabOptionFailed),
                    ],
                )],
            ),
            painter("koru_voice_ui::VoiceHud::draw"),
            support("koru_voice_ui::VoiceHud::action_at"),
            support("koru_voice_ui::HudState"),
            support("koru_voice_ui::HudAction"),
            support("koru_voice_ui::HUD_WIDTH"),
            support("koru_voice_ui::HUD_HEIGHT"),
        ],
    ),
];
