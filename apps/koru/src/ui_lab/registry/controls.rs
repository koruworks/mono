// SPDX-License-Identifier: MPL-2.0

//! **Controls**: the shared widgets of `koru-ui`, and the input support filed
//! beside the controls that use it. Each record was written from the module
//! it names; `registry/MANIFEST.md` lists what was read.
//!
//! A widget's style, tone, size, state, placement and mode enums carry a
//! translated label per member, because a story's properties panel offers
//! them to a person. Kinds of data and host vocabularies (what a menu row
//! holds, which outcome a click reported) are listed as the source spells
//! them.

use super::*;
use koru_i18n::Key;

/// The Settings controls of 2026-09-28: the component platform record says
/// Android and iOS have no implementation of these desktop contracts.
const SETTINGS_DESKTOP: [(Platform, Availability); 5] = [
    (Platform::Windows, Availability::Existing),
    (Platform::MacOs, Availability::SharedSourceUnverified),
    (Platform::Linux, Availability::SharedSourceUnverified),
    (Platform::Android, Availability::Unavailable),
    (Platform::Ios, Availability::Unavailable),
];

const fn ui(module: &'static str, file: &'static str) -> Source {
    Source {
        owner: "koru-ui",
        module,
        file,
    }
}

#[allow(clippy::too_many_arguments)]
const fn record(
    catalog_id: &'static str,
    public_name: &'static str,
    name: Key,
    purpose: Key,
    kind: Kind,
    catalog_family: Option<&'static str>,
    identities: &'static [&'static str],
    source: Source,
    platforms: [(Platform, Availability); 5],
    apis: &'static [Api],
) -> Entry {
    Entry {
        catalog_id,
        public_name,
        name,
        purpose,
        family: Family::Controls,
        kind,
        catalog_family,
        identities,
        source,
        platforms,
        recipes: &[],
        apis,
    }
}

/// A widget symbol with nothing to choose from and no story yet.
const fn widget(symbol: &'static str) -> Api {
    plain(symbol, Story::NoStoryYet)
}

/// A widget symbol whose options are listed and that has no story yet.
const fn widget_with(symbol: &'static str, options: &'static [OptionSet]) -> Api {
    plain_with(symbol, options, Story::NoStoryYet)
}

/// Geometry, outcomes or plumbing beside a widget: it draws nothing.
const fn support(symbol: &'static str) -> Api {
    plain(symbol, Story::Nonvisual)
}

const C_CHOICE: Option<&str> = Some("C-CHOICE");
const C_OVERLAY: Option<&str> = Some("C-OVERLAY");
const C_LIST: Option<&str> = Some("C-LIST");
const C_CHROME: Option<&str> = Some("C-CHROME");
const H_INPUT: Option<&str> = Some("H-INPUT");

// ---------------------------------------------------------------- options

const ICON_BUTTON: &str = "crates/koru-ui/src/icon_button.rs";
const LABEL_CHIP: &str = "crates/koru-ui/src/label_chip.rs";
const TEXT_INPUT: &str = "crates/koru-ui/src/text_input.rs";
const TEXT_FIELD: &str = "crates/koru-ui/src/text_field.rs";

const ICON_TONE: OptionSet = values(
    Key::UiLabControlTone,
    "koru_ui::icon_button::Tone",
    ICON_BUTTON,
    Declared::Enum,
    &[
        labelled("Default", Key::UiLabOptionDefault),
        labelled("Danger", Key::UiLabOptionDanger),
    ],
);

const ICON_METRICS: OptionSet = values(
    Key::UiLabControlSize,
    "koru_ui::icon_button::Metrics",
    ICON_BUTTON,
    Declared::Consts,
    &[
        labelled("STANDARD", Key::UiLabOptionStandard),
        labelled("INLINE", Key::UiLabOptionInline),
        labelled("COMPACT", Key::UiLabOptionCompact),
    ],
);

const SPLIT_TONE: OptionSet = values(
    Key::UiLabControlTone,
    "koru_ui::split_button::SplitTone",
    "crates/koru-ui/src/split_button.rs",
    Declared::Enum,
    &[
        labelled("Accent", Key::UiLabOptionAccent),
        labelled("Success", Key::UiLabOptionSuccess),
        labelled("Neutral", Key::UiLabOptionNeutral),
        labelled("Warning", Key::UiLabOptionWarning),
    ],
);

const CHIP_SIZE: OptionSet = values(
    Key::UiLabControlSize,
    "koru_ui::label_chip::Size",
    LABEL_CHIP,
    Declared::Enum,
    &[
        labelled("Compact", Key::UiLabOptionCompact),
        labelled("Regular", Key::UiLabOptionRegular),
    ],
);

const CHIP_TONE: OptionSet = values(
    Key::UiLabControlTone,
    "koru_ui::label_chip::Tone",
    LABEL_CHIP,
    Declared::Enum,
    &[
        labelled("Neutral", Key::UiLabOptionNeutral),
        labelled("Accent", Key::UiLabOptionAccent),
        labelled("Inverted", Key::UiLabOptionInverted),
    ],
);

const CHIP_LEAD: OptionSet = values(
    Key::UiLabControlLeading,
    "koru_ui::label_chip::Lead",
    LABEL_CHIP,
    Declared::Enum,
    &[
        labelled("None", Key::UiLabOptionNone),
        labelled("Glyph", Key::UiLabOptionGlyph),
        labelled("Dot", Key::UiLabOptionDot),
    ],
);

const CHIP_TRAIL: OptionSet = values(
    Key::UiLabControlTrailing,
    "koru_ui::label_chip::Trail",
    LABEL_CHIP,
    Declared::Enum,
    &[
        labelled("None", Key::UiLabOptionNone),
        labelled("Caret", Key::UiLabOptionCaret),
        labelled("Clear", Key::UiLabOptionClear),
        labelled("Remove", Key::UiLabOptionRemove),
    ],
);

const CONTEXT_STATE: OptionSet = values(
    Key::UiLabControlState,
    "koru_ui::context_chip::ContextState",
    "crates/koru-ui/src/context_chip.rs",
    Declared::Enum,
    &[
        labelled("Ready", Key::UiLabOptionReady),
        labelled("Pending", Key::UiLabOptionPending),
        labelled("Refused", Key::UiLabOptionRefused),
    ],
);

const INPUT_STYLE: OptionSet = values(
    Key::UiLabControlStyle,
    "koru_ui::text_input::InputStyle",
    TEXT_INPUT,
    Declared::Functions,
    &[
        labelled("search", Key::UiLabOptionSearch),
        labelled("inline", Key::UiLabOptionInline),
        labelled("form", Key::UiLabOptionForm),
        labelled("bare", Key::UiLabOptionBare),
    ],
);

const INPUT_INVALID: OptionSet = values(
    Key::UiLabControlState,
    "koru_ui::text_input::InputStyle",
    TEXT_INPUT,
    Declared::Functions,
    &[labelled("invalid", Key::UiLabOptionInvalid)],
);

const END_ICON: OptionSet = values(
    Key::UiLabControlTrailing,
    "koru_ui::text_input::EndIcon",
    TEXT_INPUT,
    Declared::Enum,
    &[labelled("Reveal", Key::UiLabOptionReveal)],
);

const FIELD_CARET: OptionSet = values(
    Key::UiLabControlMode,
    "koru_ui::text_field::FieldCaret",
    TEXT_FIELD,
    Declared::Enum,
    &[
        labelled("Preference", Key::UiLabOptionPreference),
        labelled("Form", Key::UiLabOptionForm),
        labelled("Hidden", Key::UiLabOptionHidden),
    ],
);

const POPOVER_ALIGN: OptionSet = values(
    Key::UiLabControlAlignment,
    "koru_ui::popover::PopoverAlign",
    "crates/koru-ui/src/popover.rs",
    Declared::Enum,
    &[
        labelled("Start", Key::UiLabOptionStart),
        labelled("End", Key::UiLabOptionEnd),
    ],
);

const TOOLTIP_PLACEMENT: OptionSet = values(
    Key::UiLabControlPlacement,
    "koru_ui::tooltip::Placement",
    "crates/koru-ui/src/tooltip.rs",
    Declared::Enum,
    &[
        labelled("Below", Key::UiLabOptionBelow),
        labelled("Beside", Key::UiLabOptionBeside),
    ],
);

const MENU_ANCHOR: OptionSet = values(
    Key::UiLabControlPlacement,
    "koru_ui::menu::Anchor",
    "crates/koru-ui/src/menu.rs",
    Declared::Enum,
    &[
        labelled("Cursor", Key::UiLabOptionAtPointer),
        labelled("ButtonRight", Key::UiLabOptionButtonRight),
        labelled("Centre", Key::UiLabOptionCentred),
    ],
);

const MENU_CONTROL: OptionSet = values(
    Key::UiLabControlKind,
    "koru_ui::menu::Control",
    "crates/koru-ui/src/menu.rs",
    Declared::Enum,
    &[literal("Button"), literal("Readout")],
);

const PROGRESS_MODE: OptionSet = values(
    Key::UiLabControlMode,
    "koru_ui::progress_bar::ProgressMode",
    "crates/koru-ui/src/progress_bar.rs",
    Declared::Enum,
    &[
        labelled("Indeterminate", Key::UiLabOptionIndeterminate),
        labelled("Determinate", Key::UiLabOptionDeterminate),
    ],
);

const ROW_STATE: OptionSet = values(
    Key::UiLabControlState,
    "koru_ui::table::RowState",
    "crates/koru-ui/src/table.rs",
    Declared::Enum,
    &[
        labelled("Idle", Key::UiLabOptionAtRest),
        labelled("Hovered", Key::UiLabOptionHovered),
        labelled("Selected", Key::UiLabOptionSelected),
    ],
);

const NAV_LEADING: OptionSet = values(
    Key::UiLabControlLeading,
    "koru_ui::nav_list::NavLeading",
    "crates/koru-ui/src/nav_list.rs",
    Declared::Enum,
    &[
        labelled("Tile", Key::UiLabOptionTile),
        labelled("Add", Key::UiLabOptionAdd),
        labelled("Glyph", Key::UiLabOptionGlyph),
    ],
);

const CONFIRM_SHAPE: OptionSet = values(
    Key::UiLabControlStyle,
    "koru_ui::confirm::Confirm",
    "crates/koru-ui/src/confirm.rs",
    Declared::Functions,
    &[
        labelled("new", Key::UiLabStyleDestructive),
        labelled("asking", Key::UiLabOptionAsking),
    ],
);

const COMPOSE_INTO: OptionSet = values(
    Key::UiLabControlKind,
    "koru_ui::dictation::ComposeInto",
    "crates/koru-ui/src/dictation.rs",
    Declared::Enum,
    &[
        literal("Area"),
        literal("Field"),
        literal("Input"),
        literal("Held"),
    ],
);

// ---------------------------------------------------------------- records

const ACTION_RECORDS: [Entry; 4] = [
    record(
        "C-ICON-BUTTON",
        "KoruIconButton",
        Key::UiLabIconButtonName,
        Key::UiLabIconButtonPurpose,
        Kind::Widget,
        Some("C-ICON"),
        &["KoruIconButton"],
        ui("koru_ui::icon_button", ICON_BUTTON),
        DESKTOP_SHARED,
        &[
            widget_with(
                "koru_ui::icon_button::IconButton",
                &[ICON_TONE, ICON_METRICS],
            ),
            widget_with("koru_ui::icon_button::draw_toned", &[ICON_TONE]),
            widget("koru_ui::icon_button::draw"),
            widget("koru_ui::icon_button::draw_with"),
            widget("koru_ui::icon_button::draw_with_in"),
            widget("koru_ui::icon_button::draw_label_with"),
            widget("koru_ui::icon_button::hover_chip"),
            support("koru_ui::icon_button::State"),
        ],
    ),
    record(
        "C-BUTTON-GROUP",
        "KoruButtonGroup",
        Key::UiLabButtonGroupName,
        Key::UiLabButtonGroupPurpose,
        Kind::Widget,
        Some("C-ICON"),
        &["KoruButtonGroup"],
        ui(
            "koru_ui::button_group",
            "crates/koru-ui/src/button_group.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::button_group::ButtonGroup"),
            support("koru_ui::button_group::slots_right"),
        ],
    ),
    record(
        "C-SPLIT-BUTTON",
        "KoruSplitButton",
        Key::UiLabSplitButtonName,
        Key::UiLabSplitButtonPurpose,
        Kind::Widget,
        None,
        &["KoruSplitButton"],
        ui(
            "koru_ui::split_button",
            "crates/koru-ui/src/split_button.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget_with("koru_ui::split_button::SplitButton", &[SPLIT_TONE]),
            support("koru_ui::split_button::SplitPart"),
        ],
    ),
    record(
        "C-COPY",
        "KoruCopyAffordance",
        Key::UiLabCopyAffordanceName,
        Key::UiLabCopyAffordancePurpose,
        Kind::Widget,
        Some("C-COPY"),
        &[],
        ui(
            "koru_ui::copy_affordance",
            "crates/koru-ui/src/copy_affordance.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::copy_affordance::CopyAffordance"),
            support("koru_ui::copy_affordance::Outcome"),
        ],
    ),
];

const CHOICE_RECORDS: [Entry; 13] = [
    record(
        "C-CHIP",
        "KoruChip",
        Key::UiLabChipName,
        Key::UiLabChipPurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruChip"],
        ui("koru_ui::chip", "crates/koru-ui/src/chip.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::chip::Chip"),
            support("koru_ui::chip::wrap"),
        ],
    ),
    record(
        "C-SEGMENTED",
        "KoruSegmented",
        Key::UiLabSegmentedName,
        Key::UiLabSegmentedPurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruSegmented"],
        ui("koru_ui::segmented", "crates/koru-ui/src/segmented.rs"),
        SETTINGS_DESKTOP,
        &[widget("koru_ui::segmented::Segmented")],
    ),
    record(
        "C-DROPDOWN",
        "KoruDropdown",
        Key::UiLabDropdownName,
        Key::UiLabDropdownPurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruDropdown"],
        ui("koru_ui::dropdown", "crates/koru-ui/src/dropdown.rs"),
        SETTINGS_DESKTOP,
        &[
            widget("koru_ui::dropdown::Dropdown"),
            support("koru_ui::dropdown::DropdownOption"),
        ],
    ),
    record(
        "C-TOGGLE",
        "KoruToggle",
        Key::UiLabToggleName,
        Key::UiLabTogglePurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruToggle"],
        ui("koru_ui::toggle", "crates/koru-ui/src/toggle.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::toggle::Toggle")],
    ),
    record(
        "C-SWATCH",
        "KoruSwatch",
        Key::UiLabSwatchName,
        Key::UiLabSwatchPurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruSwatch"],
        ui("koru_ui::swatch", "crates/koru-ui/src/swatch.rs"),
        SETTINGS_DESKTOP,
        &[widget("koru_ui::swatch::Swatch")],
    ),
    record(
        "C-COLOR-PICKER-ROW",
        "ColorPickerRow",
        Key::UiLabColorPickerRowName,
        Key::UiLabColorPickerRowPurpose,
        Kind::Widget,
        C_CHOICE,
        &["ColorPickerRow"],
        ui(
            "koru_ui::color_picker_row",
            "crates/koru-ui/src/color_picker_row.rs",
        ),
        DESKTOP_SHARED,
        &[widget("koru_ui::color_picker_row::ColorPickerRow")],
    ),
    record(
        "C-STEPPER",
        "KoruStepper",
        Key::UiLabStepperName,
        Key::UiLabStepperPurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruStepper"],
        ui("koru_ui::stepper", "crates/koru-ui/src/stepper.rs"),
        SETTINGS_DESKTOP,
        &[
            widget("koru_ui::stepper::Stepper"),
            support("koru_ui::stepper::Step"),
        ],
    ),
    record(
        "C-PREVIEW-TILE",
        "KoruPreviewTile",
        Key::UiLabPreviewTileName,
        Key::UiLabPreviewTilePurpose,
        Kind::Widget,
        C_CHOICE,
        &["KoruPreviewTile"],
        ui(
            "koru_ui::preview_tile",
            "crates/koru-ui/src/preview_tile.rs",
        ),
        SETTINGS_DESKTOP,
        &[widget("koru_ui::preview_tile::PreviewTile")],
    ),
    record(
        "C-PROFILE-PICKER",
        "KoruProfilePicker",
        Key::UiLabProfilePickerName,
        Key::UiLabProfilePickerPurpose,
        Kind::Widget,
        Some("C-PICKER"),
        &[],
        ui(
            "koru_ui::profile_picker",
            "crates/koru-ui/src/profile_picker.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::profile_picker::ProfileRow"),
            widget("koru_ui::profile_picker::draw_heading"),
            widget("koru_ui::profile_picker::draw_row"),
            widget("koru_ui::profile_picker::draw_avatar"),
            support("koru_ui::profile_picker::row_at"),
            support("koru_ui::profile_picker::initials"),
        ],
    ),
    record(
        "C-BADGE",
        "KoruBadge",
        Key::UiLabBadgeName,
        Key::UiLabBadgePurpose,
        Kind::Widget,
        C_CHOICE,
        &[],
        ui("koru_ui::badge", "crates/koru-ui/src/badge.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::badge::NumericBadge")],
    ),
    record(
        "C-LABEL-CHIP",
        "KoruLabelChip",
        Key::UiLabLabelChipName,
        Key::UiLabLabelChipPurpose,
        Kind::Widget,
        None,
        &["KoruLabelChip"],
        ui("koru_ui::label_chip", LABEL_CHIP),
        DESKTOP_SHARED,
        &[
            widget_with(
                "koru_ui::label_chip::LabelChip",
                &[CHIP_SIZE, CHIP_TONE, CHIP_LEAD, CHIP_TRAIL],
            ),
            support("koru_ui::label_chip::State"),
        ],
    ),
    record(
        "C-CONTEXT-CHIP",
        "KoruContextChip",
        Key::UiLabContextChipName,
        Key::UiLabContextChipPurpose,
        Kind::Widget,
        None,
        &["KoruContextChip"],
        ui(
            "koru_ui::context_chip",
            "crates/koru-ui/src/context_chip.rs",
        ),
        DESKTOP_SHARED,
        &[widget_with(
            "koru_ui::context_chip::ContextChip",
            &[CONTEXT_STATE],
        )],
    ),
    record(
        "C-KEY-CHIP",
        "KoruKeyChip",
        Key::UiLabKeyChipName,
        Key::UiLabKeyChipPurpose,
        Kind::Widget,
        C_CHROME,
        &["KoruKeyChip"],
        ui("koru_ui::key_chip", "crates/koru-ui/src/key_chip.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::key_chip::KeyChip")],
    ),
];

const INPUT_RECORDS: [Entry; 4] = [
    record(
        "C-TEXT-FIELD",
        "KoruTextField",
        Key::UiLabTextFieldName,
        Key::UiLabTextFieldPurpose,
        Kind::Widget,
        Some("C-INPUT"),
        &[],
        ui("koru_ui::text_field", TEXT_FIELD),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::text_field::TextField"),
            widget_with("koru_ui::text_field::draw_field", &[FIELD_CARET]),
            widget("koru_ui::text_field::draw_field_with_caret"),
            widget("koru_ui::text_field::draw_panel_search"),
            support("koru_ui::text_field::search_rect"),
            support("koru_ui::text_field::Typed"),
        ],
    ),
    record(
        "C-TEXT-AREA",
        "KoruTextArea",
        Key::UiLabTextAreaName,
        Key::UiLabTextAreaPurpose,
        Kind::Widget,
        Some("C-INPUT"),
        &[],
        ui("koru_ui::text_field", TEXT_FIELD),
        DESKTOP_SHARED,
        &[widget("koru_ui::text_field::TextArea")],
    ),
    record(
        "C-TEXT-INPUT",
        "KoruTextInput",
        Key::UiLabTextInputName,
        Key::UiLabTextInputPurpose,
        Kind::Widget,
        Some("C-INPUT"),
        &["KoruTextInput", "KoruInputEndIcon"],
        ui("koru_ui::text_input", TEXT_INPUT),
        DESKTOP_SHARED,
        &[
            widget_with("koru_ui::text_input::TextInput", &[END_ICON]),
            widget_with(
                "koru_ui::text_input::InputStyle",
                &[INPUT_STYLE, INPUT_INVALID],
            ),
            widget("koru_ui::text_input::draw_text_input"),
        ],
    ),
    record(
        "C-DATE-GRID",
        "KoruDateGrid",
        Key::UiLabDateGridName,
        Key::UiLabDateGridPurpose,
        Kind::Widget,
        Some("C-PICKER"),
        &[],
        ui("koru_ui::date_grid", "crates/koru-ui/src/date_grid.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::date_grid::DateGrid"),
            widget("koru_ui::date_grid::draw"),
            widget("koru_ui::date_grid::draw_month"),
            support("koru_ui::date_grid::cell_at"),
            support("koru_ui::date_grid::Pick"),
        ],
    ),
];

const LIST_RECORDS: [Entry; 6] = [
    record(
        "C-SELECTABLE-LIST",
        "KoruSelectableList",
        Key::UiLabSelectableListName,
        Key::UiLabSelectableListPurpose,
        Kind::Support,
        C_LIST,
        &[],
        ui(
            "koru_ui::selectable_list",
            "crates/koru-ui/src/selectable_list.rs",
        ),
        DESKTOP_SHARED,
        &[
            support("koru_ui::selectable_list::SelectableList"),
            support("koru_ui::selectable_list::ListScrollbar"),
        ],
    ),
    record(
        "C-EXIT-LIST",
        "KoruExitList",
        Key::UiLabExitListName,
        Key::UiLabExitListPurpose,
        Kind::Support,
        C_LIST,
        &[],
        ui("koru_ui::exit_list", "crates/koru-ui/src/exit_list.rs"),
        DESKTOP_SHARED,
        &[support("koru_ui::exit_list::ExitList")],
    ),
    record(
        "C-TABLE",
        "KoruTable",
        Key::UiLabTableName,
        Key::UiLabTablePurpose,
        Kind::Widget,
        C_LIST,
        &[],
        ui("koru_ui::table", "crates/koru-ui/src/table.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::table::Table"),
            widget_with("koru_ui::table::Table::paint_row_fill", &[ROW_STATE]),
            support("koru_ui::table::Metrics"),
            support("koru_ui::table::Column"),
            support("koru_ui::table::Band"),
        ],
    ),
    record(
        "C-NAV-LIST",
        "KoruNavList",
        Key::UiLabNavListName,
        Key::UiLabNavListPurpose,
        Kind::Widget,
        C_LIST,
        &["KoruNavList"],
        ui("koru_ui::nav_list", "crates/koru-ui/src/nav_list.rs"),
        SETTINGS_DESKTOP,
        &[
            widget("koru_ui::nav_list::NavList"),
            widget_with("koru_ui::nav_list::NavItem", &[NAV_LEADING]),
        ],
    ),
    record(
        "C-CHIP-NAV",
        "KoruChipNav",
        Key::UiLabChipNavName,
        Key::UiLabChipNavPurpose,
        Kind::Widget,
        C_LIST,
        &["KoruChipNav"],
        ui("koru_ui::nav_list", "crates/koru-ui/src/nav_list.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::nav_list::ChipNav")],
    ),
    record(
        "C-SCROLLBAR",
        "KoruScrollbar",
        Key::UiLabScrollbarName,
        Key::UiLabScrollbarPurpose,
        Kind::Widget,
        C_LIST,
        &[],
        Source {
            owner: "koru-draw",
            module: "koru_draw::scrollbar",
            file: "crates/koru-draw/src/scrollbar.rs",
        },
        DESKTOP_SHARED,
        &[
            widget("koru_draw::scrollbar::draw"),
            widget("koru_draw::scrollbar::draw_thumb"),
            widget("koru_draw::scrollbar::draw_thumb_in"),
            support("koru_draw::scrollbar::ScrollbarGeom"),
        ],
    ),
];

const OVERLAY_RECORDS: [Entry; 11] = [
    record(
        "C-POPOVER",
        "KoruPopover",
        Key::UiLabPopoverName,
        Key::UiLabPopoverPurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruPopover"],
        ui("koru_ui::popover", "crates/koru-ui/src/popover.rs"),
        DESKTOP_SHARED,
        &[
            widget_with("koru_ui::popover::PopoverShell", &[POPOVER_ALIGN]),
            widget("koru_ui::popover::PopoverShell::draw_with_content"),
            support("koru_ui::popover::KoruPopoverInspection"),
        ],
    ),
    record(
        "C-DIALOG-FRAME",
        "KoruDialogFrame",
        Key::UiLabDialogFrameName,
        Key::UiLabDialogFramePurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruDialogFrame"],
        ui(
            "koru_ui::dialog_frame",
            "crates/koru-ui/src/dialog_frame.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::dialog_frame::KoruDialogFrame"),
            support("koru_ui::dialog_frame::KoruDialogFrameInspection"),
        ],
    ),
    record(
        "C-MODAL",
        "KoruModal",
        Key::UiLabModalName,
        Key::UiLabModalPurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruModal"],
        ui("koru_ui::modal", "crates/koru-ui/src/modal.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::modal::draw")],
    ),
    record(
        "C-PLACE-CHOOSER",
        "KoruPlaceChooser",
        Key::UiLabPlaceChooserName,
        Key::UiLabPlaceChooserPurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruPlaceChooser"],
        ui(
            "koru_ui::place_chooser",
            "crates/koru-ui/src/place_chooser.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::place_chooser::PlaceChooser"),
            support("koru_ui::place_chooser::Place"),
        ],
    ),
    record(
        "C-PROMPT",
        "KoruPrompt",
        Key::UiLabPromptName,
        Key::UiLabPromptPurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruPrompt"],
        ui("koru_ui::prompt", "crates/koru-ui/src/prompt.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::prompt::KoruPrompt"),
            support("koru_ui::prompt::KoruPromptAction"),
        ],
    ),
    record(
        "C-MENU",
        "KoruMenu",
        Key::UiLabMenuName,
        Key::UiLabMenuPurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruMenu"],
        ui("koru_ui::menu", "crates/koru-ui/src/menu.rs"),
        DESKTOP_SHARED,
        &[
            widget_with("koru_ui::menu::Menu", &[MENU_ANCHOR]),
            widget("koru_ui::menu::Menu::draw_with_inspection"),
            widget("koru_ui::menu::MenuCard"),
            support("koru_ui::menu::KoruMenuInspection"),
            support("koru_ui::menu::measure_width"),
        ],
    ),
    record(
        "C-MENU-ITEM",
        "KoruMenuItem",
        Key::UiLabMenuItemName,
        Key::UiLabMenuItemPurpose,
        Kind::Widget,
        C_OVERLAY,
        &["KoruMenuItem"],
        ui("koru_ui::menu", "crates/koru-ui/src/menu/inspection.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::menu::MenuItem"),
            widget_with("koru_ui::menu::Control", &[MENU_CONTROL]),
        ],
    ),
    record(
        "C-CONFIRM",
        "KoruConfirm",
        Key::UiLabConfirmName,
        Key::UiLabConfirmPurpose,
        Kind::Widget,
        C_OVERLAY,
        &[],
        ui("koru_ui::confirm", "crates/koru-ui/src/confirm.rs"),
        DESKTOP_SHARED,
        &[
            widget_with("koru_ui::confirm::Confirm", &[CONFIRM_SHAPE]),
            widget("koru_ui::confirm::Confirm::with_alternative"),
            support("koru_ui::confirm::Outcome"),
        ],
    ),
    record(
        "C-CHOICE-DIALOG",
        "KoruChoice",
        Key::UiLabChoiceName,
        Key::UiLabChoicePurpose,
        Kind::Widget,
        C_CHOICE,
        &[],
        ui("koru_ui::choice", "crates/koru-ui/src/choice.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::choice::Choice"),
            support("koru_ui::choice::ChoiceOutcome"),
        ],
    ),
    record(
        "C-TOOLTIP",
        "KoruTooltip",
        Key::UiLabTooltipName,
        Key::UiLabTooltipPurpose,
        Kind::Widget,
        C_OVERLAY,
        &[],
        ui("koru_ui::tooltip", "crates/koru-ui/src/tooltip.rs"),
        DESKTOP_SHARED,
        &[
            widget_with("koru_ui::tooltip::Tooltip", &[TOOLTIP_PLACEMENT]),
            support("koru_ui::tooltip::TooltipContent"),
        ],
    ),
    record(
        "C-TOAST",
        "KoruToast",
        Key::UiLabToastName,
        Key::UiLabToastPurpose,
        Kind::Widget,
        C_OVERLAY,
        &[],
        ui("koru_ui::toast", "crates/koru-ui/src/toast.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::toast::Toast"),
            support("koru_ui::toast::Outcome"),
        ],
    ),
];

const STATUS_RECORDS: [Entry; 4] = [
    record(
        "C-PROGRESS-BAR",
        "KoruProgressBar",
        Key::UiLabProgressBarName,
        Key::UiLabProgressBarPurpose,
        Kind::Widget,
        Some("C-STATUS"),
        &[],
        ui(
            "koru_ui::progress_bar",
            "crates/koru-ui/src/progress_bar.rs",
        ),
        DESKTOP_SHARED,
        &[widget_with(
            "koru_ui::progress_bar::ProgressBar",
            &[PROGRESS_MODE],
        )],
    ),
    record(
        "C-PROGRESS-RING",
        "ProgressRing",
        Key::UiLabProgressRingName,
        Key::UiLabProgressRingPurpose,
        Kind::Widget,
        None,
        &[],
        ui(
            "koru_ui::progress_ring",
            "crates/koru-ui/src/progress_ring.rs",
        ),
        DESKTOP_SHARED,
        &[widget("koru_ui::progress_ring::ProgressRing")],
    ),
    record(
        "C-PULSING-DOTS",
        "KoruPulsingDots",
        Key::UiLabPulsingDotsName,
        Key::UiLabPulsingDotsPurpose,
        Kind::Widget,
        Some("C-STATUS"),
        &[],
        ui(
            "koru_ui::pulsing_dots",
            "crates/koru-ui/src/pulsing_dots.rs",
        ),
        DESKTOP_SHARED,
        &[widget("koru_ui::pulsing_dots::PulsingDots")],
    ),
    record(
        "C-MEASUREMENT-BADGE",
        "KoruMeasurementBadge",
        Key::UiLabMeasurementBadgeName,
        Key::UiLabMeasurementBadgePurpose,
        Kind::Widget,
        Some("C-STATUS"),
        &["KoruMeasurementBadge"],
        ui(
            "koru_ui::measurement_badge",
            "crates/koru-ui/src/measurement_badge.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::measurement_badge::KoruMeasurementBadge"),
            support("koru_ui::measurement_badge::KoruMeasurementGeometry"),
        ],
    ),
];

const TEXT_RECORDS: [Entry; 9] = [
    record(
        "C-CARD",
        "KoruCard",
        Key::UiLabCardName,
        Key::UiLabCardPurpose,
        Kind::Widget,
        Some("C-LAYOUT"),
        &["KoruCard"],
        ui("koru_ui::card", "crates/koru-ui/src/card.rs"),
        SETTINGS_DESKTOP,
        &[widget("koru_ui::card::Card")],
    ),
    record(
        "C-CARD-ROW",
        "KoruCardRow",
        Key::UiLabCardRowName,
        Key::UiLabCardRowPurpose,
        Kind::Widget,
        Some("C-LAYOUT"),
        &["KoruCardRow"],
        ui("koru_ui::card", "crates/koru-ui/src/card.rs"),
        SETTINGS_DESKTOP,
        &[widget("koru_ui::card::CardRow")],
    ),
    record(
        "C-DIVIDER",
        "KoruDivider",
        Key::UiLabDividerLaneName,
        Key::UiLabDividerLanePurpose,
        Kind::Widget,
        C_CHROME,
        &[],
        ui("koru_ui::divider", "crates/koru-ui/src/divider.rs"),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::divider::draw_vertical"),
            support("koru_ui::divider::VERTICAL_LANE_W"),
        ],
    ),
    record(
        "C-FILE-ICON",
        "KoruFileIcon",
        Key::UiLabFileIconName,
        Key::UiLabFileIconPurpose,
        Kind::Support,
        C_CHROME,
        &[],
        ui("koru_ui::file_icon", "crates/koru-ui/src/file_icon.rs"),
        DESKTOP_SHARED,
        &[
            support("koru_ui::file_icon::FileIcon"),
            support("koru_ui::file_icon::glyph"),
        ],
    ),
    record(
        "C-FONT-PREVIEW",
        "KoruFontPreview",
        Key::UiLabFontPreviewName,
        Key::UiLabFontPreviewPurpose,
        Kind::Widget,
        C_CHROME,
        &["KoruFontPreview"],
        ui(
            "koru_ui::font_preview",
            "crates/koru-ui/src/font_preview.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::font_preview::FontPreview"),
            support("koru_ui::font_preview::FontSample"),
        ],
    ),
    record(
        "C-HIGHLIGHTED-TEXT",
        "KoruHighlightedText",
        Key::UiLabHighlightedTextName,
        Key::UiLabHighlightedTextPurpose,
        Kind::Widget,
        C_CHROME,
        &[],
        ui(
            "koru_ui::highlighted_text",
            "crates/koru-ui/src/highlighted_text.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::highlighted_text::draw_highlighted_line"),
            support("koru_ui::highlighted_text::Span"),
            support("koru_ui::highlighted_text::valid_spans"),
            support("koru_ui::highlighted_text::elide_around_spans"),
        ],
    ),
    record(
        "C-LOGO",
        "KoruLogo",
        Key::UiLabLogoName,
        Key::UiLabLogoPurpose,
        Kind::Widget,
        C_CHROME,
        &["KoruLogo"],
        ui("koru_ui::logo", "crates/koru-ui/src/logo.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::logo::KoruLogo")],
    ),
    record(
        "C-TEXT-PARAGRAPH",
        "KoruTextParagraph",
        Key::UiLabTextParagraphName,
        Key::UiLabTextParagraphPurpose,
        Kind::Widget,
        C_CHROME,
        &["KoruTextParagraph"],
        ui(
            "koru_ui::text_paragraph",
            "crates/koru-ui/src/text_paragraph.rs",
        ),
        DESKTOP_SHARED,
        &[
            widget("koru_ui::text_paragraph::KoruTextParagraph"),
            widget("koru_ui::text_paragraph::draw_line"),
        ],
    ),
    record(
        "C-TEXT-TITLE",
        "KoruTextTitle",
        Key::UiLabTextTitleName,
        Key::UiLabTextTitlePurpose,
        Kind::Widget,
        C_CHROME,
        &["KoruTextTitle"],
        ui("koru_ui::text_title", "crates/koru-ui/src/text_title.rs"),
        DESKTOP_SHARED,
        &[widget("koru_ui::text_title::KoruTextTitle")],
    ),
];

const SUPPORT_RECORDS: [Entry; 5] = [
    record(
        "H-CLIPBOARD",
        "koru_ui::clipboard",
        Key::UiLabClipboardName,
        Key::UiLabClipboardPurpose,
        Kind::Support,
        H_INPUT,
        &[],
        ui("koru_ui::clipboard", "crates/koru-ui/src/clipboard.rs"),
        DESKTOP_SHARED,
        &[
            support("koru_ui::clipboard::put_plain"),
            support("koru_ui::clipboard::plain"),
            support("koru_ui::clipboard::one_line"),
        ],
    ),
    record(
        "H-CURSOR",
        "koru_ui::cursor",
        Key::UiLabCursorName,
        Key::UiLabCursorPurpose,
        Kind::Support,
        H_INPUT,
        &[],
        ui("koru_ui::cursor", "crates/koru-ui/src/cursor.rs"),
        DESKTOP_SHARED,
        &[
            support("koru_ui::cursor::PRESS"),
            support("koru_ui::cursor::IDLE"),
        ],
    ),
    record(
        "H-KEYS",
        "koru_ui::keys",
        Key::UiLabKeysName,
        Key::UiLabKeysPurpose,
        Kind::Support,
        H_INPUT,
        &[],
        ui("koru_ui::keys", "crates/koru-ui/src/keys.rs"),
        DESKTOP_SHARED,
        &[
            support("koru_ui::keys::ctrl_letter"),
            support("koru_ui::keys::recover_modified_editing_key"),
            support("koru_ui::keys::debug"),
        ],
    ),
    record(
        "H-DICTATION",
        "koru_ui::dictation",
        Key::UiLabDictationName,
        Key::UiLabDictationPurpose,
        Kind::Support,
        H_INPUT,
        &[],
        ui("koru_ui::dictation", "crates/koru-ui/src/dictation.rs"),
        DESKTOP_SHARED,
        &[
            support("koru_ui::dictation::DictationTarget"),
            plain_with(
                "koru_ui::dictation::ComposeInto",
                &[COMPOSE_INTO],
                Story::Nonvisual,
            ),
            support("koru_ui::dictation::HeldComposition"),
        ],
    ),
    record(
        "H-DATES",
        "koru_ui::dates",
        Key::UiLabDatesName,
        Key::UiLabDatesPurpose,
        Kind::Support,
        None,
        &[],
        ui("koru_ui::dates", "crates/koru-ui/src/dates.rs"),
        DESKTOP_SHARED,
        &[
            support("koru_ui::dates::month_start"),
            support("koru_ui::dates::days_in_month"),
            support("koru_ui::dates::shift_month"),
            support("koru_ui::dates::month_abbr"),
            support("koru_ui::dates::weekday_abbr"),
            support("koru_ui::dates::date_label"),
        ],
    ),
];

/// Every control record, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &{
    let mut all = [BUTTON_ENTRY; 57];
    let mut at = 1;
    let groups: [&[Entry]; 8] = [
        &ACTION_RECORDS,
        &CHOICE_RECORDS,
        &INPUT_RECORDS,
        &LIST_RECORDS,
        &OVERLAY_RECORDS,
        &STATUS_RECORDS,
        &TEXT_RECORDS,
        &SUPPORT_RECORDS,
    ];
    let mut group = 0;
    while group < groups.len() {
        let mut i = 0;
        while i < groups[group].len() {
            all[at] = groups[group][i];
            at += 1;
            i += 1;
        }
        group += 1;
    }
    assert!(
        at == all.len(),
        "the control count is every group's and the Button"
    );
    all
};
