// SPDX-License-Identifier: MPL-2.0

//! **`koru-app-shell`**: the chrome it paints, the tab and pane machinery a
//! host drives, and the host and surface contracts it sits on, a record
//! each, written from the source they name. `registry/MANIFEST.md` lists the
//! files read, which of these have a production consumer, and what building
//! each needs.

use super::*;

const OWNER: &str = "koru-app-shell";

const BANNER_STATE: &str = "crates/koru-app-shell/src/banner_state.rs";
const CHROME: &str = "crates/koru-app-shell/src/chrome.rs";
const HOST: &str = "crates/koru-app-shell/src/host.rs";
const PANEL: &str = "crates/koru-app-shell/src/panel.rs";
const POPOVER: &str = "crates/koru-app-shell/src/popover.rs";
const STATE: &str = "crates/koru-app-shell/src/state.rs";
const SURFACE: &str = "crates/koru-app-shell/src/surface.rs";
const TABSTRIP: &str = "crates/koru-app-shell/src/tabstrip.rs";
const ZOOM_LABEL: &str = "crates/koru-app-shell/src/zoom_label.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: free painters over a host-built CaptionState or
    // PaneHeader; every word arrives on a BannerChip already localized
    record(
        "C-SHELL-CAPTION",
        "chrome::draw_banner",
        Key::UiLabShellCaptionName,
        Key::UiLabShellCaptionPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_app_shell", CHROME),
        DESKTOP_SHARED,
        &[
            painter("koru_app_shell::chrome::draw_band"),
            painter("koru_app_shell::chrome::draw_banner"),
            painter("koru_app_shell::chrome::draw_pane_header"),
            support("koru_app_shell::chrome::CaptionState"),
            support("koru_app_shell::chrome::ChipStates"),
            support("koru_app_shell::chrome::PaneHeader"),
            support("koru_app_shell::chrome::chrome_background"),
            support("koru_app_shell::chrome::band_hairline"),
            support("koru_app_shell::chrome::tooltip"),
            support("koru_app_shell::chrome::TooltipText"),
            support("koru_app_shell::Banner"),
            support("koru_app_shell::BannerChip"),
            support("koru_app_shell::BannerChip::new"),
            support("koru_app_shell::BannerChip::with_mark"),
            support("koru_app_shell::BannerChip::with_tint"),
            support("koru_app_shell::BannerChip::with_tooltip"),
            support("koru_app_shell::BannerChip::unavailable"),
            support_with(
                "koru_app_shell::BannerSlot",
                &[values(
                    Key::UiLabControlKind,
                    "koru_app_shell::BannerSlot",
                    HOST,
                    Declared::Enum,
                    &[
                        literal("Profile"),
                        literal("Workspace"),
                        literal("Settings"),
                        literal("Identity"),
                    ],
                )],
            ),
            support("koru_app_shell::BannerKey"),
        ],
    ),
    // Construction: pure: open(slot, anchor, content, size) is geometry over
    // a PopoverShell; the rows in it are the host's
    record(
        "C-SHELL-POPOVER",
        "Popover",
        Key::UiLabShellPopoverName,
        Key::UiLabShellPopoverPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_app_shell", POPOVER),
        DESKTOP_SHARED,
        &[
            painter("koru_app_shell::Popover"),
            painter("koru_app_shell::Popover::open"),
            painter("koru_app_shell::Popover::draw"),
            support_with(
                "koru_app_shell::Popover::slide_to",
                &[values(
                    Key::UiLabControlPlacement,
                    "koru_app_shell::SlideFrom",
                    POPOVER,
                    Declared::Enum,
                    &[
                        labelled("Left", Key::UiLabOptionLeft),
                        labelled("Right", Key::UiLabOptionRight),
                    ],
                )],
            ),
            support("koru_app_shell::Popover::view_offsets"),
            support("koru_app_shell::Popover::animating"),
            support("koru_app_shell::Popover::advance"),
            support("koru_app_shell::Popover::bounds"),
            support("koru_app_shell::Popover::contains"),
            support("koru_app_shell::SlideFrom"),
            support("koru_app_shell::popover::GAP"),
        ],
    ),
    // Construction: pure: a deadline and an alpha; draw reads the process
    // theme and the engine the host lends
    record(
        "C-SHELL-ZOOM-LABEL",
        "ZoomLabel",
        Key::UiLabShellZoomLabelName,
        Key::UiLabShellZoomLabelPurpose,
        Family::Controls,
        Kind::Widget,
        None,
        &[],
        source(OWNER, "koru_app_shell", ZOOM_LABEL),
        DESKTOP_SHARED,
        &[
            painter("koru_app_shell::ZoomLabel"),
            painter("koru_app_shell::ZoomLabel::new"),
            painter("koru_app_shell::ZoomLabel::draw"),
            support("koru_app_shell::ZoomLabel::show"),
            support("koru_app_shell::ZoomLabel::showing"),
            support("koru_app_shell::ZoomLabel::alpha"),
            support("koru_app_shell::ZoomLabel::animating"),
            support("koru_app_shell::ZoomLabel::maintenance_deadline"),
            support("koru_app_shell::ZoomLabel::expire"),
            support("koru_app_shell::ZoomLabel::pill"),
            support("koru_app_shell::ZoomLabel::text"),
        ],
    ),
    // Construction: pure arithmetic over a width and an Instant
    record(
        "H-SHELL-PANEL",
        "Panel",
        Key::UiLabShellPanelName,
        Key::UiLabShellPanelPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_app_shell", PANEL),
        DESKTOP_SHARED,
        &[
            support("koru_app_shell::Panel"),
            support("koru_app_shell::Panel::new"),
            support("koru_app_shell::Panel::is_open"),
            support("koru_app_shell::Panel::layout_width"),
            support("koru_app_shell::Panel::full"),
            support("koru_app_shell::Panel::set_full"),
            support("koru_app_shell::Panel::commit_width"),
            support("koru_app_shell::Panel::slide_offset"),
            support("koru_app_shell::Panel::drawn_width"),
            support("koru_app_shell::Panel::progress"),
            support("koru_app_shell::Panel::animating"),
            support("koru_app_shell::Panel::advance"),
            support("koru_app_shell::Panel::toggle"),
            support("koru_app_shell::Panel::set_immediately"),
            support("koru_app_shell::panel::grip"),
            support("koru_app_shell::panel::GRIP"),
        ],
    ),
    // Construction: pure: in-memory collections generic over the host's tab
    // element; TabSlides is offsets over an Instant
    record(
        "H-SHELL-TABS",
        "TabStrip",
        Key::UiLabShellTabsName,
        Key::UiLabShellTabsPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_app_shell", TABSTRIP),
        DESKTOP_SHARED,
        &[
            support("koru_app_shell::TabStrip"),
            support("koru_app_shell::TabStrip::new"),
            support("koru_app_shell::TabStrip::pin"),
            support("koru_app_shell::TabStrip::unpin"),
            support("koru_app_shell::TabStrip::move_tab"),
            support("koru_app_shell::TabStrip::close_all_unpinned"),
            support("koru_app_shell::TabStrip::restore_pinned"),
            support("koru_app_shell::TabStrip::restore_main"),
            support("koru_app_shell::TabStrip::active_loc"),
            support_with(
                "koru_app_shell::TabLoc",
                &[values(
                    Key::UiLabControlKind,
                    "koru_app_shell::TabGroup",
                    TABSTRIP,
                    Declared::Enum,
                    &[literal("Pinned"), literal("Main")],
                )],
            ),
            support("koru_app_shell::TabGroup"),
            support("koru_app_shell::TabSet"),
            support("koru_app_shell::TabSet::new"),
            support("koru_app_shell::TabSet::empty"),
            support("koru_app_shell::TabSet::restore"),
            support("koru_app_shell::TabSlides"),
            support("koru_app_shell::TabSlides::new"),
            support("koru_app_shell::TabSlides::moved"),
            support("koru_app_shell::TabSlides::settle"),
            support("koru_app_shell::TabSlides::offset"),
            support("koru_app_shell::TabElement"),
        ],
    ),
    // Construction: pure: the collapse ladder over measured widths
    record(
        "H-SHELL-BANNER-STATE",
        "BannerState",
        Key::UiLabShellBannerStateName,
        Key::UiLabShellBannerStatePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_app_shell", BANNER_STATE),
        DESKTOP_SHARED,
        &[
            support("koru_app_shell::BannerState"),
            support("koru_app_shell::BannerState::offered"),
            support("koru_app_shell::BannerState::slots"),
            support("koru_app_shell::BannerState::pane_slots"),
            support("koru_app_shell::BannerState::caption_merged"),
            support("koru_app_shell::BannerState::pane_merged"),
            support_with(
                "koru_app_shell::BannerState::rung",
                &[values(
                    Key::UiLabControlState,
                    "koru_app_shell::Rung",
                    BANNER_STATE,
                    Declared::Enum,
                    &[
                        literal("Full"),
                        literal("Truncated"),
                        literal("IconOnly"),
                        literal("Identity"),
                        literal("NoSettings"),
                    ],
                )],
            ),
            support("koru_app_shell::BannerState::label_width"),
            support("koru_app_shell::BannerState::measured"),
            support("koru_app_shell::BannerState::settle"),
            support("koru_app_shell::BannerState::slot_for"),
            support("koru_app_shell::BannerState::pane_slot_for"),
            support("koru_app_shell::Rung"),
        ],
    ),
    // Construction: needs a host: new(host) takes the application's
    // ShellHost and holds windows only once the multi-window runner opens
    // them; nothing in this crate opens a document, profile or store
    record(
        "H-SHELL-HOST",
        "ShellState",
        Key::UiLabShellHostName,
        Key::UiLabShellHostPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_app_shell", STATE),
        DESKTOP_SHARED,
        &[
            support("koru_app_shell::ShellState"),
            support("koru_app_shell::ShellState::new"),
            support("koru_app_shell::ShellState::host"),
            support("koru_app_shell::ShellState::host_mut"),
            support("koru_app_shell::ShellState::open_window"),
            support("koru_app_shell::ShellState::close_window"),
            support("koru_app_shell::ShellState::window"),
            support("koru_app_shell::ShellState::window_mut"),
            support("koru_app_shell::ShellState::window_keys"),
            support("koru_app_shell::ShellState::set_zoom"),
            support("koru_app_shell::ShellState::select_tab"),
            support("koru_app_shell::ShellState::cycle_tab"),
            support("koru_app_shell::ShellState::close_tab"),
            support("koru_app_shell::ShellState::settle_banner"),
            support_with(
                "koru_app_shell::ShellState::toggle_pane",
                &[values(
                    Key::UiLabControlPlacement,
                    "koru_app_shell::Side",
                    STATE,
                    Declared::Enum,
                    &[
                        labelled("Left", Key::UiLabOptionLeft),
                        labelled("Right", Key::UiLabOptionRight),
                    ],
                )],
            ),
            support("koru_app_shell::ShellState::invalidate"),
            support("koru_app_shell::ShellWindow"),
            support("koru_app_shell::ShellTab"),
            support("koru_app_shell::Side"),
            support("koru_app_shell::MIN_WINDOW_SIZE"),
            support("koru_app_shell::stepped_zoom"),
            support("koru_app_shell::ShellHost"),
            support("koru_app_shell::HostCx"),
            support("koru_app_shell::HostCx::command"),
            support("koru_app_shell::SeatedTab"),
            support("koru_app_shell::SeatedTab::new"),
            support("koru_app_shell::SeatRefused"),
            support("koru_app_shell::PopoverFrame"),
            support("koru_app_shell::Commands"),
            support("koru_app_shell::Commands::push"),
            support("koru_app_shell::ShellAction"),
            support("koru_app_shell::ShellCommand"),
            support("koru_app_shell::command::DRAIN_PASSES"),
            support("koru_app_shell::ProfileKey"),
            support("koru_app_shell::WorkspaceKey"),
            support("koru_app_shell::TabKey"),
            support("koru_app_shell::TabKey::for_test"),
        ],
    ),
    // Construction: a trait a host implements; SharedSurface is an
    // Rc<RefCell<_>> over one
    record(
        "H-SHELL-SURFACE",
        "Surface",
        Key::UiLabShellSurfaceName,
        Key::UiLabShellSurfacePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_app_shell", SURFACE),
        DESKTOP_SHARED,
        &[
            support("koru_app_shell::Surface"),
            support("koru_app_shell::SurfaceCx"),
            support("koru_app_shell::PointerEvent"),
            support("koru_app_shell::PointerEvent::position"),
            support("koru_app_shell::CloseResponse"),
            support("koru_app_shell::SharedSurface"),
            support("koru_app_shell::SharedSurface::new"),
            support("koru_app_shell::SharedSurface::borrow"),
            support("koru_app_shell::SharedSurface::borrow_mut"),
            support("koru_app_shell::SharedSurface::is"),
        ],
    ),
];
