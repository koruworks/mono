// SPDX-License-Identifier: MPL-2.0

//! The page-settings menu, behind the `…` in the title row's gutter.
//!
//! Everything about the *page* rather than about the words on it: how wide it
//! is, what color the canvas around it is, and — not yet — focus mode. All of
//! it is local-only presentation state ([ADR-44-page]) — with one deliberate
//! exception: the **Language** row names the document's prose language, which
//! is a shared fact about the document (spell-check plan D4) and is the one
//! choice here whose dispatch writes to it. It lives in this menu because this
//! is the document's settings menu, and a language that hid in the global
//! Settings tab would read as the *person's* language, which it is not.
//!
//! It replaces the segmented control and the swatch that used to sit at the
//! right-hand end of the toolbar. Those were on screen permanently to offer a
//! choice most documents never change; behind a `…` they cost nothing until
//! they are wanted.
//!
//! **Nothing here closes the menu** — but the one link. Every row is a setting
//! whose result you are looking at — a width you might step through, a color
//! you compare against the page — so choosing one leaves the menu up and you
//! dismiss it when you are done. *Global settings*, beside the heading, leaves
//! the document for the Settings tab, so it closes the menu on its way out. That is the opposite of the block menus, where a choice is an edit
//! and the panel has done its job.
//!
//! [ADR-44-page]: ../../../docs/decisions/README.md

use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::theme::{self, ViewMode};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use crate::menu::{Menu, Row, Trailing};

/// The panel is wider than the editor's pickers: three of its rows carry a
/// trailing control as well as a label, and `PANEL_W` left them touching.
pub const WIDTH: f32 = 268.0;

/// What a click on this menu resolved to.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Choice {
    Mode(ViewMode),
    /// A specific swatch on the page-color row, by index into
    /// [`docview::canvas_bg_presets`](crate::docview::canvas_bg_presets).
    Canvas(usize),
    /// The page-color row itself, away from any swatch: step to the next
    /// preset, the way the old toolbar swatch did.
    CycleCanvas,
    /// A Typography row: step the role's family to the next one. The full
    /// per-family grid lives on the welcome tab; this is the quick in-document
    /// cycle, the same shape as `CycleCanvas`.
    CycleFont(FontRole),
    /// The Language row: step the document's prose language — Default, then
    /// each dictionary language. The same quick-cycle shape as the rows
    /// above; the full chip picker for the *person's* default is in Settings.
    CycleProseLanguage,
    /// *Global settings*, at the right of the heading: the Settings tab, on
    /// its Editor page, where the defaults this menu overrides per page live.
    /// The host opens it; this menu only asks.
    OpenGlobalSettings,
}

/// The next prose language after `current` in the cycle Default → each
/// dictionary language → Default. Beside the row it drives so the two cannot
/// drift.
pub fn next_prose_language(
    current: Option<koru_workspace_host::spelling::SpellLang>,
) -> Option<koru_workspace_host::spelling::SpellLang> {
    use koru_workspace_host::spelling::SpellLang;
    match current {
        None => Some(SpellLang::ALL[0]),
        Some(lang) => {
            let at = SpellLang::ALL.iter().position(|l| *l == lang)?;
            SpellLang::ALL.get(at + 1).copied()
        }
    }
}

/// Which document font role a Typography row cycles. The app (chrome) font is
/// deliberately not here — it is not a document setting.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum FontRole {
    Body,
    Headings,
    Code,
}

/// The three widths, in the order the menu lists them, with the standalone
/// chord that picks each one.
///
/// **`Ctrl+Shift`, not `Ctrl+Alt`.** These were `Ctrl+Alt+P/N/W` and did not
/// fire at all, for a reason the key map already had written down: *on Windows
/// `Ctrl+Alt` is AltGr*, so on most layouts `AltGr+letter` is a different
/// logical key — or a dead key — and `Character("p")` never arrives. That is
/// why `SetHeading` is on `Ctrl+Alt+`**digits**, whose logical key survives.
/// `Ctrl+Shift+letter` is the family alignment, strike and highlight already
/// use, and it has none of that trouble.
///
/// **The container binds one `Ctrl+Alt+letter` — `Ctrl+Alt+W`, the workspace
/// switcher — and that is not a counter-example.** It works only because
/// `alt_letter` in `koru-workspace-host` rebuilds that one letter from its physical
/// key, and it still does not fire on a layout whose AltGr table names `W`
/// ([ADR-122-place](../../../docs/decisions/ADR-101-125.md#adr-122-place--a-new-document-is-placed-by-the-store-and-ctrlaltletter-is-reachable-again)).
/// A width chord that could be missing on somebody's keyboard is a worse trade
/// than one more `Ctrl+Shift`.
///
/// Full width is `G`, not `W` or `F`: `Ctrl+Shift+W` closes every tab and
/// `Ctrl+Shift+F` opens global search in the app container. `F7` still steps
/// through all three, which is the faster gesture once you know there are only
/// three.
///
/// Page's `P` is contextual: the container owns it for switching profiles,
/// while a standalone document has no profile switcher and keeps Page width.
/// `show_page_shortcut` prevents an embedded page menu advertising a command
/// its host intercepts.
const WIDTHS: [(ViewMode, &str, koru_keys::ShortcutId); 3] = [
    (ViewMode::Page, "Page", koru_keys::ShortcutId::ViewModePage),
    (
        ViewMode::Narrow,
        "Narrow",
        koru_keys::ShortcutId::ViewModeNarrow,
    ),
    (
        ViewMode::Full,
        "Full width",
        koru_keys::ShortcutId::ViewModeFull,
    ),
];

/// The letter that picks `mode`, for the key map to match against.
pub fn key_for(mode: ViewMode) -> char {
    match mode {
        ViewMode::Page => 'p',
        ViewMode::Narrow => 'n',
        ViewMode::Full => 'g',
    }
}

/// The mode a `Ctrl+Alt+<letter>` chord selects, if it selects one.
pub fn mode_for_key(letter: char) -> Option<ViewMode> {
    [ViewMode::Page, ViewMode::Narrow, ViewMode::Full]
        .into_iter()
        .find(|m| key_for(*m) == letter)
}

/// The rows, paired with what each one does.
///
/// One list rather than a `rows` function and a `choice_at` reading indices out
/// of consts: the page-color row exists only in Page mode, so every index after
/// it moves with the mode — which is exactly the drift two parallel lists
/// invite.
///
/// `settings_link` is whether the host has a Settings tab to open: a document
/// in a window of its own has none, and gets the plain heading rather than a
/// link that would do nothing. Either way the heading is row 0, so no other
/// row's index moves with it.
pub fn rows(
    mode: ViewMode,
    canvas: usize,
    prose_language: Option<koru_workspace_host::spelling::SpellLang>,
    lang: koru_i18n::Lang,
    settings_link: bool,
) -> Vec<(Row, Option<Choice>)> {
    let page = mode == ViewMode::Page;
    let heading = match settings_link {
        true => (
            Row::HeadingLink {
                heading: "PAGE SETTINGS",
                link: koru_i18n::t(lang, koru_i18n::Key::PageMenuGlobalSettings),
            },
            Some(Choice::OpenGlobalSettings),
        ),
        false => (Row::Heading("PAGE SETTINGS"), None),
    };
    let mut out: Vec<(Row, Option<Choice>)> = vec![heading];
    for (m, label, chord) in WIDTHS {
        out.push((
            Row::item(label)
                .with_shortcut(crate::menu::Chord::Id(chord))
                .checked(mode == m),
            Some(Choice::Mode(m)),
        ));
        // The color belongs to Page, so it sits *under* Page rather than at the
        // bottom of the menu — a setting of the row above it, not a fourth
        // width beside them. It is always listed and **dimmed** off Page rather
        // than appearing and disappearing: a row that comes and goes moves
        // everything below it, and the canvas is a Page concept (ADR-44-page),
        // so saying "this belongs to the mode above" is the honest shape.
        //
        // No icon: its swatches already say what it is, and a page diagram
        // beside them would claim it was a fourth width.
        if m == ViewMode::Page {
            out.push((
                Row::item("Color")
                    .with_trailing(Trailing::Swatches(canvas))
                    .enabled(page),
                page.then_some(Choice::CycleCanvas),
            ));
        }
    }
    out.push((Row::Divider, None));
    // Typography: the three document font roles. Each row shows the role's
    // current family as a trailing note and cycles to the next on click — the
    // quick in-document switch. The full per-family grid is on the welcome tab.
    out.push((Row::Heading("TYPOGRAPHY"), None));
    for (role, label) in [
        (FontRole::Body, "Body"),
        (FontRole::Headings, "Headings"),
        (FontRole::Code, "Code"),
    ] {
        let current = match role {
            FontRole::Body => theme::body_font(),
            FontRole::Headings => theme::headings_font(),
            FontRole::Code => theme::code_font(),
        };
        out.push((
            Row::item(label).with_trailing(Trailing::Note(current.label())),
            Some(Choice::CycleFont(role)),
        ));
    }
    out.push((Row::Divider, None));
    // The document's prose language — the spelling override collaborators
    // share (D4). The trailing note names what is in force; a click cycles.
    out.push((
        Row::item(koru_i18n::t(lang, koru_i18n::Key::Language)).with_trailing(Trailing::Note(
            prose_language.map_or_else(
                || koru_i18n::t(lang, koru_i18n::Key::SpellingDefaultLanguage),
                koru_workspace_host::spelling::SpellLang::endonym,
            ),
        )),
        Some(Choice::CycleProseLanguage),
    ));
    out.push((Row::Divider, None));
    out.push((
        Row::item("Focus mode")
            .with_trailing(Trailing::Toggle(false))
            .enabled(false),
        None,
    ));
    out
}

/// Just the rows, for the geometry.
pub fn shape(
    mode: ViewMode,
    canvas: usize,
    prose_language: Option<koru_workspace_host::spelling::SpellLang>,
    lang: koru_i18n::Lang,
    settings_link: bool,
) -> Vec<Row> {
    rows(mode, canvas, prose_language, lang, settings_link)
        .into_iter()
        .map(|(r, _)| r)
        .collect()
}

/// What row `i` does, or `None` for a row that is furniture. The language and
/// UI-language arguments never change the row *set*, so any values answer —
/// and the link is asked for, because row 0 can only be chosen where it is
/// drawn as one.
pub fn choice_at(mode: ViewMode, i: usize) -> Option<Choice> {
    rows(mode, 0, None, koru_i18n::Lang::En, true)
        .get(i)
        .and_then(|(_, c)| *c)
}

/// Whether row `i` is the page-color row, whose swatches are hit individually.
pub fn is_color_row(mode: ViewMode, i: usize) -> bool {
    matches!(choice_at(mode, i), Some(Choice::CycleCanvas))
}

/// Whether row `i` draws no leading art at all. Only the color row: its
/// swatches say what it is, and a page diagram beside them would read as a
/// fourth width.
fn is_bare_row(i: usize) -> bool {
    matches!(
        rows(ViewMode::Page, 0, None, koru_i18n::Lang::En, true).get(i),
        Some((Row::Item { label, .. }, _)) if label == "Color"
    )
}

/// The little page diagram in the leading column of a width row.
///
/// A sheet outline with a filled band showing how much of it the text uses —
/// which is the whole difference between the three modes, said without words.
/// Drawn rather than set: Phosphor has no page-width glyph, and `docs/features/design/DESIGN.md`
/// forbids inventing a codepoint.
pub fn draw_width_icon(scene: &mut Scene, root: Affine, i: usize, r: Rect, color: Color) {
    if is_bare_row(i) {
        return;
    }
    let (w, h) = (16.0, 12.0);
    let x = r.x + (r.w - w) * 0.5;
    let y = r.y + (r.h - h) * 0.5;
    scene::stroke(scene, root, color, 1.0, &scene::rrect(x, y, w, h, 2.0));
    // How wide the text column runs inside the sheet. Keyed off the row's own
    // *mode* rather than its index, which moves with the color row.
    let inset = match choice_at(ViewMode::Page, i) {
        Some(Choice::Mode(ViewMode::Page)) => 4.0,
        Some(Choice::Mode(ViewMode::Narrow)) => 5.0,
        _ => 2.0,
    };
    scene::fill(
        scene,
        root,
        color,
        &scene::rect(x + inset, y + 3.0, w - inset * 2.0, h - 6.0),
    );
}

/// The `…` button in the title row's gutter, which opens this menu.
///
/// Its rect is the title row's `+` slot — the same lane the insert button and
/// the drag grip use on every other row, which is what makes the title's text
/// line up with the body's rather than sitting one button further right.
pub fn draw_button(scene: &mut Scene, root: Affine, r: Rect, hovered: bool) {
    let pal = theme::p();
    if hovered {
        scene::fill(
            scene,
            root,
            pal.hover_tint,
            &scene::rrect(r.x, r.y, r.w, r.h, 5.0),
        );
    }
    scene::dots_three(
        scene,
        root,
        r,
        if hovered { pal.text } else { pal.row_hint },
    );
}

/// The swatch colors the page-color row offers, resolved for this theme.
pub fn swatch_colors() -> Vec<Color> {
    crate::docview::canvas_bg_presets()
        .into_iter()
        .map(|c| match c {
            Some(c) => Color::from_rgba8(c[0], c[1], c[2], c[3]),
            // "Auto" — whatever the theme's canvas currently is.
            None => theme::p().canvas,
        })
        .collect()
}

/// A menu anchored under `button`.
pub fn open(button: Rect) -> Menu {
    Menu::new(button, WIDTH)
}

#[cfg(test)]
mod tests;
