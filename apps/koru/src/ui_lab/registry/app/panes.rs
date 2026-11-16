// SPDX-License-Identifier: MPL-2.0

//! **The workspace window's side columns** (`apps/koru/src/panes`): the
//! sidebar's apps rail, the explore column with its navigator modes, the
//! Tasks rail, the Home rows and page, the switcher and help popovers, and
//! the context column. `registry/MANIFEST.md` lists how deeply each file
//! was read and what COMPONENTS.md's WF rows turned out to be.

use super::*;

const CONTEXT: &str = "apps/koru/src/panes/context.rs";
const EXPLORE: &str = "apps/koru/src/panes/explore.rs";
const HOME: &str = "apps/koru/src/panes/home/mod.rs";
const HOME_NAV: &str = "apps/koru/src/panes/explore/home_nav.rs";
const POPOVER_ELEMENTS: &str = "apps/koru/src/panes/explore/popover_elements.rs";
const RAIL: &str = "apps/koru/src/panes/explore/rail.rs";
const RAIL_ELEMENTS: &str = "apps/koru/src/panes/sidebar/rail_elements.rs";

/// Every record of the side columns, in navigator order.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: needs the container: rows come from the workspace's
    // library, the chats and runs lists and the canvas index it feeds in
    record(
        "C-EXPLORE",
        "ExploreView",
        Key::UiLabExploreName,
        Key::UiLabExplorePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes::explore", EXPLORE),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::explore::ExploreView"),
            painter("koru::panes::explore::ExploreView::draw"),
            painter("koru::panes::explore::actions::draw_dots"),
            support("koru::panes::explore::actions::Action"),
            support_with(
                "koru::panes::explore::NavigatorMode",
                &[values(
                    Key::UiLabControlMode,
                    "koru::panes::explore::NavigatorMode",
                    EXPLORE,
                    Declared::Enum,
                    &[
                        literal("Home"),
                        literal("Documents"),
                        literal("Files"),
                        literal("Tasks"),
                        literal("Chats"),
                        literal("Agents"),
                        literal("Artifacts"),
                        literal("Canvases"),
                        literal("Socials"),
                    ],
                )],
            ),
            support("koru::panes::explore::SyncStatus"),
            support("koru::panes::explore::ExploreChromeFocus"),
            support("koru::panes::explore::accessibility::Context"),
        ],
    ),
    // Construction: pure: items the container projects from the open
    // workspaces, the person's view order and the pinned mini apps
    record(
        "C-APPS-RAIL",
        "rail_elements::Rail",
        Key::UiLabAppsRailName,
        Key::UiLabAppsRailPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes", RAIL_ELEMENTS),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::sidebar::rail_elements::Rail"),
            painter("koru::panes::sidebar::rail_elements::Layout"),
            painter("koru::panes::explore::ExploreView::draw_apps_rail"),
            support_with(
                "koru::panes::sidebar::rail_elements::Contents",
                &[values(
                    Key::UiLabControlKind,
                    "koru::panes::sidebar::rail_elements::Contents",
                    RAIL_ELEMENTS,
                    Declared::Enum,
                    &[literal("Tile"), literal("Glyph"), literal("NotifiedGlyph")],
                )],
            ),
            support("koru::panes::sidebar::apps_rail::Hit"),
            support("koru::panes::sidebar::apps_rail::Rects"),
            support("koru::panes::sidebar::rail_order::RailOrder"),
        ],
    ),
    // Construction: pure: the buckets, the workspace's lists and counts
    record(
        "C-TASKS-RAIL",
        "TasksRail",
        Key::UiLabTasksRailName,
        Key::UiLabTasksRailPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes::explore", RAIL),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::explore::rail::TasksRail"),
            support("koru::panes::explore::rail::RailTarget"),
            support("koru::panes::explore::rail::Bucket"),
            support_with(
                "koru::panes::explore::rail::RailRow",
                &[values(
                    Key::UiLabControlKind,
                    "koru::panes::explore::rail::RailRow",
                    RAIL,
                    Declared::Enum,
                    &[
                        literal("Bucket"),
                        literal("Reminders"),
                        literal("ListsHeading"),
                        literal("List"),
                        literal("NewList"),
                    ],
                )],
            ),
            support("koru::panes::explore::rail::TasksRailRequest"),
        ],
    ),
    // Construction: pure: a fixed set of rows and the recent documents
    record(
        "C-HOME-NAV",
        "HomeNav",
        Key::UiLabHomeNavName,
        Key::UiLabHomeNavPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes::explore", HOME_NAV),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::explore::home_nav::HomeNav"),
            painter("koru::panes::explore::home_nav::HomeNav::draw"),
            support_with(
                "koru::panes::explore::home_nav::HomeRow",
                &[values(
                    Key::UiLabControlKind,
                    "koru::panes::explore::home_nav::HomeRow",
                    HOME_NAV,
                    Declared::Enum,
                    &[
                        literal("Inbox"),
                        literal("Today"),
                        literal("Upcoming"),
                        literal("RecentHeading"),
                        literal("Recent"),
                    ],
                )],
            ),
        ],
    ),
    // Construction: pure: fed by the container each frame, asking for what
    // it needs through HomeWant
    record(
        "C-HOME-PAGE",
        "HomePage",
        Key::UiLabHomePageName,
        Key::UiLabHomePagePurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes::home", HOME),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::home::HomePage"),
            support("koru::panes::home::HomeWant"),
            support("koru::panes::home::Target"),
        ],
    ),
    // Construction: pure element trees over the container's workspace and
    // Profile lists and the update feed
    record(
        "C-EXPLORE-POPOVERS",
        "PopoverKind",
        Key::UiLabExplorePopoversName,
        Key::UiLabExplorePopoversPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes::explore", POPOVER_ELEMENTS),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::explore::ExploreView::draw_popover"),
            painter("koru::panes::explore::popover_elements::TileLeaf"),
            painter("koru::panes::explore::popover_elements::TrailingAdapter"),
            painter("koru::panes::explore::popover_elements::profiles::ProfileTileLeaf"),
            support_with(
                "koru::panes::explore::PopoverKind",
                &[values(
                    Key::UiLabControlKind,
                    "koru::panes::explore::PopoverKind",
                    EXPLORE,
                    Declared::Enum,
                    &[
                        literal("Help"),
                        literal("Summary"),
                        literal("Profiles"),
                        literal("LibraryActions"),
                    ],
                )],
            ),
            support("koru::panes::explore::PopoverAnchor"),
            support("koru::panes::explore::help::Model"),
            support("koru::panes::explore::help::project"),
            support("koru::panes::explore::workspace_menu_elements::MenuAction"),
        ],
    ),
    // Construction: pure: the subject node's history, comments, assets,
    // reminders, tasks and Cloud sync, each pushed in by the container
    record(
        "C-CONTEXT-COLUMN",
        "ContextView",
        Key::UiLabContextColumnName,
        Key::UiLabContextColumnPurpose,
        Family::Compositions,
        Kind::AppLocal,
        None,
        &[],
        source(KORU, "koru::panes::context", CONTEXT),
        DESKTOP_SHARED,
        &[
            painter("koru::panes::context::ContextView"),
            painter("koru::panes::context::ContextView::draw"),
            support_with(
                "koru::panes::context::Section",
                &[values(
                    Key::UiLabControlKind,
                    "koru::panes::context::Section",
                    CONTEXT,
                    Declared::Enum,
                    &[
                        literal("Reminders"),
                        literal("Comments"),
                        literal("Assets"),
                        literal("Tasks"),
                        literal("History"),
                        literal("CloudSync"),
                    ],
                )],
            ),
        ],
    ),
];
