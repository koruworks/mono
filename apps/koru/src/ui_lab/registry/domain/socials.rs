// SPDX-License-Identifier: MPL-2.0

//! **`koru-socials-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-socials-ui";

const LIB: &str = "crates/koru-socials-ui/src/lib.rs";
const NAVIGATION: &str = "crates/koru-socials-ui/src/navigation.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: side effects: Surface::new(root) builds a TextEngine and
    // eleven TextInput/TextArea fields, then backend::Backend::new(root),
    // which spawns the koru-socials-store worker thread (Store client,
    // workspace watch) unless the crate is built with cfg(test) or the
    // no-reader feature, in which case no thread is spawned and requests go
    // unanswered. The browser opener defaults to one that always fails. Under
    // no-reader, preview_fixture(view) returns a Surface filled with synthetic
    // channels and posts for the 'calendar', 'delivery', 'unconfirmed',
    // 'accounts…' and composer views.
    record(
        "C-SOCIALS-SURFACE",
        "Surface",
        Key::UiLabSocialsSurfaceName,
        Key::UiLabSocialsSurfacePurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_socials_ui", LIB),
        [
            (Platform::Windows, Availability::Existing),
            (Platform::MacOs, Availability::SharedSourceUnverified),
            (Platform::Linux, Availability::SharedSourceUnverified),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            painter("koru_socials_ui::Surface"),
            support("koru_socials_ui::Surface::new"),
            support("koru_socials_ui::SharedSurface"),
            painter("koru_socials_ui::Surface::draw"),
            support("koru_socials_ui::Surface::set_viewport"),
            support("koru_socials_ui::Surface::set_context_workspace"),
            support("koru_socials_ui::Surface::set_browser_opener"),
            support("koru_socials_ui::Surface::maintain"),
            support("koru_socials_ui::Surface::key"),
            support("koru_socials_ui::Surface::mouse_down"),
            support("koru_socials_ui::Surface::mouse_move"),
            support("koru_socials_ui::Surface::mouse_up"),
            support("koru_socials_ui::Surface::mouse_double_click"),
            support("koru_socials_ui::Surface::scroll_by"),
            support("koru_socials_ui::Surface::cursor_icon"),
            support("koru_socials_ui::Surface::is_dragging"),
            support("koru_socials_ui::Surface::request_close"),
            support("koru_socials_ui::Surface::save_draft"),
            support("koru_socials_ui::Surface::import_document"),
            support("koru_socials_ui::Surface::invalidate"),
            support("koru_socials_ui::Surface::maintenance_deadline"),
        ],
    ),
    // Construction: no constructor of its own: the private
    // navigation::Navigation state is built by Default
    // (SelectableList::new(32.), empty sets) inside Surface::new, so building
    // it carries Surface::new's side effects (see C-SOCIALS-SURFACE). Counts
    // are refreshed from cached posts in Surface::maintain; drawing does not
    // query the Store.
    record(
        "C-SOCIALS-NAVIGATOR",
        "NavigationOutcome",
        Key::UiLabSocialsNavigatorName,
        Key::UiLabSocialsNavigatorPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_socials_ui", NAVIGATION),
        [
            (Platform::Windows, Availability::Existing),
            (Platform::MacOs, Availability::SharedSourceUnverified),
            (Platform::Linux, Availability::SharedSourceUnverified),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            painter("koru_socials_ui::Surface::navigation_draw"),
            support("koru_socials_ui::Surface::navigation_mouse_down"),
            support("koru_socials_ui::Surface::navigation_mouse_move"),
            support("koru_socials_ui::Surface::navigation_scroll"),
            support("koru_socials_ui::Surface::navigation_key"),
            support("koru_socials_ui::Surface::navigation_focus"),
            support("koru_socials_ui::Surface::navigation_blur"),
            support("koru_socials_ui::Surface::navigation_has_keyboard"),
            support("koru_socials_ui::NavigationOutcome"),
        ],
    ),
];
