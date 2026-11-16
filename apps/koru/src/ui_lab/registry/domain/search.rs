// SPDX-License-Identifier: MPL-2.0

//! **`koru-search-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-search-ui";

const ENTITY_SEARCH: &str = "crates/koru-search-ui/src/entity_search.rs";
const LIB: &str = "crates/koru-search-ui/src/lib.rs";
const MODAL: &str = "crates/koru-search-ui/src/modal.rs";
const OFFER: &str = "crates/koru-search-ui/src/offer.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: new(viewport) computes the panel rect (640 by 400 at
    // most, inside 16-unit margins, top at 96 when it fits) and stores it with
    // the viewport; draw borrows a host's &mut SearchSurface, resizes it to
    // the panel and paints scrim, floating panel, clipped surface and border
    record(
        "C-SEARCH-MODAL",
        "SearchModal",
        Key::UiLabSearchOverlayName,
        Key::UiLabSearchOverlayPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        Some("C-OVERLAY"),
        &["SearchModal"],
        source(OWNER, "koru_search_ui", MODAL),
        [
            (Platform::Windows, Availability::Existing),
            (Platform::MacOs, Availability::SharedSourceUnverified),
            (Platform::Linux, Availability::SharedSourceUnverified),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            painter("koru_search_ui::SearchModal"),
            painter("koru_search_ui::SearchModal::new"),
            support("koru_search_ui::SearchModal::bounds"),
            painter("koru_search_ui::SearchModal::draw"),
        ],
    ),
    // Construction: services and side effects: new, new_at, for_workspace and
    // for_workspace_at all go through with_scope_at (lib.rs:575), which starts
    // SearchReader (two named threads dialling the store on this channel, or
    // at --root) unless cfg(test) or the no-reader feature, asks it for the
    // catalog, and seeds query, sort, profile and selection from the process
    // session state (state::search_session, which reads the persisted choices
    // file once a host has named one with persist_choices_in); Default is
    // new()
    record(
        "C-SEARCH-SURFACE",
        "SearchSurface",
        Key::UiLabSearchPanelName,
        Key::UiLabSearchPanelPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &["Reading offer"],
        source(OWNER, "koru_search_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            // The one line about pictures and scans (reconciled against
            // main, 17 November 2026): what it says and the answers under it.
            support("koru_search_ui::LineView"),
            support_with(
                "koru_search_ui::LineAction",
                &[values(
                    Key::UiLabControlKind,
                    "koru_search_ui::LineAction",
                    OFFER,
                    Declared::Enum,
                    &[
                        literal("TurnOn"),
                        literal("NotNow"),
                        literal("Stop"),
                        literal("TryAgain"),
                    ],
                )],
            ),
            support("koru_search_ui::import_summary"),
            support("koru_search_ui::install_line"),
            painter_with(
                "koru_search_ui::SearchSurface",
                &[values(
                    Key::UiLabControlMode,
                    "koru_search_ui::SearchSurface",
                    LIB,
                    Declared::Functions,
                    &[
                        labelled("new", Key::UiLabOptionAllWorkspaces),
                        labelled("for_workspace", Key::UiLabOptionOneWorkspace),
                    ],
                )],
            ),
            painter("koru_search_ui::SearchSurface::new"),
            painter("koru_search_ui::SearchSurface::new_at"),
            painter("koru_search_ui::SearchSurface::for_workspace"),
            painter("koru_search_ui::SearchSurface::for_workspace_at"),
            painter("koru_search_ui::SearchSurface::draw"),
            support("koru_search_ui::SearchSurface::key"),
            support("koru_search_ui::SearchSurface::mouse_down"),
            support("koru_search_ui::SearchSurface::mouse_move"),
            support("koru_search_ui::SearchSurface::mouse_up"),
            support("koru_search_ui::SearchSurface::mouse_double_click"),
            support("koru_search_ui::SearchSurface::scroll_by"),
            support("koru_search_ui::SearchSurface::set_viewport"),
            support("koru_search_ui::SearchSurface::cursor_icon"),
            support("koru_search_ui::SearchSurface::maintain"),
            support("koru_search_ui::SearchSurface::animating"),
            support("koru_search_ui::SearchSurface::resume"),
            support("koru_search_ui::SearchSurface::resume_in_workspace"),
            support("koru_search_ui::SearchSurface::workspace_scope"),
            support("koru_search_ui::SearchSurface::set_pinned"),
            support("koru_search_ui::SearchSurface::set_footer_chips"),
            support("koru_search_ui::SearchSurface::set_create_rows"),
            support("koru_search_ui::SearchSurface::take_create"),
            support("koru_search_ui::SearchSurface::set_placeholder"),
            support("koru_search_ui::SearchSurface::set_availability_warning"),
            support("koru_search_ui::SearchSurface::set_host_warning"),
            support("koru_search_ui::SearchSurface::dictation_field"),
            support("koru_search_ui::SearchSurface::take_target"),
            support("koru_search_ui::SearchSurface::take_window_request"),
            support_with(
                "koru_search_ui::Target",
                &[values(
                    Key::UiLabControlKind,
                    "koru_search_ui::Target",
                    LIB,
                    Declared::Enum,
                    &[literal("Store"), literal("Chat")],
                )],
            ),
            support_with(
                "koru_search_ui::Disposition",
                &[values(
                    Key::UiLabControlValues,
                    "koru_search_ui::Disposition",
                    LIB,
                    Declared::Enum,
                    &[literal("CurrentWindow"), literal("NewWindow")],
                )],
            ),
            support("koru_search_ui::CreateRow"),
            support("koru_search_ui::FooterChip"),
            support("koru_search_ui::Create"),
            support("koru_search_ui::SURFACE_RADIUS"),
            support("koru_search_ui::SearchSurface::maintenance_deadline"),
        ],
    ),
    // Construction: services: new_at(root) starts a SearchReader (two threads
    // dialling the store) and asks it for the catalog unless cfg(test) or the
    // no-reader feature; new() is new_at(None), which in a normal build dials
    // the channel's default store
    record(
        "H-ENTITY-SEARCH",
        "EntitySearch",
        Key::UiLabSearchDocumentsName,
        Key::UiLabSearchDocumentsPurpose,
        Family::Controls,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_search_ui", ENTITY_SEARCH),
        DESKTOP_NOT_ON_PHONES,
        &[
            host_only("koru_search_ui::EntitySearch"),
            support("koru_search_ui::EntitySearch::new"),
            host_only("koru_search_ui::EntitySearch::new_at"),
            host_only("koru_search_ui::EntitySearch::request"),
            host_only("koru_search_ui::EntitySearch::poll"),
            host_only("koru_search_ui::EntitySearch::pending"),
            support("koru_search_ui::EntitySearch::catalog_workspaces"),
            support("koru_search_ui::EntitySearch::request_outline"),
            support("koru_search_ui::EntitySearch::poll_outline"),
            support("koru_search_ui::EntitySearch::request_document_faces"),
            support("koru_search_ui::EntitySearch::poll_document_faces"),
            host_only("koru_search_ui::EntitySearchUpdate"),
            support("koru_search_ui::EntityDocument"),
            support("koru_search_ui::EntityWorkspace"),
        ],
    ),
];
