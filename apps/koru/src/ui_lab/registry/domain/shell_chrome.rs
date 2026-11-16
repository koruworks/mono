// SPDX-License-Identifier: MPL-2.0

//! **`koru-shell-chrome`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-shell-chrome";

const BANNER: &str = "crates/koru-shell-chrome/src/banner.rs";
const BRANCH: &str = "crates/koru-shell-chrome/src/branch.rs";
const CAPTION: &str = "crates/koru-shell-chrome/src/caption.rs";
const LANE: &str = "crates/koru-shell-chrome/src/lane.rs";
const REGIONS: &str = "crates/koru-shell-chrome/src/regions.rs";
const REORDER: &str = "crates/koru-shell-chrome/src/reorder.rs";
const STATUS_CHIP: &str = "crates/koru-shell-chrome/src/status_chip.rs";
const STATUS_MARK: &str = "crates/koru-shell-chrome/src/status_mark.rs";
const TABS: &str = "crates/koru-shell-chrome/src/tabs.rs";
const TITLE_ICON: &str = "crates/koru-shell-chrome/src/title_icon.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: free functions over a rect, a text engine and the
    // name the host passes
    record(
        "C-BRANCH-CHIP",
        "KoruBranchChip",
        Key::UiLabBranchChipName,
        Key::UiLabBranchChipPurpose,
        Family::Controls,
        Kind::Widget,
        Some("C-CHROME"),
        &["KoruBranchChip"],
        source(OWNER, "koru_shell_chrome::branch", BRANCH),
        [
            (Platform::Windows, Availability::Existing),
            (Platform::MacOs, Availability::SharedSourceUnverified),
            (Platform::Linux, Availability::SharedSourceUnverified),
            (Platform::Android, Availability::NotApplicable),
            (Platform::Ios, Availability::NotApplicable),
        ],
        &[
            painter("koru_shell_chrome::branch::draw"),
            support("koru_shell_chrome::branch::width"),
            support("koru_shell_chrome::branch::pill"),
            support("koru_shell_chrome::branch::MAX_W"),
        ],
    ),
    // Construction: pure: draws koru-ui's LabelChip (Inverted) and a floating
    // panel from host-supplied, already translated text
    record(
        "C-STATUS-CHIP",
        "status_chip",
        Key::UiLabStatusChipName,
        Key::UiLabStatusChipPurpose,
        Family::Controls,
        Kind::Widget,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::status_chip", STATUS_CHIP),
        DESKTOP_SHARED,
        &[
            painter("koru_shell_chrome::status_chip::draw"),
            support("koru_shell_chrome::status_chip::Text"),
            support("koru_shell_chrome::status_chip::chip"),
            support("koru_shell_chrome::status_chip::CHIP_W"),
        ],
    ),
    // Construction: pure; spin_phase reads one process-wide start instant (a
    // OnceLock), no thread
    record(
        "C-STATUS-MARK",
        "StatusMark",
        Key::UiLabStatusMarkName,
        Key::UiLabStatusMarkPurpose,
        Family::Controls,
        Kind::Widget,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::status_mark", STATUS_MARK),
        DESKTOP_SHARED,
        &[
            painter_with(
                "koru_shell_chrome::status_mark::draw",
                &[values(
                    Key::UiLabControlKind,
                    "koru_shell_chrome::status_mark::StatusMark",
                    STATUS_MARK,
                    Declared::Enum,
                    &[
                        labelled("Spinner", Key::UiLabOptionSpinner),
                        labelled("Question", Key::UiLabOptionQuestion),
                        labelled("Dot", Key::UiLabOptionDot),
                    ],
                )],
            ),
            painter("koru_shell_chrome::status_mark::draw_badge"),
            support("koru_shell_chrome::status_mark::StatusMark"),
            support("koru_shell_chrome::status_mark::spin_phase"),
            support("koru_shell_chrome::status_mark::FRAME"),
        ],
    ),
    // Construction: pure: the host passes tabs, widths and scroll state each
    // frame
    record(
        "C-TAB-STRIP",
        "tabs::draw",
        Key::UiLabTabStripName,
        Key::UiLabTabStripPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::tabs", TABS),
        DESKTOP_SHARED,
        &[
            painter("koru_shell_chrome::tabs::draw"),
            painter("koru_shell_chrome::tabs::draw_icon"),
            painter("koru_shell_chrome::tabs::draw_icon_filled"),
            support_with(
                "koru_shell_chrome::tabs::Tab",
                &[values(
                    Key::UiLabControlLeading,
                    "koru_shell_chrome::tabs::TabIcon",
                    TABS,
                    Declared::Enum,
                    &[
                        labelled("Emoji", Key::UiLabOptionEmoji),
                        labelled("Glyph", Key::UiLabOptionGlyph),
                        labelled("Swatch", Key::UiLabOptionSwatch),
                        labelled("Brand", Key::UiLabOptionProductMark),
                    ],
                )],
            ),
            support("koru_shell_chrome::tabs::TabIcon"),
            support("koru_shell_chrome::tabs::TabRect"),
            support("koru_shell_chrome::tabs::layout"),
            support("koru_shell_chrome::tabs::preferred_width"),
            support("koru_shell_chrome::tabs::new_tab"),
            support("koru_shell_chrome::tabs::drop_index"),
            support("koru_shell_chrome::tabs::ensure_visible"),
            support("koru_shell_chrome::tabs::scroll_thumb"),
            support("koru_shell_chrome::tabs::hover_chip"),
        ],
    ),
    // Construction: pure
    record(
        "P-PAGE-EMOJI",
        "draw_page_emoji",
        Key::UiLabPageEmojiName,
        Key::UiLabPageEmojiPurpose,
        Family::Foundations,
        Kind::Primitive,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::title_icon", TITLE_ICON),
        DESKTOP_SHARED,
        &[
            painter("koru_shell_chrome::title_icon::draw_page_emoji"),
            support("koru_shell_chrome::title_icon::PAGE_EMOJI_SIZE"),
            support("koru_shell_chrome::title_icon::PAGE_ICON_SLOT"),
        ],
    ),
    // Construction: pure arithmetic
    record(
        "H-BANNER-SLOTS",
        "Slots",
        Key::UiLabBannerSlotsName,
        Key::UiLabBannerSlotsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::banner", BANNER),
        DESKTOP_SHARED,
        &[
            support_with(
                "koru_shell_chrome::banner::Slots",
                &[values(
                    Key::UiLabControlMode,
                    "koru_shell_chrome::banner::Workspace",
                    BANNER,
                    Declared::Enum,
                    &[literal("Hidden"), literal("Named"), literal("IconOnly")],
                )],
            ),
            support("koru_shell_chrome::banner::Workspace"),
            support("koru_shell_chrome::banner::Hit"),
            support("koru_shell_chrome::banner::profile_slot"),
            support("koru_shell_chrome::banner::workspace_slot"),
            support("koru_shell_chrome::banner::settings_slot"),
            support("koru_shell_chrome::banner::hit"),
            support("koru_shell_chrome::banner::pane_hit"),
        ],
    ),
    // Construction: pure arithmetic
    record(
        "H-CAPTION-LAYOUT",
        "caption::tab_strip",
        Key::UiLabCaptionLayoutName,
        Key::UiLabCaptionLayoutPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::caption", CAPTION),
        DESKTOP_SHARED,
        &[
            support("koru_shell_chrome::caption::sidebar_toggle"),
            support("koru_shell_chrome::caption::context_toggle"),
            support("koru_shell_chrome::caption::host_action"),
            support("koru_shell_chrome::caption::leading_chain"),
            support("koru_shell_chrome::caption::Chain"),
            support("koru_shell_chrome::caption::tab_strip"),
            support("koru_shell_chrome::caption::tab_strip_with_leading"),
            support("koru_shell_chrome::caption::DIVIDER_W"),
        ],
    ),
    // Construction: pure arithmetic
    record(
        "H-LANE-LAYOUT",
        "LaneRects",
        Key::UiLabLaneLayoutName,
        Key::UiLabLaneLayoutPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::lane", LANE),
        DESKTOP_SHARED,
        &[
            support("koru_shell_chrome::lane::solve"),
            support("koru_shell_chrome::lane::LaneRects"),
            support_with(
                "koru_shell_chrome::lane::Leading",
                &[values(
                    Key::UiLabControlLeading,
                    "koru_shell_chrome::lane::Mark",
                    LANE,
                    Declared::Enum,
                    &[literal("None"), literal("Mark"), literal("Lights")],
                )],
            ),
            support("koru_shell_chrome::lane::Mark"),
            support("koru_shell_chrome::lane::Hit"),
            support("koru_shell_chrome::lane::hit"),
            support("koru_shell_chrome::lane::square"),
        ],
    ),
    // Construction: pure arithmetic
    record(
        "H-WINDOW-REGIONS",
        "Regions",
        Key::UiLabWindowRegionsName,
        Key::UiLabWindowRegionsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::regions", REGIONS),
        DESKTOP_SHARED,
        &[
            support("koru_shell_chrome::regions::solve"),
            support("koru_shell_chrome::regions::Regions"),
            support("koru_shell_chrome::regions::Hit"),
            support("koru_shell_chrome::regions::hit"),
            support("koru_shell_chrome::regions::hit_with_banner"),
        ],
    ),
    // Construction: pure arithmetic
    record(
        "H-DRAG-REORDER",
        "reorder::drop_index",
        Key::UiLabDragReorderName,
        Key::UiLabDragReorderPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_shell_chrome::reorder", REORDER),
        DESKTOP_SHARED,
        &[
            support("koru_shell_chrome::reorder::drop_index"),
            support("koru_shell_chrome::reorder::carry"),
        ],
    ),
];
