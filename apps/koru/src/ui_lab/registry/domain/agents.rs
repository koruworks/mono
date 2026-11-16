// SPDX-License-Identifier: MPL-2.0

//! **`koru-agents-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-agents-ui";

const SURFACE: &str = "crates/koru-agents-ui/src/surface.rs";
const WATCHING: &str = "crates/koru-agents-ui/src/watching.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: needs a host: new(sender, stats, lang, composer_visible,
    // cursor) takes the host's SurfaceInteraction handle to the session
    // backend and shared SessionStats; it spawns nothing and opens nothing
    // itself
    record(
        "C-SESSION-SURFACE",
        "SessionSurface",
        Key::UiLabSessionSurfaceName,
        Key::UiLabSessionSurfacePurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_agents_ui", SURFACE),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_agents_ui::SessionSurface"),
            painter("koru_agents_ui::SessionSurface::new"),
            painter("koru_agents_ui::SessionSurface::draw_pane"),
            painter("koru_agents_ui::SessionSurface::draw_watch_band"),
            support("koru_agents_ui::SessionSurface::split"),
            support("koru_agents_ui::SessionSurface::grid_size_for"),
            support("koru_agents_ui::SessionSurface::apply_render"),
            support("koru_agents_ui::SessionSurface::terminal_point"),
            support("koru_agents_ui::PaneStatus"),
            support("koru_agents_ui::PaneFrame"),
            support("koru_agents_ui::RenderOutcome"),
            support("koru_agents_ui::SurfaceFocus"),
            support("koru_agents_ui::CURSOR_BLINK_INTERVAL"),
            support("koru_agents_ui::SurfaceInteraction"),
        ],
    ),
    // Construction: pure: a free painter over a Band value; its words come
    // from koru-i18n
    record(
        "C-WATCH-BAND",
        "Watching",
        Key::UiLabWatchBandName,
        Key::UiLabWatchBandPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_agents_ui::watching", WATCHING),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter_with(
                "koru_agents_ui::watching::draw",
                &[values(
                    Key::UiLabControlState,
                    "koru_agents_ui::watching::Watching",
                    WATCHING,
                    Declared::Enum,
                    &[
                        labelled("No", Key::UiLabOptionNotWatching),
                        labelled("Yes", Key::UiLabOptionWatching),
                        labelled("Asked", Key::UiLabOptionTakeoverAsked),
                    ],
                )],
            ),
            support("koru_agents_ui::watching::Watching"),
            support("koru_agents_ui::watching::Band"),
            support("koru_agents_ui::watching::layout"),
            support("koru_agents_ui::watching::Layout"),
            support("koru_agents_ui::watching::FOOTER_H"),
        ],
    ),
];
