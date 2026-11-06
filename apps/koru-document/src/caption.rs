// SPDX-License-Identifier: MPL-2.0

//! The window's own title bar, for a document opened on its own.
//!
//! ```text
//!                                                    ─   □   ✕
//!  ↶ ↷ │ H1 H2 H3 H4 │ B I U S </> ✏ │ x² x₂ │ ≡ ≡ ≡        spec.md
//!                                                    (or a single ⋮)
//! ```
//!
//! `koru-workspace-host` opens every window undecorated ([ADR-25]) on the understanding
//! that the view draws its own caption. The container does; a single-view binary
//! did not, so `cargo run -p koru-document` had no way to minimise, maximise,
//! close or drag its window, and Alt+F4 was the only way out.
//!
//! **The controls are `koru_ui::caption`'s, not a second copy.** A window
//! button is in the same place here as in the container, hit-tested and painted
//! by the same widget — the rule in `docs/features/design/DESIGN.md`, and the reason it is a
//! rule. `koru-shell-chrome` still owns only the surrounding tab geometry.
//!
//! [ADR-25]: ../../../docs/decisions/README.md

use vello::Scene;
use vello::kurbo::Affine;

use koru_draw::geometry::Rect;
use koru_draw::scene::{fill, rect};
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_ui::caption::{self as controls, Action};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Caption,
    Control(Action),
}

/// The caption row, in window coordinates. The formatting toolbar sits directly
/// underneath it.
pub fn strip(viewport_w: f32) -> Rect {
    Rect::new(0.0, 0.0, viewport_w, theme::CAPTION_H)
}

/// What a point in window coordinates lands on, or `None` below the strip.
///
/// [`Hit::Caption`] is the bare row, while `None` means below/outside it.
pub fn hit(x: f32, y: f32, viewport_w: f32) -> Option<Hit> {
    let cap = strip(viewport_w);
    if y < cap.y || y >= cap.y + cap.h || x < cap.x || x >= cap.x + cap.w {
        return None;
    }
    Some(controls::hit(cap, x, y).map_or(Hit::Caption, Hit::Control))
}

/// Where the window's name starts.
///
/// **After the traffic lights on a Mac.** The window's titlebar is
/// transparent there and AppKit draws its lights over the caption's left end
/// (`koru_window::traffic_lights`), so the name starts after the same room
/// the container's lane leaves for them. Elsewhere it takes the inset the
/// button at the other end gives its glyph (`theme::CAPTION_PAD_X`).
fn title_x(cap: Rect) -> f32 {
    if cfg!(target_os = "macos") {
        cap.x + koru_window::traffic_lights::LANE_W
    } else {
        cap.x + theme::CAPTION_PAD_X
    }
}

/// The **Offline** chip's rect, when there is something to say.
///
/// Just inside the window buttons, so it reads as belonging to the same row
/// without being one of them. `None` when the store is fine, and the title then
/// takes the room back.
///
/// **This window needs its own, and that was the bug.** A popped-out document
/// draws this caption instead of the container's, so for a while it was the one
/// window in Koru that never said the store was away — while being exactly the
/// window somebody puts a document into to concentrate on it.
pub fn offline_chip(viewport_w: f32, offline: bool) -> Option<Rect> {
    if !offline {
        return None;
    }
    let cap = strip(viewport_w);
    let buttons_left = controls::controls(cap)
        .first()
        .map_or(cap.x + cap.w, |control| control.rect.x);
    let band = Rect::new(
        cap.x,
        cap.y,
        (buttons_left - cap.x - theme::CAPTION_PAD_X).max(0.0),
        cap.h,
    );
    (band.w > 0.0).then(|| koru_shell_chrome::status_chip::chip(band))
}

/// This window's display language, resolved once.
///
/// **Resolved here rather than passed in**, because this crate is both a
/// library the container hosts and a binary of its own, and only one of those
/// two has an app-wide language global to borrow. Both arms come out the same:
/// `apps/koru` sets its own from `Lang::for_system(system_locale())` at
/// startup, which is exactly this expression.
///
/// **Cached, because the caption is drawn every frame.** `system_locale` is an
/// OS call, and `AGENTS.md`'s standing rule is that nothing asks the OS about
/// the window on a timer.
pub(crate) fn lang() -> koru_i18n::Lang {
    static LANG: std::sync::OnceLock<koru_i18n::Lang> = std::sync::OnceLock::new();
    *LANG.get_or_init(|| {
        koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref())
    })
}

/// The status chip's detail line, for what this window's workspace knows.
///
/// **A map with no words in it.** Every sentence and the branching between them
/// live in `koru_i18n::store`, which the container reads through the matching
/// function in `apps/koru/src/mainview/draw.rs`. The two apps share crates and
/// nothing else, so this is the thinnest thing that can be duplicated without
/// the two captions being able to drift — and
/// `the_container_and_this_window_word_the_chip_identically` asserts they have
/// not.
pub(crate) fn store_detail(
    lang: koru_i18n::Lang,
    trouble: &koru_workspace::StoreTrouble,
) -> String {
    koru_i18n::store::popover_detail(
        lang,
        koru_i18n::store::Trouble {
            documents: trouble.unconfirmed.documents,
            document_list: trouble.unconfirmed.document_list,
            not_buffered: trouble.unconfirmed.not_buffered.is_some(),
            fatal: trouble.fatal,
            message: &trouble.message,
        },
    )
}

/// The button's rect, when that is what the caption is offering.
///
/// `None` with the experimental menu off, because there is no such button then —
/// the same absent-rather-than-empty answer the control list gives.
pub fn window_menu_button(viewport_w: f32) -> Option<Rect> {
    controls::controls(strip(viewport_w))
        .into_iter()
        .find(|control| control.action == Action::WindowMenu)
        .map(|control| control.rect)
}

/// Draw the row and its buttons.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    viewport_w: f32,
    pointer: (f32, f32),
    maximized: bool,
    _pinned: bool,
    menu_morph: f32,
    menu_open: bool,
    title: &str,
    search: Option<&koru_editor::SearchHighlight>,
    bar_bg: vello::peniko::Color,
    offline: Option<(&str, bool)>,
) {
    let p = theme::p();
    let cap = strip(viewport_w);
    // The page's own colour, handed down — see `draw_chrome`. Not `status_bg`:
    // a grey strip made the window look like a frame around the document rather
    // than the top of it.
    fill(scene, root, bar_bg, &rect(cap.x, cap.y, cap.w, cap.h));

    // **The document's name, in the bar.** A standalone window turns
    // `Body::title_row` off — the name is the window's here, not the first line
    // of the page — so this is the only place it appears.
    //
    // **Left, not centred.** A centred name drifts as the window is resized and
    // as the name is edited, which puts the one fixed label in the window on a
    // moving target; and the page it names starts on the left. Wrapped to the
    // room before the buttons, so a long name truncates rather than running
    // under them.
    let chip = offline_chip(viewport_w, offline.is_some());
    if !title.is_empty() {
        // The chip takes precedence over the name: a truncated title is a
        // nuisance and an unsaid "Offline" is somebody's afternoon.
        let buttons_left = chip.map_or_else(
            || {
                controls::controls(cap)
                    .first()
                    .map_or(cap.x + cap.w, |control| control.rect.x)
            },
            |c| c.x,
        );
        let x = title_x(cap);
        let avail = (buttons_left - x - theme::CAPTION_PAD_X).max(0.0);
        if avail > 0.0 {
            let laid = engine.simple_layout_wrapped(title, theme::role::row(), avail);
            let y = cap.y + (cap.h - laid.height()) * 0.5;
            if let Some(highlight) = search {
                koru_editor::draw::draw_search_range(scene, root, &laid, highlight, x, y);
            }
            koru_draw::scene::draw_layout_first_line(scene, root, &laid, x, y, Some(p.text));
        }
    }

    if let (Some(chip), Some((detail, open))) = (chip, offline) {
        // **The words are this window's, not the chip's** — see the container's
        // matching call in `apps/koru/src/mainview/draw.rs`. All three lines
        // come out of the same `koru-i18n` rows there and here, so the two
        // captions cannot say one fact two ways; `detail` was assembled from
        // the same rows by [`store_detail`], up in `window_chrome`.
        let lang = lang();
        koru_shell_chrome::status_chip::draw(
            scene,
            root,
            engine,
            chip,
            koru_shell_chrome::status_chip::Text {
                label: koru_i18n::t(lang, koru_i18n::Key::StoreOfflineChip),
                title: koru_i18n::t(lang, koru_i18n::Key::StoreOfflineTitle),
                detail,
            },
            open,
        );
    }

    controls::draw(
        scene,
        root,
        engine,
        cap,
        controls::State {
            hovered: hit(pointer.0, pointer.1, viewport_w).and_then(|hit| match hit {
                Hit::Caption => None,
                Hit::Control(action) => Some(action),
            }),
            maximized,
            menu: controls::MenuVisual::Morph {
                progress: menu_morph,
                open: menu_open,
            },
        },
    );
}

#[cfg(test)]
mod tests;
