// SPDX-License-Identifier: MPL-2.0

//! **`koru-appearance-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-appearance-ui";

const LIB: &str = "crates/koru-appearance-ui/src/lib.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: builds koru-editor's standalone emoji picker (an
    // in-memory emoji search) and a koru-ui ColorPickerRow; no disk, no thread
    record(
        "C-EMOJI-COLOR-PANEL",
        "EmojiColorPanel",
        Key::UiLabEmojiColorPanelName,
        Key::UiLabEmojiColorPanelPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &["EmojiColorPanel"],
        source(OWNER, "koru_appearance_ui", LIB),
        DESKTOP_SHARED,
        &[
            painter("koru_appearance_ui::EmojiColorPanel"),
            painter("koru_appearance_ui::EmojiColorPanel::new"),
            painter("koru_appearance_ui::EmojiColorPanel::for_profile"),
            painter("koru_appearance_ui::EmojiColorPanel::for_draft"),
            painter("koru_appearance_ui::EmojiColorPanel::for_profile_draft"),
            painter("koru_appearance_ui::EmojiColorPanel::draw"),
            support("koru_appearance_ui::EmojiColorPanel::panel"),
            support("koru_appearance_ui::EmojiColorPanel::key"),
            support("koru_appearance_ui::EmojiColorPanel::mouse_down"),
            support("koru_appearance_ui::EmojiColorPanel::scroll_by"),
            support("koru_appearance_ui::EmojiColorPanel::dictation_field"),
            support("koru_appearance_ui::Outcome"),
            support_with(
                "koru_appearance_ui::Target",
                &[values(
                    Key::UiLabControlKind,
                    "koru_appearance_ui::Target",
                    LIB,
                    Declared::Enum,
                    &[
                        literal("Workspace"),
                        literal("Profile"),
                        literal("Draft"),
                        literal("DraftProfile"),
                    ],
                )],
            ),
        ],
    ),
];
