// SPDX-License-Identifier: MPL-2.0

//! **The drift checks**: the registration held to the shared source it was
//! written from. Each production file below is compiled in as text, so the
//! checks read nothing at run time and fail the build's tests the moment a
//! module, token, inspector identity or option is added or removed upstream
//! without a record saying what it is.
//!
//! What these checks cover is bounded and named: the `pub mod` lists of
//! `koru-ui` and `koru-draw` (and `koru-draw::theme`), the module lists of
//! the domain crates' roots ([`domain`]) and of the app files that declare
//! modules ([`app`]), the token tables, the component identities painters
//! register in any of those sources, the option enums and constants a record
//! lists, the symbols each record names, the A2UI composition catalog and
//! the fixed catalog's search host, the counts and platform tables, and the
//! native platforms' composables, views and entry points ([`native`]). They
//! do not discover a painter that registers no identity, a new public item
//! inside a module already classified, or a new native file; that is the
//! manifest's hand-reviewed scope.

use super::*;
use koru_ai_core::mini_apps::composition as model;

/// Every non-test file of `koru-ui` and the `koru-draw` files the records
/// name, from the repository root.
const SOURCES: &[(&str, &str)] = &[
    (
        "crates/koru-ui/src/badge.rs",
        include_str!("../../../../../../crates/koru-ui/src/badge.rs"),
    ),
    (
        "crates/koru-ui/src/button.rs",
        include_str!("../../../../../../crates/koru-ui/src/button.rs"),
    ),
    (
        "crates/koru-ui/src/button_group.rs",
        include_str!("../../../../../../crates/koru-ui/src/button_group.rs"),
    ),
    (
        "crates/koru-ui/src/caption.rs",
        include_str!("../../../../../../crates/koru-ui/src/caption.rs"),
    ),
    (
        "crates/koru-ui/src/card.rs",
        include_str!("../../../../../../crates/koru-ui/src/card.rs"),
    ),
    (
        "crates/koru-ui/src/chip.rs",
        include_str!("../../../../../../crates/koru-ui/src/chip.rs"),
    ),
    (
        "crates/koru-ui/src/choice.rs",
        include_str!("../../../../../../crates/koru-ui/src/choice.rs"),
    ),
    (
        "crates/koru-ui/src/clipboard.rs",
        include_str!("../../../../../../crates/koru-ui/src/clipboard.rs"),
    ),
    (
        "crates/koru-ui/src/color_picker_row.rs",
        include_str!("../../../../../../crates/koru-ui/src/color_picker_row.rs"),
    ),
    (
        "crates/koru-ui/src/confirm.rs",
        include_str!("../../../../../../crates/koru-ui/src/confirm.rs"),
    ),
    (
        "crates/koru-ui/src/context_chip.rs",
        include_str!("../../../../../../crates/koru-ui/src/context_chip.rs"),
    ),
    (
        "crates/koru-ui/src/copy_affordance.rs",
        include_str!("../../../../../../crates/koru-ui/src/copy_affordance.rs"),
    ),
    (
        "crates/koru-ui/src/cursor.rs",
        include_str!("../../../../../../crates/koru-ui/src/cursor.rs"),
    ),
    (
        "crates/koru-ui/src/date_grid.rs",
        include_str!("../../../../../../crates/koru-ui/src/date_grid.rs"),
    ),
    (
        "crates/koru-ui/src/dates.rs",
        include_str!("../../../../../../crates/koru-ui/src/dates.rs"),
    ),
    (
        "crates/koru-ui/src/dialog_frame.rs",
        include_str!("../../../../../../crates/koru-ui/src/dialog_frame.rs"),
    ),
    (
        "crates/koru-ui/src/dictation.rs",
        include_str!("../../../../../../crates/koru-ui/src/dictation.rs"),
    ),
    (
        "crates/koru-ui/src/divider.rs",
        include_str!("../../../../../../crates/koru-ui/src/divider.rs"),
    ),
    (
        "crates/koru-ui/src/dropdown.rs",
        include_str!("../../../../../../crates/koru-ui/src/dropdown.rs"),
    ),
    (
        "crates/koru-ui/src/exit_list.rs",
        include_str!("../../../../../../crates/koru-ui/src/exit_list.rs"),
    ),
    (
        "crates/koru-ui/src/file_icon.rs",
        include_str!("../../../../../../crates/koru-ui/src/file_icon.rs"),
    ),
    (
        "crates/koru-ui/src/font_preview.rs",
        include_str!("../../../../../../crates/koru-ui/src/font_preview.rs"),
    ),
    (
        "crates/koru-ui/src/highlighted_text.rs",
        include_str!("../../../../../../crates/koru-ui/src/highlighted_text.rs"),
    ),
    (
        "crates/koru-ui/src/icon_button.rs",
        include_str!("../../../../../../crates/koru-ui/src/icon_button.rs"),
    ),
    (
        "crates/koru-ui/src/key_chip.rs",
        include_str!("../../../../../../crates/koru-ui/src/key_chip.rs"),
    ),
    (
        "crates/koru-ui/src/keys.rs",
        include_str!("../../../../../../crates/koru-ui/src/keys.rs"),
    ),
    (
        "crates/koru-ui/src/label_chip.rs",
        include_str!("../../../../../../crates/koru-ui/src/label_chip.rs"),
    ),
    (
        "crates/koru-ui/src/launcher.rs",
        include_str!("../../../../../../crates/koru-ui/src/launcher.rs"),
    ),
    (
        "crates/koru-ui/src/lib.rs",
        include_str!("../../../../../../crates/koru-ui/src/lib.rs"),
    ),
    (
        "crates/koru-ui/src/logo.rs",
        include_str!("../../../../../../crates/koru-ui/src/logo.rs"),
    ),
    (
        "crates/koru-ui/src/measurement_badge.rs",
        include_str!("../../../../../../crates/koru-ui/src/measurement_badge.rs"),
    ),
    (
        "crates/koru-ui/src/media_viewer.rs",
        include_str!("../../../../../../crates/koru-ui/src/media_viewer.rs"),
    ),
    (
        "crates/koru-ui/src/menu.rs",
        include_str!("../../../../../../crates/koru-ui/src/menu.rs"),
    ),
    (
        "crates/koru-ui/src/modal.rs",
        include_str!("../../../../../../crates/koru-ui/src/modal.rs"),
    ),
    (
        "crates/koru-ui/src/nav_list.rs",
        include_str!("../../../../../../crates/koru-ui/src/nav_list.rs"),
    ),
    (
        "crates/koru-ui/src/page_shell.rs",
        include_str!("../../../../../../crates/koru-ui/src/page_shell.rs"),
    ),
    (
        "crates/koru-ui/src/place_chooser.rs",
        include_str!("../../../../../../crates/koru-ui/src/place_chooser.rs"),
    ),
    (
        "crates/koru-ui/src/popover.rs",
        include_str!("../../../../../../crates/koru-ui/src/popover.rs"),
    ),
    (
        "crates/koru-ui/src/preview_tile.rs",
        include_str!("../../../../../../crates/koru-ui/src/preview_tile.rs"),
    ),
    (
        "crates/koru-ui/src/profile_picker.rs",
        include_str!("../../../../../../crates/koru-ui/src/profile_picker.rs"),
    ),
    (
        "crates/koru-ui/src/progress_bar.rs",
        include_str!("../../../../../../crates/koru-ui/src/progress_bar.rs"),
    ),
    (
        "crates/koru-ui/src/progress_ring.rs",
        include_str!("../../../../../../crates/koru-ui/src/progress_ring.rs"),
    ),
    (
        "crates/koru-ui/src/prompt.rs",
        include_str!("../../../../../../crates/koru-ui/src/prompt.rs"),
    ),
    (
        "crates/koru-ui/src/pulsing_dots.rs",
        include_str!("../../../../../../crates/koru-ui/src/pulsing_dots.rs"),
    ),
    (
        "crates/koru-ui/src/scrollbar.rs",
        include_str!("../../../../../../crates/koru-ui/src/scrollbar.rs"),
    ),
    (
        "crates/koru-ui/src/segmented.rs",
        include_str!("../../../../../../crates/koru-ui/src/segmented.rs"),
    ),
    (
        "crates/koru-ui/src/selectable_list.rs",
        include_str!("../../../../../../crates/koru-ui/src/selectable_list.rs"),
    ),
    (
        "crates/koru-ui/src/split_button.rs",
        include_str!("../../../../../../crates/koru-ui/src/split_button.rs"),
    ),
    (
        "crates/koru-ui/src/stepper.rs",
        include_str!("../../../../../../crates/koru-ui/src/stepper.rs"),
    ),
    (
        "crates/koru-ui/src/swatch.rs",
        include_str!("../../../../../../crates/koru-ui/src/swatch.rs"),
    ),
    (
        "crates/koru-ui/src/table.rs",
        include_str!("../../../../../../crates/koru-ui/src/table.rs"),
    ),
    (
        "crates/koru-ui/src/text_field.rs",
        include_str!("../../../../../../crates/koru-ui/src/text_field.rs"),
    ),
    (
        "crates/koru-ui/src/text_input.rs",
        include_str!("../../../../../../crates/koru-ui/src/text_input.rs"),
    ),
    (
        "crates/koru-ui/src/text_paragraph.rs",
        include_str!("../../../../../../crates/koru-ui/src/text_paragraph.rs"),
    ),
    (
        "crates/koru-ui/src/text_title.rs",
        include_str!("../../../../../../crates/koru-ui/src/text_title.rs"),
    ),
    (
        "crates/koru-ui/src/toast.rs",
        include_str!("../../../../../../crates/koru-ui/src/toast.rs"),
    ),
    (
        "crates/koru-ui/src/toggle.rs",
        include_str!("../../../../../../crates/koru-ui/src/toggle.rs"),
    ),
    (
        "crates/koru-ui/src/tooltip.rs",
        include_str!("../../../../../../crates/koru-ui/src/tooltip.rs"),
    ),
    (
        "crates/koru-ui/src/tree_drag.rs",
        include_str!("../../../../../../crates/koru-ui/src/tree_drag.rs"),
    ),
    (
        "crates/koru-ui/src/typography.rs",
        include_str!("../../../../../../crates/koru-ui/src/typography.rs"),
    ),
    (
        "crates/koru-ui/src/welcome.rs",
        include_str!("../../../../../../crates/koru-ui/src/welcome.rs"),
    ),
    (
        "crates/koru-ui/src/menu/inspection.rs",
        include_str!("../../../../../../crates/koru-ui/src/menu/inspection.rs"),
    ),
    (
        "crates/koru-ui/src/popover/inspection.rs",
        include_str!("../../../../../../crates/koru-ui/src/popover/inspection.rs"),
    ),
    (
        "crates/koru-ui/src/keys/debug.rs",
        include_str!("../../../../../../crates/koru-ui/src/keys/debug.rs"),
    ),
    (
        "crates/koru-draw/src/anim.rs",
        include_str!("../../../../../../crates/koru-draw/src/anim.rs"),
    ),
    (
        "crates/koru-draw/src/brand.rs",
        include_str!("../../../../../../crates/koru-draw/src/brand.rs"),
    ),
    (
        "crates/koru-draw/src/checkbox.rs",
        include_str!("../../../../../../crates/koru-draw/src/checkbox.rs"),
    ),
    (
        "crates/koru-draw/src/chip.rs",
        include_str!("../../../../../../crates/koru-draw/src/chip.rs"),
    ),
    (
        "crates/koru-draw/src/diagram.rs",
        include_str!("../../../../../../crates/koru-draw/src/diagram.rs"),
    ),
    (
        "crates/koru-draw/src/el.rs",
        include_str!("../../../../../../crates/koru-draw/src/el.rs"),
    ),
    (
        "crates/koru-draw/src/emoji.rs",
        include_str!("../../../../../../crates/koru-draw/src/emoji.rs"),
    ),
    (
        "crates/koru-draw/src/geometry.rs",
        include_str!("../../../../../../crates/koru-draw/src/geometry.rs"),
    ),
    (
        "crates/koru-draw/src/icons.rs",
        include_str!("../../../../../../crates/koru-draw/src/icons.rs"),
    ),
    (
        "crates/koru-draw/src/image.rs",
        include_str!("../../../../../../crates/koru-draw/src/image.rs"),
    ),
    (
        "crates/koru-draw/src/inspect.rs",
        include_str!("../../../../../../crates/koru-draw/src/inspect.rs"),
    ),
    (
        "crates/koru-draw/src/installed_fonts.rs",
        include_str!("../../../../../../crates/koru-draw/src/installed_fonts.rs"),
    ),
    (
        "crates/koru-draw/src/lib.rs",
        include_str!("../../../../../../crates/koru-draw/src/lib.rs"),
    ),
    (
        "crates/koru-draw/src/pill.rs",
        include_str!("../../../../../../crates/koru-draw/src/pill.rs"),
    ),
    (
        "crates/koru-draw/src/scene.rs",
        include_str!("../../../../../../crates/koru-draw/src/scene.rs"),
    ),
    (
        "crates/koru-draw/src/scrollbar.rs",
        include_str!("../../../../../../crates/koru-draw/src/scrollbar.rs"),
    ),
    (
        "crates/koru-draw/src/text.rs",
        include_str!("../../../../../../crates/koru-draw/src/text.rs"),
    ),
    (
        "crates/koru-draw/src/theme.rs",
        include_str!("../../../../../../crates/koru-draw/src/theme.rs"),
    ),
    (
        "crates/koru-draw/src/tile.rs",
        include_str!("../../../../../../crates/koru-draw/src/tile.rs"),
    ),
    (
        "crates/koru-draw/src/tween.rs",
        include_str!("../../../../../../crates/koru-draw/src/tween.rs"),
    ),
    (
        "crates/koru-draw/src/el/frame.rs",
        include_str!("../../../../../../crates/koru-draw/src/el/frame.rs"),
    ),
    (
        "crates/koru-draw/src/icons/vector.rs",
        include_str!("../../../../../../crates/koru-draw/src/icons/vector.rs"),
    ),
    (
        "crates/koru-draw/src/theme/tokens.rs",
        include_str!("../../../../../../crates/koru-draw/src/theme/tokens.rs"),
    ),
    (
        "crates/koru-draw/src/theme/themes.rs",
        include_str!("../../../../../../crates/koru-draw/src/theme/themes.rs"),
    ),
    (
        "crates/koru-draw/src/theme/motion.rs",
        include_str!("../../../../../../crates/koru-draw/src/theme/motion.rs"),
    ),
    (
        "crates/koru-draw/src/theme/overlay.rs",
        include_str!("../../../../../../crates/koru-draw/src/theme/overlay.rs"),
    ),
    (
        "crates/koru-draw/src/theme/preview.rs",
        include_str!("../../../../../../crates/koru-draw/src/theme/preview.rs"),
    ),
];

/// The adapter that draws a composition, and the catalog it validates.
const COMPOSITION_HOST: &str = include_str!("../../../mini_app/composition.rs");
/// The fixed catalog's component vocabulary, and the mini-app chrome that
/// hosts its search.
const FIXED_CATALOG: &str = include_str!("../../../../../../crates/koru-ai-core/src/mini_apps.rs");
const MINI_APP_CHROME: &str = include_str!("../../../mini_app/chrome.rs");
const COMPOSITION_MODEL: &str =
    include_str!("../../../../../../crates/koru-ai-core/src/mini_apps/composition.rs");

/// The registry's own guide to what it covers, with the counts this file
/// keeps true.
const MANIFEST: &str = include_str!("../MANIFEST.md");

/// Every compiled-in source: the shared crates', the domain crates' and the
/// apps'.
fn all_sources() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    SOURCES.iter().chain(domain::SOURCES).chain(app::SOURCES)
}

fn source(path: &str) -> &'static str {
    all_sources()
        .find(|(file, _)| *file == path)
        .map(|(_, text)| *text)
        .unwrap_or_else(|| panic!("{path} is not compiled into the drift checks"))
}

/// The lines of `text` that are code rather than comments.
fn code(text: &'static str) -> impl Iterator<Item = &'static str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
}

/// The modules a file declares, public and private, in declaration order,
/// leaving out the test modules beside it.
fn declared_modules(text: &'static str) -> Vec<(&'static str, bool)> {
    code(text)
        .filter_map(|line| {
            let (public, rest) = match line.strip_prefix("pub mod ") {
                Some(rest) => (true, rest),
                None => (false, line.strip_prefix("mod ")?),
            };
            let name = rest.strip_suffix(';')?;
            (!name.ends_with("tests")).then_some((name, public))
        })
        .collect()
}

/// How the baseline accounts for one module of a crate root.
#[derive(Debug)]
enum Module {
    /// These records own what it exports.
    Records(&'static [&'static str]),
    /// It is not a catalogue entry, for this reason.
    Excluded(&'static str),
}

/// `crates/koru-ui/src/lib.rs`, module by module: the registration
/// baseline. A module added or removed upstream fails
/// [`every_koru_ui_module_is_classified`] until it is classified here.
const KORU_UI_MODULES: &[(&str, Module)] = &[
    ("badge", Module::Records(&["C-BADGE"])),
    ("button", Module::Records(&["C-BUTTON"])),
    ("button_group", Module::Records(&["C-BUTTON-GROUP"])),
    ("caption", Module::Records(&["C-CAPTION"])),
    ("card", Module::Records(&["C-CARD", "C-CARD-ROW"])),
    ("chip", Module::Records(&["C-CHIP"])),
    ("choice", Module::Records(&["C-CHOICE-DIALOG"])),
    ("clipboard", Module::Records(&["H-CLIPBOARD"])),
    ("color_picker_row", Module::Records(&["C-COLOR-PICKER-ROW"])),
    ("confirm", Module::Records(&["C-CONFIRM"])),
    ("context_chip", Module::Records(&["C-CONTEXT-CHIP"])),
    ("copy_affordance", Module::Records(&["C-COPY"])),
    ("cursor", Module::Records(&["H-CURSOR"])),
    ("date_grid", Module::Records(&["C-DATE-GRID"])),
    ("dates", Module::Records(&["H-DATES"])),
    ("dialog_frame", Module::Records(&["C-DIALOG-FRAME"])),
    ("dictation", Module::Records(&["H-DICTATION"])),
    ("divider", Module::Records(&["C-DIVIDER"])),
    ("dropdown", Module::Records(&["C-DROPDOWN"])),
    ("exit_list", Module::Records(&["C-EXIT-LIST"])),
    ("file_icon", Module::Records(&["C-FILE-ICON"])),
    ("font_preview", Module::Records(&["C-FONT-PREVIEW"])),
    ("highlighted_text", Module::Records(&["C-HIGHLIGHTED-TEXT"])),
    ("icon_button", Module::Records(&["C-ICON-BUTTON"])),
    ("key_chip", Module::Records(&["C-KEY-CHIP"])),
    ("keys", Module::Records(&["H-KEYS"])),
    ("label_chip", Module::Records(&["C-LABEL-CHIP"])),
    ("launcher", Module::Records(&["C-LAUNCHER"])),
    ("logo", Module::Records(&["C-LOGO"])),
    (
        "measurement_badge",
        Module::Records(&["C-MEASUREMENT-BADGE"]),
    ),
    ("media_viewer", Module::Records(&["C-MEDIA-VIEWER"])),
    ("menu", Module::Records(&["C-MENU", "C-MENU-ITEM"])),
    ("modal", Module::Records(&["C-MODAL"])),
    ("nav_list", Module::Records(&["C-NAV-LIST", "C-CHIP-NAV"])),
    ("page_shell", Module::Records(&["C-PAGE-SHELL"])),
    ("place_chooser", Module::Records(&["C-PLACE-CHOOSER"])),
    ("popover", Module::Records(&["C-POPOVER"])),
    ("preview_tile", Module::Records(&["C-PREVIEW-TILE"])),
    ("profile_picker", Module::Records(&["C-PROFILE-PICKER"])),
    ("progress_bar", Module::Records(&["C-PROGRESS-BAR"])),
    ("progress_ring", Module::Records(&["C-PROGRESS-RING"])),
    ("prompt", Module::Records(&["C-PROMPT"])),
    ("pulsing_dots", Module::Records(&["C-PULSING-DOTS"])),
    // A re-export: the bar is `koru_draw::scrollbar`, catalogued there.
    ("scrollbar", Module::Records(&["C-SCROLLBAR"])),
    ("segmented", Module::Records(&["C-SEGMENTED"])),
    ("selectable_list", Module::Records(&["C-SELECTABLE-LIST"])),
    ("split_button", Module::Records(&["C-SPLIT-BUTTON"])),
    ("stepper", Module::Records(&["C-STEPPER"])),
    ("swatch", Module::Records(&["C-SWATCH"])),
    ("table", Module::Records(&["C-TABLE"])),
    (
        "text_field",
        Module::Records(&["C-TEXT-FIELD", "C-TEXT-AREA"]),
    ),
    ("text_input", Module::Records(&["C-TEXT-INPUT"])),
    ("text_paragraph", Module::Records(&["C-TEXT-PARAGRAPH"])),
    ("text_title", Module::Records(&["C-TEXT-TITLE"])),
    ("toast", Module::Records(&["C-TOAST"])),
    ("toggle", Module::Records(&["C-TOGGLE"])),
    ("tooltip", Module::Records(&["C-TOOLTIP"])),
    ("tree_drag", Module::Records(&["H-TREE-DRAG"])),
    (
        "typography",
        Module::Excluded(
            "private: the provenance helper shared painters record text through; nothing outside koru-ui can call it",
        ),
    ),
    ("welcome", Module::Records(&["C-WELCOME"])),
];

/// `crates/koru-draw/src/lib.rs`, module by module.
const KORU_DRAW_MODULES: &[(&str, Module)] = &[
    ("anim", Module::Records(&["H-ANIMATION"])),
    ("brand", Module::Records(&["P-BRAND"])),
    ("checkbox", Module::Records(&["P-CHECKBOX"])),
    ("chip", Module::Records(&["H-ATTRIBUTE-CHIP"])),
    ("diagram", Module::Records(&["P-DIAGRAM"])),
    (
        "el",
        Module::Records(&[
            "P-ROW",
            "P-COLUMN",
            "P-BOX",
            "P-SCROLL-Y",
            "P-TEXT",
            "P-ICON",
            "P-LEAF",
            "P-SPACER",
            "P-DIVIDER",
            "P-LAYOUT",
            "P-STATES",
            "H-ELEMENT-SEMANTICS",
            "H-ELEMENT-FRAME",
        ]),
    ),
    ("emoji", Module::Records(&["H-EMOJI"])),
    ("geometry", Module::Records(&["H-GEOMETRY"])),
    ("icons", Module::Records(&["P-ICONS"])),
    ("image", Module::Records(&["P-IMAGE"])),
    ("inspect", Module::Records(&["H-INSPECT"])),
    ("installed_fonts", Module::Records(&["H-INSTALLED-FONTS"])),
    ("pill", Module::Records(&["P-PILL"])),
    (
        "scene",
        Module::Records(&[
            "P-FLOATING-PANEL",
            "P-SHAPES",
            "P-TEXT-PAINT",
            "P-STATUS-BAR",
            "P-SMALL-MARKS",
        ]),
    ),
    ("scrollbar", Module::Records(&["C-SCROLLBAR"])),
    ("text", Module::Records(&["H-TEXT-ENGINE"])),
    (
        "theme",
        Module::Records(&[
            "T-PALETTE",
            "T-TYPE-SCALE",
            "T-ACCENT-CHOICES",
            "T-NAMED-COLORS",
            "T-TERMINAL-PALETTE",
            "T-APPEARANCE",
            "T-CARET",
            "T-FONTS",
            "T-DOCUMENT-TYPE",
            "T-LAYOUT-CONSTANTS",
        ]),
    ),
    ("tile", Module::Records(&["P-TILE"])),
    ("tween", Module::Records(&["H-ANIMATION"])),
];

/// `crates/koru-draw/src/theme.rs`'s own modules.
const THEME_MODULES: &[(&str, Module)] = &[
    ("motion", Module::Records(&["T-MOTION"])),
    ("overlay", Module::Records(&["T-POPOVER-GEOMETRY"])),
    ("preview", Module::Records(&["H-APPEARANCE-PREVIEW"])),
    ("themes", Module::Records(&["T-THEMES", "T-CONTRAST"])),
    (
        "tokens",
        Module::Records(&[
            "T-PROFILE",
            "T-SPACE",
            "T-RADIUS",
            "T-ICON-SIZE",
            "T-METRICS",
            "T-ALIASES",
            "T-TEXT-ROLES",
            "T-SURFACE",
            "T-INK",
            "T-LINE",
            "T-ACCENT",
        ]),
    ),
    (
        "token_guards",
        Module::Excluded("test-only guards on the token tables; no API"),
    ),
    (
        "ui_scan",
        Module::Excluded("test-only source scan of migrated surfaces; no API"),
    ),
];

fn entry(id: &str) -> &'static Entry {
    catalog()
        .find(|entry| entry.catalog_id == id)
        .unwrap_or_else(|| panic!("{id} is in a baseline and not in the catalog"))
}

/// The baseline matches the file's declarations name for name and in
/// order, and every record it names exists.
fn check_baseline(file: &str, baseline: &[(&str, Module)], public_only: bool) -> Vec<&'static str> {
    let declared: Vec<&str> = declared_modules(source(file))
        .into_iter()
        .filter(|(_, public)| *public || !public_only)
        .map(|(name, _)| name)
        .collect();
    let listed: Vec<&str> = baseline.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        declared, listed,
        "{file}'s modules changed: classify the new one or drop the removed one"
    );
    let mut ids = Vec::new();
    for (name, module) in baseline {
        match module {
            Module::Records(records) => {
                assert!(!records.is_empty(), "{file}: {name} names no record");
                for id in *records {
                    entry(id);
                    ids.push(*id);
                }
            }
            Module::Excluded(reason) => assert!(!reason.is_empty(), "{name}"),
        }
    }
    ids
}

#[test]
fn every_koru_ui_module_is_classified() {
    let ids = check_baseline("crates/koru-ui/src/lib.rs", KORU_UI_MODULES, false);
    let public = declared_modules(source("crates/koru-ui/src/lib.rs"))
        .iter()
        .filter(|(_, public)| *public)
        .count();
    assert_eq!(public, 59, "the inventory's baseline is 59 public modules");
    // Each record of the shared widgets sits in the module that names it,
    // and every koru-ui record is reached from a module.
    for (name, module) in KORU_UI_MODULES {
        if let Module::Records(records) = module {
            for id in *records {
                let owner = entry(id);
                let module = format!("koru_ui::{name}");
                assert!(
                    owner.source.module == module || owner.source.owner == "koru-draw",
                    "{id} is filed under {name} but names {}",
                    owner.source.module
                );
            }
        }
    }
    for record in catalog().filter(|e| e.source.owner == "koru-ui") {
        assert!(
            ids.contains(&record.catalog_id),
            "{} is a koru-ui record no module of lib.rs accounts for",
            record.catalog_id
        );
    }
    // The re-export really is one.
    assert!(
        source("crates/koru-ui/src/scrollbar.rs").contains("pub use koru_draw::scrollbar::"),
        "koru_ui::scrollbar is no longer a re-export of koru_draw::scrollbar"
    );
}

#[test]
fn every_koru_draw_module_is_classified() {
    let mut ids = check_baseline("crates/koru-draw/src/lib.rs", KORU_DRAW_MODULES, true);
    ids.extend(check_baseline(
        "crates/koru-draw/src/theme.rs",
        THEME_MODULES,
        false,
    ));
    for record in catalog().filter(|e| e.source.owner == "koru-draw") {
        assert!(
            ids.contains(&record.catalog_id),
            "{} is a koru-draw record no module accounts for",
            record.catalog_id
        );
    }
}

/// The token records list each table's members exactly, in the table's
/// order: the tables themselves are read, not their source text.
#[test]
fn the_token_records_are_the_token_tables() {
    use koru_draw::theme::{
        self, Accent, Contrast, IconSize, Ink, Line, Profile, Radius, Space, Surface, alias,
        metrics, roles,
    };
    fn members(symbol: &str, source: &str) -> Vec<&'static str> {
        let set = catalog()
            .flat_map(|e| e.symbols())
            .filter(|listed| listed.symbol == symbol)
            .flat_map(|listed| listed.options)
            .find(|set| set.source == source)
            .unwrap_or_else(|| panic!("no option set {source} on {symbol}"));
        set.members.iter().map(|m| m.ident).collect()
    }
    let own = |s: &'static str| -> &'static str { s.rsplit("::").next().unwrap() };
    assert_eq!(
        members("koru_draw::theme::Space", "koru_draw::theme::Space"),
        Space::ALL.map(Space::name)
    );
    assert_eq!(
        members("koru_draw::theme::Radius", "koru_draw::theme::Radius"),
        Radius::ALL.map(Radius::name)
    );
    assert_eq!(
        members("koru_draw::theme::IconSize", "koru_draw::theme::IconSize"),
        IconSize::ALL.map(IconSize::name)
    );
    assert_eq!(
        members("koru_draw::theme::metrics", "koru_draw::theme::metrics"),
        metrics::ALL.map(|m| m.name)
    );
    let roles: Vec<String> = roles::ALL.iter().map(|r| r.name.to_uppercase()).collect();
    assert_eq!(
        members("koru_draw::theme::roles", "koru_draw::theme::roles"),
        roles
    );
    for (symbol, all) in [
        (
            "koru_draw::theme::Surface",
            Surface::ALL
                .iter()
                .map(|s| own(s.name()))
                .collect::<Vec<_>>(),
        ),
        (
            "koru_draw::theme::Ink",
            Ink::ALL.iter().map(|s| own(s.name())).collect(),
        ),
        (
            "koru_draw::theme::Line",
            Line::ALL.iter().map(|s| own(s.name())).collect(),
        ),
        (
            "koru_draw::theme::Accent",
            Accent::ALL.iter().map(|s| own(s.name())).collect(),
        ),
    ] {
        assert_eq!(members(symbol, symbol), all, "{symbol}");
    }
    // The divider takes a line color: the same table.
    assert_eq!(
        members("koru_draw::el::divider", "koru_draw::theme::Line"),
        members("koru_draw::theme::Line", "koru_draw::theme::Line")
    );
    let profiles: Vec<String> = Profile::ALL.iter().map(|p| format!("{p:?}")).collect();
    assert_eq!(
        members("koru_draw::theme::Profile", "koru_draw::theme::Profile"),
        profiles
    );
    let contrasts: Vec<String> = Contrast::ALL.iter().map(|c| format!("{c:?}")).collect();
    assert_eq!(
        members("koru_draw::theme::Contrast", "koru_draw::theme::Contrast"),
        contrasts
    );
    // The test build registers the fixture theme beside the two production
    // ones (`fixture-theme` is a dev-dependency feature).
    let themes: Vec<&str> = theme::themes::registered().iter().map(|t| t.key).collect();
    assert_eq!(
        members(
            "koru_draw::theme::Theme",
            "koru_draw::theme::themes::registered"
        ),
        themes
    );
    assert_eq!(
        members(
            "koru_draw::theme::highlight_names",
            "koru_draw::theme::highlight_names"
        ),
        theme::highlight_names()
    );
    assert_eq!(
        members(
            "koru_draw::theme::workspace_color_names",
            "koru_draw::theme::workspace_color_names"
        ),
        theme::workspace_color_names()
    );
    assert_eq!(
        members(
            "koru_draw::theme::ACCENT_TOKENS",
            "koru_draw::theme::ACCENT_TOKENS"
        ),
        theme::ACCENT_TOKENS
    );
    // The aliases have no table; each is a value, named in its own `name`.
    assert_eq!(
        members("koru_draw::theme::alias", "koru_draw::theme::alias"),
        [alias::PANEL_PAD, alias::NAV_PAD, alias::ROW_PLATE_INSET].map(|a| a.name)
    );
}

/// The members of every enum an option set lists, read from the source;
/// constants and constructors by their declaration.
#[test]
fn every_option_set_is_its_source_declaration() {
    let mut checked = 0;
    for set in catalog()
        .flat_map(|e| e.symbols())
        .flat_map(|symbol| symbol.options)
        .copied()
        .chain([DIALOG_OPTIONS, COMPACT_OPTIONS])
    {
        let own = set.source.rsplit("::").next().unwrap();
        let idents: Vec<&str> = set.members.iter().map(|m| m.ident).collect();
        // A native enum is Kotlin or Swift: read in its own syntax.
        if native::is_native_file(set.file) {
            assert_eq!(set.declared, Declared::Enum, "{}", set.source);
            // `ui.theme.KoruThemePreference`, `SettingsScreen.Appearance`:
            // the enum is the last segment.
            let name = own.rsplit('.').next().unwrap();
            assert_eq!(
                idents,
                native::enum_members(set.file, name),
                "{} in {}",
                set.source,
                set.file
            );
            checked += 1;
            continue;
        }
        let text = source(set.file);
        // An app's types are rarely `pub`: read them whatever their visibility.
        if app::is_app_file(set.file) {
            match set.declared {
                Declared::Enum => assert_eq!(
                    idents,
                    app::enum_members(text, own),
                    "{} in {}",
                    set.source,
                    set.file
                ),
                Declared::Table => continue,
                declared => {
                    for ident in &idents {
                        assert!(
                            app::declares_value(text, ident, declared),
                            "{}: no {ident} in {}",
                            set.source,
                            set.file
                        );
                    }
                }
            }
            assert!(!idents.is_empty(), "{} lists nothing", set.source);
            checked += 1;
            continue;
        }
        match set.declared {
            Declared::Enum => assert_eq!(
                idents,
                enum_members(text, own),
                "{} in {}",
                set.source,
                set.file
            ),
            Declared::Consts => {
                for ident in &idents {
                    assert!(
                        text.contains(&format!("pub const {ident}:")),
                        "{}: no `pub const {ident}` in {}",
                        set.source,
                        set.file
                    );
                }
            }
            Declared::Functions => {
                for ident in &idents {
                    assert!(
                        text.contains(&format!("pub fn {ident}("))
                            || text.contains(&format!("pub const fn {ident}(")),
                        "{}: no `pub fn {ident}` in {}",
                        set.source,
                        set.file
                    );
                }
            }
            // Read as values in `the_token_records_are_the_token_tables`.
            Declared::Table => continue,
        }
        assert!(!idents.is_empty(), "{} lists nothing", set.source);
        checked += 1;
    }
    assert!(checked > 0);
}

/// Where a symbol under a crate's module may be declared: that module's own
/// file and the files of its children. A symbol written at the crate root
/// (`koru_voice_ui::VoiceHud`, `koru_ai_ui::ChatSurface::draw`) names an item
/// `lib.rs` declares or re-exports from a private module: it may then be
/// declared in any of the crate's files, and only if `lib.rs` names it.
fn files_of(symbol: &str) -> Vec<&'static str> {
    let mut parts = symbol.split("::");
    let krate = parts.next().unwrap().replace('_', "-");
    let top = parts.next().unwrap_or("lib");
    let root = format!("crates/{krate}/src/");
    let module: Vec<&'static str> = all_sources()
        .filter(|(file, _)| {
            file.strip_prefix(&root).is_some_and(|rest| {
                rest == format!("{top}.rs") || rest.starts_with(&format!("{top}/"))
            })
        })
        .map(|(_, text)| *text)
        .collect();
    if !module.is_empty() {
        return module;
    }
    let lib = format!("{root}lib.rs");
    let Some((_, lib_text)) = all_sources().find(|(file, _)| *file == lib) else {
        return module;
    };
    if !(declares(lib_text, top) || reexports(lib_text).contains(&top)) {
        return module;
    }
    all_sources()
        .filter(|(file, _)| file.starts_with(&root))
        .map(|(_, text)| *text)
        .collect()
}

/// The names a file's `pub use` statements re-export, a statement read whole
/// even when its list runs over several lines.
fn reexports(text: &'static str) -> Vec<&'static str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("\npub use ") {
        let statement = &rest[at + 1..];
        let end = statement.find(';').unwrap_or(statement.len());
        let statement = &statement[..end];
        // The last segment of each path the statement lists.
        names.extend(
            statement
                .split(|c: char| c == ',' || c == '{' || c == '}' || c.is_whitespace())
                .filter_map(|path| path.rsplit("::").next())
                .filter(|name| !name.is_empty()),
        );
        rest = &rest[at + 1 + end..];
    }
    names
}

fn declares(text: &str, item: &str) -> bool {
    [
        format!("pub struct {item}"),
        format!("pub enum {item}"),
        format!("pub trait {item}"),
        format!("pub type {item}"),
        format!("pub mod {item}"),
        format!("pub fn {item}("),
        format!("pub fn {item}<"),
        format!("pub const fn {item}("),
        format!("pub const {item}:"),
        format!("pub static {item}:"),
    ]
    .iter()
    .any(|pattern| {
        text.match_indices(pattern.as_str()).any(|(at, _)| {
            // A whole identifier, not the start of a longer one.
            text[at + pattern.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
                || pattern.ends_with(['(', '<', ':'])
        })
    }) || text.lines().any(|line| {
        line.trim_start().starts_with("pub use ")
            && line
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == item)
    }) || text.contains("macro_rules!") && text.contains(&format!("\n    {item} {{\n"))
}

/// Every symbol a record names is declared where its path says, so a
/// renamed or removed item fails here rather than leaving a stale page.
#[test]
fn every_symbol_is_declared_under_its_path() {
    for entry in catalog() {
        // A native symbol is a Kotlin or Swift declaration, read in its own
        // syntax from the files of its package or target.
        if native::owns(entry.source.owner) {
            for (symbol, _, _) in symbols(entry) {
                assert!(
                    native::declared(symbol),
                    "{}: {symbol} is not declared",
                    entry.catalog_id
                );
            }
            assert!(
                native::is_native_file(entry.source.file),
                "{}: {} is not a compiled-in native source",
                entry.catalog_id,
                entry.source.file
            );
            continue;
        }
        // An app's symbols are read whatever their visibility.
        if app::owns(entry.source.owner) {
            for (symbol, _, _) in symbols(entry) {
                assert!(
                    app::declared(symbol),
                    "{}: {symbol} is not declared",
                    entry.catalog_id
                );
            }
        }
        for (symbol, _, _) in symbols(entry).filter(|_| !app::owns(entry.source.owner)) {
            if symbol == entry.source.module && symbol.matches("::").count() == 1 {
                // The record is the module's constants as a whole.
                assert!(!files_of(&format!("{symbol}::_")).is_empty(), "{symbol}");
                continue;
            }
            let files = files_of(symbol);
            assert!(!files.is_empty(), "{symbol}: its module is not compiled in");
            let segments: Vec<&str> = symbol.split("::").collect();
            let item = segments[segments.len() - 1];
            let parent = segments[segments.len() - 2];
            // `Type::method` when the segment before the item is a type.
            let found = if parent.starts_with(|c: char| c.is_ascii_uppercase()) {
                files.iter().any(|text| declares(text, parent))
                    && files.iter().any(|text| declares(text, item))
            } else {
                files.iter().any(|text| declares(text, item))
            };
            assert!(found, "{}: {symbol} is not declared", entry.catalog_id);
        }
        assert!(
            all_sources().any(|(file, _)| *file == entry.source.file),
            "{}: {} is not a compiled-in source",
            entry.catalog_id,
            entry.source.file
        );
    }
}

/// The component names the shared painters register with the inspector, as
/// the source passes them to `inspect::scope(…, "component", …)`, each once.
///
/// Three spellings are read, because the source uses three: the name on the
/// line above `"component",` (almost every painter); the whole call on one
/// line, `scope("Name", "component", …)`; and a name chosen by an `if` whose
/// arms are bare literals, read back to the line that opens the scope. A
/// chosen name counts once per literal: each is an identity the inspector
/// can show.
fn registered_identities() -> Vec<&'static str> {
    let mut found = Vec::new();
    for (_, text) in all_sources() {
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for (at, line) in lines.iter().enumerate() {
            if let Some(name) = one_line_identity(line) {
                found.push(name);
                continue;
            }
            if *line != "\"component\"," || at == 0 {
                continue;
            }
            let previous = lines[at - 1];
            if let Some(name) = previous
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix("\","))
            {
                found.push(name);
            } else if previous == "}," {
                found.extend(
                    lines[..at - 1]
                        .iter()
                        .rev()
                        .take(8)
                        .take_while(|earlier| !earlier.contains("scope("))
                        .filter_map(|earlier| {
                            earlier
                                .strip_prefix('"')
                                .and_then(|rest| rest.strip_suffix('"'))
                        }),
                );
            }
        }
    }
    found
}

/// `scope("Name", "component", …)` written on one line.
fn one_line_identity(line: &'static str) -> Option<&'static str> {
    let opening = "scope(\"";
    let rest = &line[line.find(opening)? + opening.len()..];
    let (name, rest) = rest.split_once('"')?;
    rest.starts_with(", \"component\"").then_some(name)
}

/// Every component identity a shared painter registers belongs to exactly
/// one record, and no record claims an identity nothing registers.
#[test]
fn every_registered_identity_has_a_record() {
    let mut found = registered_identities();
    found.sort_unstable();
    found.dedup();
    let mut claimed: Vec<&str> = catalog()
        .flat_map(|e| e.identities.iter().copied())
        .collect();
    let count = claimed.len();
    claimed.sort_unstable();
    claimed.dedup();
    assert_eq!(claimed.len(), count, "an identity is claimed twice");
    assert_eq!(found, claimed);
}

/// The composition catalog's node kinds, by the name a declaration uses.
/// Every kind is matched, so a kind added to the catalog stops this file
/// compiling until it is classified below.
fn node_name(kind: &model::NodeKind) -> &'static str {
    match kind {
        model::NodeKind::Row { .. } => "Row",
        model::NodeKind::Column { .. } => "Column",
        model::NodeKind::Box { .. } => "Box",
        model::NodeKind::Text { .. } => "Text",
        model::NodeKind::Spacer { .. } => "Spacer",
        model::NodeKind::Divider => "Divider",
        model::NodeKind::Button { .. } => "Button",
        model::NodeKind::DocumentHost { .. } => "DocumentHost",
    }
}

/// The node kinds, in the order [`node_name`] matches them.
const NODE_KINDS: [&str; 8] = [
    "Row",
    "Column",
    "Box",
    "Text",
    "Spacer",
    "Divider",
    "Button",
    "DocumentHost",
];

/// Each A2UI component a record stands behind is a node kind of the
/// composition catalog or a component of the fixed catalog, named once, and
/// drawn by the host with the symbol the record names. `DocumentHost` is in
/// both catalogs and is one thing — the app's retained editor — so it is
/// claimed once; the app's records hold the fixed catalog's five to the host
/// in `app::the_fixed_catalog_is_served_by_the_app_host`.
#[test]
fn the_a2ui_components_are_the_composition_catalogs() {
    let _ = node_name;
    let mut claimed: Vec<&str> = catalog()
        .flat_map(symbols)
        .filter_map(|(_, a2ui, _)| match a2ui {
            A2ui::Component(name) | A2ui::HostStyled(name) => Some(name),
            _ => None,
        })
        .collect();
    let count = claimed.len();
    claimed.sort_unstable();
    claimed.dedup();
    assert_eq!(claimed.len(), count, "an A2UI component is claimed twice");
    let mut expected: Vec<&str> = NODE_KINDS
        .into_iter()
        .chain(app::fixed_catalog_names())
        .collect();
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(claimed, expected);
    for kind in NODE_KINDS {
        assert!(
            COMPOSITION_MODEL.contains(&format!("\"{kind}\"")),
            "the catalog no longer parses {kind}"
        );
    }
    // How the host draws each, read from the adapter.
    for (symbol, host) in [
        ("koru_draw::el::row", "el::row(\"Composition row\")"),
        ("koru_draw::el::col", "el::col(\"Composition column\")"),
        ("koru_draw::el::boxed", "el::boxed(\"Composition box\")"),
        ("koru_draw::el::text", "el::text(text)"),
        ("koru_draw::el::spacer", "el::spacer()"),
        (
            "koru_draw::el::divider",
            "el::divider(theme::Line::Divider)",
        ),
        ("koru_ui::button::ActionButton", "ActionButton::new(label"),
        (
            "koru_draw::el::scroll_y",
            "el::scroll_y(\"Composition controls\")",
        ),
        ("koru_draw::el::leaf", "el::leaf(\"Document selection\""),
        (
            "koru_ui::button::ActionButton::draw_focused_elided",
            ".draw_focused_elided(",
        ),
    ] {
        assert!(
            catalog()
                .flat_map(symbols)
                .any(|(s, a2ui, _)| s == symbol && !matches!(a2ui, A2ui::NotSupported)),
            "{symbol} is not recorded as used by the composition host"
        );
        assert!(
            COMPOSITION_HOST.contains(host),
            "the composition host no longer draws through {symbol}"
        );
    }
    // The selected button is Primary, the others Secondary, Save and Pin
    // Ghost: the host's styles, not the declaration's.
    for style in ["Primary", "Secondary", "Ghost"] {
        assert!(COMPOSITION_HOST.contains(&format!("ActionStyle::{style}")));
    }
}

/// The values a record says a declaration can name: the catalog's own token
/// enums, matched member for member, and the host's mapping of each.
#[test]
fn the_a2ui_values_are_the_catalogs_tokens() {
    fn mapped(symbol: &str) -> &'static [(&'static str, &'static str)] {
        match catalog()
            .flat_map(symbols)
            .find(|(s, _, _)| *s == symbol)
            .unwrap_or_else(|| panic!("{symbol} is not catalogued"))
            .1
        {
            A2ui::Values { mapped, .. } => mapped,
            other => panic!("{symbol} is {other:?}"),
        }
    }
    fn declared(symbol: &str) -> Vec<&'static str> {
        mapped(symbol).iter().map(|(d, _)| *d).collect()
    }
    // Each list is the catalog enum's members, in order; each `match` is
    // exhaustive, so a member added upstream stops this compiling.
    let space = |s: model::Space| match s {
        model::Space::S0 => "S0",
        model::Space::S0_5 => "S0_5",
        model::Space::S1 => "S1",
        model::Space::S1_5 => "S1_5",
        model::Space::S2 => "S2",
        model::Space::S2_5 => "S2_5",
        model::Space::S3 => "S3",
        model::Space::S3_5 => "S3_5",
        model::Space::S4 => "S4",
        model::Space::S5 => "S5",
        model::Space::S6 => "S6",
        model::Space::S7 => "S7",
        model::Space::S8 => "S8",
        model::Space::S10 => "S10",
        model::Space::S12 => "S12",
    };
    let radius = |r: model::Radius| match r {
        model::Radius::None => "None",
        model::Radius::S => "S",
        model::Radius::M => "M",
        model::Radius::L => "L",
        model::Radius::XL => "XL",
        model::Radius::XXL => "XXL",
        model::Radius::Full => "Full",
    };
    let role = |r: model::TextRole| match r {
        model::TextRole::Ui => "Ui",
        model::TextRole::Row => "Row",
        model::TextRole::Hint => "Hint",
        model::TextRole::Meta => "Meta",
        model::TextRole::PopoverHeading => "PopoverHeading",
    };
    let ink = |i: model::Ink| match i {
        model::Ink::Primary => "Primary",
        model::Ink::Secondary => "Secondary",
        model::Ink::Hint => "Hint",
        model::Ink::Accent => "Accent",
    };
    let surface = |s: model::Surface| match s {
        model::Surface::Ground => "Ground",
        model::Surface::Raised => "Raised",
        model::Surface::Chip => "Chip",
    };
    let align = |a: model::Align| match a {
        model::Align::Start => "Start",
        model::Align::Center => "Center",
        model::Align::End => "End",
        model::Align::Stretch => "Stretch",
    };
    let sizing = |s: model::Sizing| match s {
        model::Sizing::Hug => "Hug",
        model::Sizing::Fill => "Fill",
    };
    use model::{Align, Ink, Radius, Sizing, Space, Surface, TextRole};
    assert_eq!(
        declared("koru_draw::theme::Space"),
        [
            Space::S0,
            Space::S0_5,
            Space::S1,
            Space::S1_5,
            Space::S2,
            Space::S2_5,
            Space::S3,
            Space::S3_5,
            Space::S4,
            Space::S5,
            Space::S6,
            Space::S7,
            Space::S8,
            Space::S10,
            Space::S12,
        ]
        .map(space)
    );
    assert_eq!(
        declared("koru_draw::theme::Radius"),
        [
            Radius::None,
            Radius::S,
            Radius::M,
            Radius::L,
            Radius::XL,
            Radius::XXL,
            Radius::Full,
        ]
        .map(radius)
    );
    assert_eq!(
        declared("koru_draw::theme::roles"),
        [
            TextRole::Ui,
            TextRole::Row,
            TextRole::Hint,
            TextRole::Meta,
            TextRole::PopoverHeading,
        ]
        .map(role)
    );
    assert_eq!(
        declared("koru_draw::theme::Ink"),
        [Ink::Primary, Ink::Secondary, Ink::Hint, Ink::Accent].map(ink)
    );
    assert_eq!(
        declared("koru_draw::theme::Surface"),
        [Surface::Ground, Surface::Raised, Surface::Chip].map(surface)
    );
    assert_eq!(
        declared("koru_draw::el::Align"),
        [Align::Start, Align::Center, Align::End, Align::Stretch].map(align)
    );
    assert_eq!(declared("koru_draw::el::El::fill"), [sizing(Sizing::Fill)]);
    assert_eq!(declared("koru_draw::el::El::hug_w"), [sizing(Sizing::Hug)]);
    assert_eq!(declared("koru_draw::el::El::hug_h"), [sizing(Sizing::Hug)]);
    // The host's own mapping, line by line.
    for (symbol, line) in [
        (
            "koru_draw::theme::Space",
            "model::Space::{d} => Space::{p},",
        ),
        (
            "koru_draw::theme::Radius",
            "model::Radius::{d} => theme::Radius::{p},",
        ),
        (
            "koru_draw::theme::roles",
            "model::TextRole::{d} => theme::roles::{p},",
        ),
        (
            "koru_draw::theme::Ink",
            "model::Ink::{d} => theme::Ink::{p},",
        ),
        (
            "koru_draw::theme::Surface",
            "model::Surface::{d} => theme::Surface::{p},",
        ),
    ] {
        for (d, p) in mapped(symbol) {
            let expected = line.replace("{d}", d).replace("{p}", p);
            assert!(
                COMPOSITION_HOST.contains(&expected),
                "the composition host no longer maps {d} to {p}: `{expected}`"
            );
        }
    }
    for (d, _) in mapped("koru_draw::el::Align") {
        let expected = format!(
            "Some(model::Align::{d}) => element.items_{}(),",
            d.to_lowercase()
        );
        assert!(COMPOSITION_HOST.contains(&expected), "{expected}");
    }
    for needle in ["element.fill()", "element.hug_w()", "element.hug_h()"] {
        assert!(COMPOSITION_HOST.contains(needle), "{needle}");
    }
}

/// Everything else a record names is outside the catalog: the list of what
/// the composition host uses is exactly the list above, and nothing a
/// declaration cannot name is advertised as if it could.
#[test]
fn only_the_catalogs_symbols_claim_a2ui_support() {
    let mut supported: Vec<&str> = catalog()
        .flat_map(symbols)
        .filter(|(_, a2ui, _)| !matches!(a2ui, A2ui::NotSupported))
        .map(|(symbol, _, _)| symbol)
        .collect();
    supported.sort_unstable();
    assert_eq!(
        supported,
        [
            "koru::leaf::Leaf",
            "koru::mini_app::MiniApp",
            "koru::mini_app::MiniApp::draw_composition",
            "koru::mini_app::MiniApp::draw_surface",
            "koru::mini_app::MiniApp::fixture",
            "koru::mini_app::MiniApp::from_declaration",
            "koru::mini_app::MiniApp::land",
            "koru::mini_app::MiniApp::refine",
            "koru::mini_app::MiniApp::set_lists",
            "koru::mini_app::chrome::Chrome::changed_query",
            "koru::mini_app::chrome::Chrome::open_selector",
            "koru::mini_app::chrome::Chrome::poll",
            "koru::mini_app::composition::Surface",
            "koru::mini_app::composition::Surface::layout",
            "koru::mini_app::composition::ink_role",
            "koru::mini_app::composition::radius_role",
            "koru::mini_app::composition::spacing",
            "koru::mini_app::composition::text_role",
            "koru::tasks_view::TasksView",
            "koru::tasks_view::TasksView::collection_only",
            "koru_draw::el::Align",
            "koru_draw::el::El::fill",
            "koru_draw::el::El::hug_h",
            "koru_draw::el::El::hug_w",
            "koru_draw::el::boxed",
            "koru_draw::el::col",
            "koru_draw::el::divider",
            "koru_draw::el::leaf",
            "koru_draw::el::row",
            "koru_draw::el::scroll_y",
            "koru_draw::el::spacer",
            "koru_draw::el::text",
            "koru_draw::theme::Ink",
            "koru_draw::theme::Radius",
            "koru_draw::theme::Space",
            "koru_draw::theme::Surface",
            "koru_draw::theme::roles",
            "koru_search_ui::EntitySearch",
            "koru_search_ui::EntitySearch::new_at",
            "koru_search_ui::EntitySearch::pending",
            "koru_search_ui::EntitySearch::poll",
            "koru_search_ui::EntitySearch::request",
            "koru_search_ui::EntitySearchUpdate",
            "koru_ui::button::ActionButton",
            "koru_ui::button::ActionButton::draw_focused_elided",
            "koru_workspace_host::accessibility::ControlPart",
            "koru_workspace_host::accessibility::ObjectKey",
            "koru_workspace_host::accessibility::Occurrence",
            "koru_workspace_host::accessibility::Revision",
            "koru_workspace_host::accessibility::SemanticNode",
            "koru_workspace_host::accessibility::ViewSlot",
            "koru_workspace_host::accessibility::WindowSlot",
        ]
    );
}

/// The A2UI hosts publish the controls a declaration draws through the
/// workspace host's semantic identity: the composition host names five of
/// its types, the mini-app chrome that hosts the fixed catalog seven. Each
/// symbol a record marks host-only for that reason is imported there, as
/// the host's source spells it.
#[test]
fn the_a2ui_hosts_publish_their_controls_through_the_workspace_accessibility() {
    let claimed: Vec<&str> = catalog()
        .filter(|e| e.source.owner == "koru-workspace-host")
        .flat_map(symbols)
        .filter(|(_, a2ui, _)| *a2ui == A2ui::HostOnly)
        .map(|(symbol, _, _)| symbol)
        .collect();
    let item = |symbol: &'static str| symbol.rsplit("::").next().unwrap();
    let composition = [
        "ControlPart",
        "Occurrence",
        "SemanticNode",
        "ViewSlot",
        "WindowSlot",
    ];
    let chrome = [
        "ControlPart",
        "ObjectKey",
        "Occurrence",
        "Revision",
        "SemanticNode",
        "ViewSlot",
        "WindowSlot",
    ];
    let mut items: Vec<&str> = claimed.iter().copied().map(item).collect();
    items.sort_unstable();
    assert_eq!(items, chrome, "the host-only workspace accessibility types");
    let imports = |host: &str| -> String {
        let start = host
            .find("use koru_workspace_host::accessibility::{")
            .expect("the host imports the workspace accessibility types");
        let rest = &host[start..];
        rest[..rest.find("};").expect("the import closes")].to_owned()
    };
    for (host, names) in [
        (imports(COMPOSITION_HOST), &composition[..]),
        (imports(MINI_APP_CHROME), &chrome[..]),
    ] {
        for name in names {
            assert!(
                host.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|word| word == *name),
                "an A2UI host no longer imports {name}: {host}"
            );
        }
    }
}

/// The fixed catalog's `DocumentSearch` is the mini-app chrome's own field
/// and floating results (an app-local surface, recorded with the app); what
/// it reads through is `koru-search-ui`'s entity search. Each symbol that
/// says so is used there, as the host's source spells it, and the catalog
/// still names the component.
#[test]
fn the_fixed_catalogs_document_search_reads_through_the_entity_search() {
    assert!(
        FIXED_CATALOG.contains("(\"search\", \"DocumentSearch\")"),
        "the fixed catalog no longer names its search component"
    );
    // The workspace host's semantic identity types are host-only for the
    // controls the hosts publish, held by the test above.
    let host_only: Vec<&str> = domain::CRATES
        .iter()
        .filter(|krate| krate.owner != "koru-workspace-host")
        .flat_map(|krate| catalog().filter(move |e| e.source.owner == krate.owner))
        .flat_map(symbols)
        .filter(|(_, a2ui, _)| *a2ui == A2ui::HostOnly)
        .map(|(symbol, _, _)| symbol)
        .collect();
    let uses = [
        (
            "koru_search_ui::EntitySearch",
            "Option<koru_search_ui::EntitySearch>",
        ),
        (
            "koru_search_ui::EntitySearch::new_at",
            "koru_search_ui::EntitySearch::new_at(",
        ),
        ("koru_search_ui::EntitySearch::request", "reader.request("),
        ("koru_search_ui::EntitySearch::poll", "reader.poll()"),
        ("koru_search_ui::EntitySearch::pending", "reader.pending()"),
        (
            "koru_search_ui::EntitySearchUpdate",
            "result: koru_search_ui::EntitySearchUpdate",
        ),
    ];
    assert_eq!(
        host_only,
        uses.map(|(symbol, _)| symbol),
        "a domain symbol claims the fixed catalog's host, or lost it"
    );
    for (symbol, host) in uses {
        assert!(
            MINI_APP_CHROME.contains(host),
            "the mini-app chrome no longer reads {symbol} (`{host}`)"
        );
    }
}

/// Each domain crate's `lib.rs` against its baseline in [`domain::CRATES`]:
/// every module classified, every record of the crate reached from a module
/// or from `lib.rs` itself, and no record left over.
#[test]
fn every_domain_module_is_classified() {
    let mut owners = Vec::new();
    for krate in domain::CRATES {
        let mut ids = check_baseline(krate.lib, krate.modules, false);
        for id in krate.root {
            assert_eq!(entry(id).source.owner, krate.owner, "{id}");
            ids.push(id);
        }
        for (_, module) in krate.modules {
            if let Module::Records(records) = module {
                for id in *records {
                    assert_eq!(entry(id).source.owner, krate.owner, "{id}");
                }
            }
        }
        for record in catalog().filter(|e| e.source.owner == krate.owner) {
            assert!(
                ids.contains(&record.catalog_id),
                "{} is a {} record no module of its lib.rs accounts for",
                record.catalog_id,
                krate.owner
            );
        }
        owners.push(krate.owner);
    }
    owners.sort_unstable();
    owners.dedup();
    assert_eq!(
        owners.len(),
        21,
        "the thirteen domain UI crates, batch 2b's editor, document and host crates and batch 4's notification boundary"
    );
}

/// What one record's symbols add up to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Disposition {
    StoryExists,
    NoStoryYet,
    Nonvisual,
    PlatformOnly,
    Boundary,
}

/// A record has a story if any symbol has one; otherwise it waits for one
/// if any visual symbol does; otherwise it is a boundary, platform-only or
/// nonvisual, in that order.
fn disposition(entry: &Entry) -> Disposition {
    // The registry's own answer: the one a record's page states.
    match entry.status() {
        Story::Available(_) => Disposition::StoryExists,
        Story::NoStoryYet => Disposition::NoStoryYet,
        Story::Boundary => Disposition::Boundary,
        Story::PlatformOnly => Disposition::PlatformOnly,
        Story::Nonvisual => Disposition::Nonvisual,
    }
}

/// The counts the manifest states, computed from the registration and the
/// compiled-in source.
fn counts() -> Vec<(&'static str, usize)> {
    let entries: Vec<&Entry> = catalog().collect();
    let count = |f: &dyn Fn(&&Entry) -> bool| entries.iter().filter(|e| f(e)).count();
    let ui = declared_modules(source("crates/koru-ui/src/lib.rs"));
    let draw = declared_modules(source("crates/koru-draw/src/lib.rs"));
    let theme = declared_modules(source("crates/koru-draw/src/theme.rs"));
    let domain_modules: Vec<(&str, bool)> = domain::CRATES
        .iter()
        .flat_map(|krate| declared_modules(source(krate.lib)))
        .collect();
    let domain_excluded = domain::CRATES
        .iter()
        .flat_map(|krate| krate.modules)
        .filter(|(_, module)| matches!(module, Module::Excluded(_)))
        .count();
    let domain_owned = |e: &&Entry| {
        domain::CRATES
            .iter()
            .any(|krate| krate.owner == e.source.owner)
    };
    let all_symbols: Vec<(&str, A2ui, Story)> = entries.iter().flat_map(|e| symbols(e)).collect();
    let option_members = |kind: Kind| -> usize {
        entries
            .iter()
            .filter(|e| e.kind == kind)
            .flat_map(|e| e.symbols())
            .flat_map(|symbol| symbol.options)
            .map(|set| set.members.len())
            .sum()
    };
    let identity_sites = registered_identities().len();
    let mut identities = registered_identities();
    identities.sort_unstable();
    identities.dedup();
    let story = |s: fn(&Story) -> bool| all_symbols.iter().filter(|(_, _, st)| s(st)).count();
    let a2ui = |a: fn(&A2ui) -> bool| all_symbols.iter().filter(|(_, x, _)| a(x)).count();
    let mut rows = vec![
        (
            "koru-ui public modules",
            ui.iter().filter(|(_, p)| *p).count(),
        ),
        (
            "koru-ui private modules",
            ui.iter().filter(|(_, p)| !*p).count(),
        ),
        (
            "koru-draw public modules",
            draw.iter().filter(|(_, p)| *p).count(),
        ),
        (
            "koru-draw theme public submodules",
            theme.iter().filter(|(_, p)| *p).count(),
        ),
        ("domain crates", domain::CRATES.len()),
        ("domain crate source files", domain::SOURCES.len()),
        ("domain crate lib.rs modules", domain_modules.len()),
        (
            "domain crate lib.rs public modules",
            domain_modules.iter().filter(|(_, p)| *p).count(),
        ),
        ("domain crate modules excluded", domain_excluded),
    ];
    rows.extend(app::counts());
    rows.extend(native::counts());
    rows.extend([
        ("records", entries.len()),
        (
            "records: foundations",
            count(&|e| e.family == Family::Foundations),
        ),
        (
            "records: controls",
            count(&|e| e.family == Family::Controls),
        ),
        (
            "records: application compositions",
            count(&|e| e.family == Family::Compositions),
        ),
        ("records: domain crates", count(&domain_owned)),
        ("kind: token", count(&|e| e.kind == Kind::Token)),
        ("kind: primitive", count(&|e| e.kind == Kind::Primitive)),
        ("kind: widget", count(&|e| e.kind == Kind::Widget)),
        ("kind: support", count(&|e| e.kind == Kind::Support)),
        (
            "kind: domain composition",
            count(&|e| e.kind == Kind::DomainComposition),
        ),
        ("kind: app-local", count(&|e| e.kind == Kind::AppLocal)),
        (
            "kind: native adapter",
            count(&|e| e.kind == Kind::NativeAdapter),
        ),
        ("token values listed", option_members(Kind::Token)),
        ("symbols", all_symbols.len()),
        (
            "symbols with typed recipes",
            entries.iter().map(|e| e.recipes.len()).sum(),
        ),
        ("inspector identities", identities.len()),
        ("inspector identity registration sites", identity_sites),
        (
            "disposition: story exists",
            count(&|e| disposition(e) == Disposition::StoryExists),
        ),
        (
            "disposition: no story yet",
            count(&|e| disposition(e) == Disposition::NoStoryYet),
        ),
        (
            "disposition: nonvisual",
            count(&|e| disposition(e) == Disposition::Nonvisual),
        ),
        (
            "disposition: platform-only",
            count(&|e| disposition(e) == Disposition::PlatformOnly),
        ),
        (
            "disposition: boundary",
            count(&|e| disposition(e) == Disposition::Boundary),
        ),
        (
            "symbol story: available",
            story(|s| matches!(s, Story::Available(_))),
        ),
        (
            "symbol story: no story yet",
            story(|s| *s == Story::NoStoryYet),
        ),
        ("symbol story: nonvisual", story(|s| *s == Story::Nonvisual)),
        (
            "symbol story: platform-only",
            story(|s| *s == Story::PlatformOnly),
        ),
        ("symbol story: boundary", story(|s| *s == Story::Boundary)),
        ("A2UI: component", a2ui(|a| matches!(a, A2ui::Component(_)))),
        (
            "A2UI: host-styled",
            a2ui(|a| matches!(a, A2ui::HostStyled(_))),
        ),
        (
            "A2UI: declarable values",
            a2ui(|a| matches!(a, A2ui::Values { .. })),
        ),
        ("A2UI: host only", a2ui(|a| *a == A2ui::HostOnly)),
        ("A2UI: not supported", a2ui(|a| *a == A2ui::NotSupported)),
    ]);
    rows
}

/// The manifest's counts table is the registration's: a record added,
/// removed or reclassified fails here until the manifest says so.
#[test]
fn the_manifests_counts_are_the_registrations() {
    let start = MANIFEST
        .find("<!-- counts:begin -->")
        .expect("the manifest has a counts table");
    let end = MANIFEST
        .find("<!-- counts:end -->")
        .expect("the counts table ends");
    let stated: Vec<(String, usize)> = MANIFEST[start..end]
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            let value = cells.get(2)?.parse().ok()?;
            Some((cells.get(1)?.to_string(), value))
        })
        .collect();
    let computed: Vec<(String, usize)> = counts()
        .into_iter()
        .map(|(label, n)| (label.to_owned(), n))
        .collect();
    let table: String = computed
        .iter()
        .map(|(label, n)| format!("| {label} | {n} |\n"))
        .collect();
    assert_eq!(
        stated, computed,
        "the manifest's counts should read:\n{table}"
    );
}

/// The availability values, in the order the manifest's platform table
/// lists them. Every member is matched, so a value added to [`Availability`]
/// stops this compiling until the table has a column for it.
const AVAILABILITIES: [(Availability, &str); 9] = [
    (Availability::Available, "available"),
    (Availability::Existing, "existing"),
    (Availability::SharedSourceUnverified, "shared source"),
    (Availability::NativePartial, "native partial"),
    (Availability::Planned, "planned"),
    (Availability::Unavailable, "unavailable"),
    (Availability::NotApplicable, "not applicable"),
    (Availability::NativeOwned, "native-owned"),
    (Availability::NotAudited, "not audited"),
];

const fn column(availability: Availability) -> usize {
    match availability {
        Availability::Available => 0,
        Availability::Existing => 1,
        Availability::SharedSourceUnverified => 2,
        Availability::NativePartial => 3,
        Availability::Planned => 4,
        Availability::Unavailable => 5,
        Availability::NotApplicable => 6,
        Availability::NativeOwned => 7,
        Availability::NotAudited => 8,
    }
}

/// The manifest's platform table is the registration's: one row per
/// platform, one column per availability, each cell the number of records
/// that say that about that platform.
#[test]
fn the_manifests_platform_cells_are_the_registrations() {
    for (slot, (availability, _)) in AVAILABILITIES.iter().enumerate() {
        assert_eq!(column(*availability), slot);
    }
    let platforms = [
        (Platform::Windows, "Windows"),
        (Platform::MacOs, "macOS"),
        (Platform::Linux, "Linux"),
        (Platform::Android, "Android"),
        (Platform::Ios, "iOS"),
    ];
    let mut table = String::from("| Platform |");
    for (_, name) in AVAILABILITIES {
        table.push_str(&format!(" {name} |"));
    }
    table.push_str("\n| --- |");
    for _ in AVAILABILITIES {
        table.push_str(" --- |");
    }
    table.push('\n');
    for (platform, name) in platforms {
        let mut cells = [0usize; 9];
        for entry in catalog() {
            for (p, availability) in entry.platforms {
                if p == platform {
                    cells[column(availability)] += 1;
                }
            }
        }
        table.push_str(&format!("| {name} |"));
        for cell in cells {
            table.push_str(&format!(" {cell} |"));
        }
        table.push('\n');
    }
    let marker = MANIFEST
        .find("<!-- cells:begin -->")
        .expect("the manifest has a platform table");
    let start = marker
        + MANIFEST[marker..]
            .find('\n')
            .expect("the marker ends its line")
        + 1;
    let end = MANIFEST
        .find("<!-- cells:end -->")
        .expect("the platform table ends");
    assert_eq!(
        MANIFEST[start..end].replace("\r\n", "\n"),
        table,
        "the manifest's platform table should read:\n{table}"
    );
}

mod app;
mod domain;
mod native;
