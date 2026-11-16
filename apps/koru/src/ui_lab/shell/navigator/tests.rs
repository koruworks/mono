// SPDX-License-Identifier: MPL-2.0

//! The navigator's list: what it lists, which rows are elements, and how the
//! pointer, the wheel, the scrollbar and the keyboard move through it. Driven
//! through the window, as the shell's own tests are.

use super::super::tests::{
    SIZE, SMALLEST, assert_sound, centre, draw, drawn, english, entry_key, frame, group_key, named,
    press, record, tab, type_text,
};
use super::*;

/// Which of the three regions a frame holds.
fn regions(lab: &Lab) -> (bool, bool, bool) {
    let frame = frame(lab);
    (
        frame.rect_of("Navigator").is_some(),
        frame.rect_of("Preview").is_some(),
        frame.rect_of("Properties").is_some(),
    )
}

const NONE: ModifiersState = ModifiersState::empty();

/// The keys of every navigator row the frame holds, top to bottom.
fn rows_in_frame(lab: &Lab) -> Vec<Key> {
    frame(lab)
        .nodes
        .iter()
        .filter(|node| matches!(node.name, "Navigator entry" | "Navigator group"))
        .filter_map(|node| node.key)
        .collect()
}

fn list_viewport(lab: &Lab) -> Rect {
    frame(lab).node(LIST).unwrap().viewport.unwrap()
}

/// Move the keyboard to the navigator's stop.
fn tab_to_list(lab: &mut Lab, engine: &mut TextEngine) {
    while !lab.focus.is_some_and(|key| lab.is_navigator_key(key)) {
        assert!(tab(lab, engine));
    }
}

fn key(lab: &mut Lab, engine: &mut TextEngine, key: NamedKey) -> bool {
    named(lab, engine, key, NONE)
}

/// The list is the gallery: the three groups in their order, each followed
/// by its records, and not one record that exists only on a phone.
#[test]
fn the_list_is_the_gallery_in_its_three_groups() {
    let navigator = Navigator::new(record(registry::BUTTON));
    let rows = navigator.rows();
    let groups: Vec<Family> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Group(family) => Some(*family),
            Row::Entry(_) => None,
        })
        .collect();
    assert_eq!(groups, Family::ALL);
    assert_eq!(rows[0], Row::Group(Family::Foundations));
    let listed: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Entry(entry) => Some(entry.catalog_id),
            Row::Group(_) => None,
        })
        .collect();
    // Every desktop record once, under its own group, in the gallery's
    // order within it.
    assert_eq!(listed.len(), registry::desktop().count());
    for family in Family::ALL {
        let expected: Vec<&str> = registry::desktop()
            .filter(|entry| entry.family == family)
            .map(|entry| entry.catalog_id)
            .collect();
        assert_eq!(navigator.count(family), expected.len(), "{family:?}");
        let at = rows
            .iter()
            .position(|row| *row == Row::Group(family))
            .unwrap();
        let under: Vec<&str> = rows[at + 1..]
            .iter()
            .map_while(|row| match row {
                Row::Entry(entry) => Some(entry.catalog_id),
                Row::Group(_) => None,
            })
            .collect();
        assert_eq!(under, expected, "{family:?}");
    }
    // What the registry holds and the gallery does not is not a row.
    let hidden: Vec<&str> = registry::catalog()
        .filter(|entry| !entry.on_desktop())
        .map(|entry| entry.catalog_id)
        .collect();
    assert!(
        !hidden.is_empty(),
        "the registry still holds the phones' records"
    );
    for id in hidden {
        assert!(!listed.contains(&id), "{id} is listed");
        assert_eq!(navigator.row_of(Key::surface("ui-lab.navigator", id)), None);
    }
}

/// A screenful of rows is laid out however many the catalog has, and the two
/// spacers stand for the rest, so the list is exactly as tall as all of
/// them.
#[test]
fn only_the_rows_in_view_are_elements() {
    english(|| {
        for size in [SIZE, SMALLEST] {
            let (lab, _) = drawn(size);
            let all = lab.nav.rows().len();
            assert!(all > 250, "the gallery has {all} rows");
            let viewport = list_viewport(&lab);
            let held = rows_in_frame(&lab).len();
            let fit = (viewport.h / NAVIGATOR_ROW.value).ceil() as usize;
            assert!(held >= fit.min(all), "{size:?}: {held} rows for {fit}");
            assert!(
                held <= (size.1 / NAVIGATOR_ROW.value).ceil() as usize + 1,
                "{size:?}: {held} rows laid out"
            );
            let (content, _) = frame(&lab).node(LIST).unwrap().scroll.unwrap();
            assert_eq!(content, all as f32 * NAVIGATOR_ROW.value);
            // Each row in the frame is where its place in the list puts it.
            let offset = lab.nav.scroll;
            for key in rows_in_frame(&lab) {
                let row = lab.nav.row_of(key).unwrap();
                let at = lab.nav.rows().iter().position(|r| *r == row).unwrap();
                let rect = frame(&lab).node(key).unwrap().rect;
                assert_eq!(rect.h, NAVIGATOR_ROW.value);
                assert_eq!(
                    rect.y,
                    viewport.y + at as f32 * NAVIGATOR_ROW.value - offset,
                    "{size:?}: row {at}"
                );
            }
            assert_sound(&lab, "the navigator");
        }
    });
}

/// Scrolled a screen at a time from the top, every row of the list is an
/// element of some frame, the last one at the very end.
#[test]
fn scrolling_reaches_every_row() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let all: Vec<Key> = lab.nav.rows().iter().map(|row| row.key()).collect();
        let mut seen: Vec<Key> = Vec::new();
        assert!(lab.scroll_to(LIST, 0.0));
        draw(&mut lab, &mut engine, SIZE);
        let (max, viewport) = lab.scroll_range(LIST).unwrap();
        assert!(max > 0.0 && viewport > 0.0);
        loop {
            for key in rows_in_frame(&lab) {
                if !seen.contains(&key) {
                    seen.push(key);
                }
            }
            if lab.nav.scroll >= max {
                break;
            }
            let next = lab.nav.scroll + viewport;
            assert!(lab.scroll_to(LIST, next));
            draw(&mut lab, &mut engine, SIZE);
        }
        assert_eq!(seen, all);
        // At the end the last row is whole and at the viewport's bottom.
        let last = frame(&lab).node(*all.last().unwrap()).unwrap().rect;
        let viewport = list_viewport(&lab);
        assert_eq!(last.y + last.h, viewport.y + viewport.h);
        // An offset past either end is held to the list.
        assert!(!lab.scroll_to(LIST, max + 500.0));
        assert!(lab.scroll_to(LIST, -40.0));
        assert_eq!(lab.nav.scroll, 0.0);
    });
}

/// A wheel notch scrolls the list while the pointer is over it, and does
/// nothing over a viewport with nowhere to go or with the pointer away.
#[test]
fn the_wheel_scrolls_the_list_under_the_pointer() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        lab.scroll_to(LIST, 0.0);
        draw(&mut lab, &mut engine, SIZE);
        assert!(!lab.scroll_by(-60.0, NONE), "the pointer is nowhere yet");

        let viewport = list_viewport(&lab);
        lab.pointer_moved(viewport.x + 20.0, viewport.y + 20.0);
        // Down is a negative notch: the content moves up.
        assert!(lab.scroll_by(-60.0, NONE));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.nav.scroll, 60.0);
        assert!(lab.scroll_by(30.0, NONE));
        assert_eq!(lab.nav.scroll, 30.0);
        assert!(lab.scroll_by(500.0, NONE));
        assert_eq!(lab.nav.scroll, 0.0);
        assert!(!lab.scroll_by(500.0, NONE), "already at the top");

        // Over the caption there is nothing to scroll.
        lab.pointer_moved(200.0, 10.0);
        assert!(!lab.scroll_by(-60.0, NONE));
        assert!(lab.pointer_left() || lab.pointer.is_none());
        assert!(!lab.scroll_by(-60.0, NONE));
    });
}

/// The shared scrollbar: a press in its gutter takes hold of the thumb, a
/// drag moves the list, and the row under the gutter is not pressed.
#[test]
fn the_scrollbar_drags_the_list() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        lab.scroll_to(LIST, 0.0);
        draw(&mut lab, &mut engine, SIZE);
        let shown = lab.entry.catalog_id;
        let bar = frame(&lab).scrollbar(LIST).expect("the list overflows");
        let thumb = bar.thumb();
        let (x, y) = thumb.centre();
        assert_eq!(lab.hit_at(x, y), Some(LIST), "the gutter is the list's");
        assert!(lab.pointer_moved(x, y));
        assert_eq!(lab.hot, Some(LIST));

        assert!(lab.click(&mut engine, x, y, false));
        assert_eq!(lab.entry.catalog_id, shown, "no row was pressed");
        assert!(lab.scroll_drag.is_some());
        assert!(lab.drag(&mut engine, x, y + 80.0));
        draw(&mut lab, &mut engine, SIZE);
        let dragged = lab.nav.scroll;
        assert!(dragged > 0.0);
        assert!(frame(&lab).scrollbar(LIST).unwrap().thumb().y > thumb.y);
        // To the bottom of the track is the end of the list.
        assert!(lab.drag(&mut engine, x, SIZE.1 + 200.0));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.nav.scroll, lab.scroll_range(LIST).unwrap().0);

        lab.mouse_up();
        assert!(lab.scroll_drag.is_none());
        // Released: the pointer moving is a hover again, not a scroll.
        let before = lab.nav.scroll;
        lab.drag(&mut engine, x, y);
        assert_eq!(lab.nav.scroll, before);
    });
}

/// Pressing a record's row shows it; pressing a group's heading folds it and
/// pressing again unfolds it. Neither moves the list.
#[test]
fn a_press_shows_a_record_and_folds_a_group() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        lab.scroll_to(LIST, 0.0);
        draw(&mut lab, &mut engine, SIZE);
        let rows = rows_in_frame(&lab);
        assert_eq!(rows[0], group_key(Family::Foundations));
        let Some(Row::Entry(first)) = lab.nav.row_of(rows[1]) else {
            panic!("a record follows the first heading");
        };
        assert!(press(&mut lab, &mut engine, rows[1]));
        assert_eq!(lab.entry.catalog_id, first.catalog_id);
        assert_eq!(lab.focus, Some(rows[1]));
        assert_eq!(frame(&lab).node(rows[1]).unwrap().selected, Some(true));
        // Pressing the row already on show changes nothing.
        assert!(!press(&mut lab, &mut engine, rows[1]));

        let all = lab.nav.rows().len();
        let count = lab.nav.count(Family::Foundations);
        assert!(press(&mut lab, &mut engine, group_key(Family::Foundations)));
        assert!(lab.nav.is_folded(Family::Foundations));
        assert_eq!(lab.nav.rows().len(), all - count);
        assert_eq!(lab.nav.count(Family::Foundations), count, "still counted");
        assert_eq!(lab.focus, Some(group_key(Family::Foundations)));
        // The record on show stays on show with its row folded away, and
        // the next heading has moved up under this one.
        assert_eq!(lab.entry.catalog_id, first.catalog_id);
        assert_eq!(
            rows_in_frame(&lab)[..2],
            [group_key(Family::Foundations), group_key(Family::Controls)]
        );
        assert_sound(&lab, "a folded group");

        assert!(press(&mut lab, &mut engine, group_key(Family::Foundations)));
        assert!(!lab.nav.is_folded(Family::Foundations));
        assert_eq!(lab.nav.rows().len(), all);
    });
}

/// The list is one Tab stop. The arrows move through its rows and bring the
/// one they land on into view; Home and End go to the ends.
#[test]
fn the_arrows_move_through_the_rows_and_tab_passes_the_list() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        tab_to_list(&mut lab, &mut engine);
        assert_eq!(lab.focus, Some(entry_key(registry::BUTTON)));
        assert_eq!(lab.nav.cursor(), Some(Row::Entry(record(registry::BUTTON))));
        let at = |lab: &Lab| {
            let row = lab.nav.row_of(lab.focus.unwrap()).unwrap();
            lab.nav.rows().iter().position(|r| *r == row).unwrap()
        };
        let start = at(&lab);
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(at(&lab), start + 1);
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowUp));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowUp));
        assert_eq!(at(&lab), start - 1);
        // Moving the keyboard shows nothing new: the record on show stays.
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);
        // The row the keyboard is on is the one drawn with the focus ring.
        let focused: Vec<Key> = frame(&lab)
            .nodes
            .iter()
            .filter(|node| node.state.focused)
            .filter_map(|node| node.key)
            .collect();
        assert_eq!(focused, [lab.focus.unwrap()]);

        assert!(key(&mut lab, &mut engine, NamedKey::End));
        assert_eq!(at(&lab), lab.nav.rows().len() - 1);
        let viewport = list_viewport(&lab);
        let row = frame(&lab).node(lab.focus.unwrap()).unwrap().rect;
        assert!(row.y >= viewport.y && row.y + row.h <= viewport.y + viewport.h);
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowDown), "the end");
        assert!(key(&mut lab, &mut engine, NamedKey::PageUp));
        assert!(at(&lab) < lab.nav.rows().len() - 1);
        assert!(key(&mut lab, &mut engine, NamedKey::Home));
        assert_eq!(lab.focus, Some(group_key(Family::Foundations)));
        assert_eq!(lab.nav.scroll, 0.0);
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowUp), "the start");
        assert!(key(&mut lab, &mut engine, NamedKey::PageDown));
        let row = frame(&lab).node(lab.focus.unwrap()).unwrap().rect;
        assert!(row.y >= viewport.y && row.y + row.h <= viewport.y + viewport.h);

        // One Tab leaves the list however many rows it has, and Shift+Tab
        // comes back to the row the keyboard was on.
        let left = lab.focus;
        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(RESET), "the centre's toolbar is next");
        assert!(named(
            &mut lab,
            &mut engine,
            NamedKey::Tab,
            ModifiersState::SHIFT
        ));
        assert_eq!(lab.focus, left);
        assert!(key(&mut lab, &mut engine, NamedKey::Escape));
        assert_eq!(lab.focus, None);
    });
}

/// Enter or Space shows the record the keyboard is on; Left and Right fold,
/// unfold and step in and out of a group; a folded row hands the keyboard to
/// its heading rather than to whatever took its place.
#[test]
fn the_keyboard_shows_a_record_and_folds_a_group() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        tab_to_list(&mut lab, &mut engine);
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        let Some(Row::Entry(next)) = lab.nav.row_of(lab.focus.unwrap()) else {
            panic!("a record follows the Button");
        };
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert_eq!(lab.entry.catalog_id, next.catalog_id);
        assert!(
            !key(&mut lab, &mut engine, NamedKey::Enter),
            "already shown"
        );
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowUp));
        assert!(key(&mut lab, &mut engine, NamedKey::Space));
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);

        // Left on a record goes to its group; Left there folds it.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        assert_eq!(lab.focus, Some(group_key(Family::Controls)));
        assert!(!lab.nav.is_folded(Family::Controls));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        assert!(lab.nav.is_folded(Family::Controls));
        assert_eq!(lab.focus, Some(group_key(Family::Controls)));
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        // Down from a folded group is the next group, not a hidden record.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(lab.focus, Some(group_key(Family::Compositions)));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowUp));
        // Right unfolds, then steps onto the first record.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        assert!(!lab.nav.is_folded(Family::Controls));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        assert!(matches!(
            lab.nav.row_of(lab.focus.unwrap()),
            Some(Row::Entry(entry)) if entry.family == Family::Controls
        ));
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowRight));
        // Enter on a heading folds it too.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert!(lab.nav.is_folded(Family::Controls));

        // The keyboard on a record whose group is folded by the pointer
        // moves to the heading: never to the row that took its place.
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        let on = lab.focus.unwrap();
        assert!(matches!(lab.nav.row_of(on), Some(Row::Entry(_))));
        assert!(lab.fold(&mut engine, Family::Controls, true));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.focus, Some(group_key(Family::Controls)));
        assert_eq!(lab.nav.row_of(on), None);
        assert_sound(&lab, "after folding");
    });
}

/// With the record on show folded away, or scrolled out of view, the list
/// still has a stop for Tab to land on.
#[test]
fn the_list_always_has_a_stop() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let stop = |lab: &Lab| -> Vec<Key> {
            lab.laid
                .as_ref()
                .unwrap()
                .stops()
                .into_iter()
                .filter(|key| lab.is_navigator_key(*key))
                .collect()
        };
        assert_eq!(stop(&lab), [entry_key(registry::BUTTON)]);
        lab.scroll_to(LIST, 0.0);
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(stop(&lab), [group_key(Family::Foundations)]);
        for family in Family::ALL {
            lab.fold(&mut engine, family, true);
        }
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(rows_in_frame(&lab).len(), 3);
        assert_eq!(stop(&lab).len(), 1);
        assert!(frame(&lab).scrollbar(LIST).is_none(), "three rows fit");
        tab_to_list(&mut lab, &mut engine);
        assert!(lab.nav.row_of(lab.focus.unwrap()).is_some());
    });
}

/// A window made shorter while the list is at its end keeps the list inside
/// what it can scroll, and the rows it shows are the ones at that offset.
#[test]
fn a_resize_keeps_the_list_within_its_rows() {
    english(|| {
        let (mut lab, mut engine) = drawn(SMALLEST);
        let max = lab.scroll_range(LIST).unwrap().0;
        lab.scroll_to(LIST, max);
        draw(&mut lab, &mut engine, SMALLEST);
        let last = lab.nav.rows().last().unwrap().key();
        assert!(frame(&lab).contains(last));

        lab.resized(PhysicalSize::new(1120, 1400), 1.0);
        draw(&mut lab, &mut engine, (1120.0, 1400.0));
        let taller = lab.scroll_range(LIST).unwrap().0;
        assert!(taller < max);
        assert_eq!(lab.nav.scroll, taller, "held to the taller window's end");
        let rect = frame(&lab)
            .node(last)
            .expect("the last row is still shown")
            .rect;
        let viewport = list_viewport(&lab);
        assert_eq!(rect.y + rect.h, viewport.y + viewport.h);
        let (x, y) = centre(&lab, last);
        assert_eq!(lab.hit_at(x, y), Some(last));
    });
}

// ---------------------------------------------------------------- search

/// Put the keyboard in the search field with the pointer.
fn into_search(lab: &mut Lab, engine: &mut TextEngine) {
    assert!(press(lab, engine, SEARCH));
    assert_eq!(lab.focus, Some(SEARCH));
}

fn listed(lab: &Lab) -> Vec<&'static str> {
    lab.nav
        .rows()
        .iter()
        .filter_map(|row| match row {
            Row::Entry(entry) => Some(entry.catalog_id),
            Row::Group(_) => None,
        })
        .collect()
}

/// Typing in the search field narrows the list to what it finds, with each
/// group's heading counting its own. The record on show is not changed by
/// searching, and emptying the field brings the whole gallery back.
#[test]
fn typing_in_the_search_field_narrows_the_list() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let everything = listed(&lab);
        into_search(&mut lab, &mut engine);
        type_text(&mut lab, &mut engine, "button");
        assert_eq!(lab.query(), "button");
        let found = listed(&lab);
        assert!(found.contains(&registry::BUTTON));
        assert!(found.len() > 1 && found.len() < everything.len());
        let words = registry::search_words("button");
        for id in &found {
            assert!(record(id).matches(&words), "{id} was not asked for");
        }
        for id in &everything {
            assert_eq!(found.contains(id), record(id).matches(&words), "{id}");
        }
        // The groups that found something keep their heading and count it.
        let mut counted = 0;
        for family in Family::ALL {
            let count = found
                .iter()
                .filter(|id| record(id).family == family)
                .count();
            assert_eq!(lab.nav.count(family), count, "{family:?}");
            assert_eq!(
                lab.nav.rows().contains(&Row::Group(family)),
                count > 0,
                "{family:?}"
            );
            counted += count;
        }
        assert_eq!(counted, found.len());
        // From the top, and the record on show is still the one on show.
        assert_eq!(lab.nav.scroll, 0.0);
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);
        assert_eq!(
            frame(&lab)
                .node(entry_key(registry::BUTTON))
                .unwrap()
                .selected,
            Some(true)
        );
        assert_sound(&lab, "a narrowed list");

        // More words narrow further: every word has to be found.
        type_text(&mut lab, &mut engine, " split");
        assert_eq!(listed(&lab), ["C-SPLIT-BUTTON"]);
        // Case and the spaces round the words do not matter.
        assert!(lab.set_field(&mut engine, SEARCH, "  SPLIT   Button "));
        assert_eq!(listed(&lab), ["C-SPLIT-BUTTON"]);
        assert!(
            !lab.set_field(&mut engine, SEARCH, "split button"),
            "the same search"
        );

        assert!(lab.set_field(&mut engine, SEARCH, ""));
        assert_eq!(listed(&lab), everything);
    });
}

/// A record is found by what it is called in the display language, by its
/// public name, by its catalog ID and by a symbol it records.
#[test]
fn a_record_is_found_by_its_names_its_id_and_its_symbols() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        for (query, expected) in [
            ("KoruButton", registry::BUTTON),
            ("c-button", registry::BUTTON),
            ("draw_google_button", registry::BUTTON),
            ("koru_ui::text_input::InputStyle", "C-TEXT-INPUT"),
            ("T-SPACE", "T-SPACE"),
        ] {
            lab.set_field(&mut engine, SEARCH, query);
            assert!(listed(&lab).contains(&expected), "`{query}`");
        }
        // A purpose is not searched: a sentence would match half the list.
        let purpose = text(record(registry::BUTTON).purpose);
        lab.set_field(&mut engine, SEARCH, purpose);
        assert!(!listed(&lab).contains(&registry::BUTTON));
    });
    // In each language, a record's own name finds it.
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let (mut lab, mut engine) = drawn(SIZE);
            for id in [registry::BUTTON, "T-SPACE", "C-TEXT-INPUT"] {
                let name = text(record(id).name);
                lab.set_field(&mut engine, SEARCH, name);
                assert!(listed(&lab).contains(&id), "{lang:?}: `{name}`");
            }
        });
    }
}

/// What exists only on a phone cannot be searched for: not by its name, not
/// by its catalog ID, not by its symbols.
#[test]
fn a_phone_record_is_never_found() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let hidden: Vec<&'static Entry> = registry::catalog()
            .filter(|entry| !entry.on_desktop())
            .collect();
        assert!(hidden.len() > 20);
        for entry in hidden {
            let symbol = entry.symbols().next().unwrap().symbol;
            for query in [entry.catalog_id, text(entry.name), symbol] {
                lab.set_field(&mut engine, SEARCH, query);
                assert!(
                    !listed(&lab).contains(&entry.catalog_id),
                    "`{query}` found {}",
                    entry.catalog_id
                );
            }
        }
    });
}

/// A search that finds nothing keeps what was typed, says so, and offers a
/// button that empties it. The list comes back with the record on show in
/// view, and the keyboard is in the field again.
#[test]
fn a_search_that_finds_nothing_keeps_the_query_and_offers_reset() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let everything = listed(&lab);
        into_search(&mut lab, &mut engine);
        type_text(&mut lab, &mut engine, "no such thing");
        assert!(lab.nav.rows().is_empty());
        assert_eq!(lab.query(), "no such thing", "the query is kept");
        let frame = frame(&lab);
        assert!(!frame.contains(LIST));
        assert!(frame.contains(NOTHING_FOUND));
        assert!(frame.contains(RESET_SEARCH));
        assert_sound(&lab, "nothing found");
        // The page is still the Button's, and still works.
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);
        assert!(frame.contains(SUBJECT));
        // Tab goes from the field to the button under it.
        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(RESET_SEARCH));
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert_eq!(lab.query(), "");
        assert_eq!(listed(&lab), everything);
        assert_eq!(lab.focus, Some(SEARCH), "back in the field it cleared");
        let frame = super::super::tests::frame(&lab);
        assert!(!frame.contains(RESET_SEARCH) && !frame.contains(NOTHING_FOUND));
        let row = frame
            .node(entry_key(registry::BUTTON))
            .expect("the record on show is in view again");
        assert_eq!(row.selected, Some(true));

        // The pointer does the same.
        type_text(&mut lab, &mut engine, "zzzz");
        assert!(press(&mut lab, &mut engine, RESET_SEARCH));
        assert_eq!(lab.query(), "");
        assert_eq!(listed(&lab), everything);

        // A very long query with nowhere to break still fits the column.
        lab.set_field(&mut engine, SEARCH, &"q".repeat(400));
        draw(&mut lab, &mut engine, SMALLEST);
        assert!(lab.nav.rows().is_empty());
        assert_sound(&lab, "a very long query");
    });
}

/// The toolbar's Reset is the story's: it leaves the search, what it found
/// and the record on show alone. Showing another record leaves them too.
#[test]
fn reset_and_showing_a_record_keep_the_search() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        lab.set_field(&mut engine, SEARCH, "button");
        draw(&mut lab, &mut engine, SIZE);
        let found = listed(&lab);
        press(&mut lab, &mut engine, SUBJECT);
        assert!(press(&mut lab, &mut engine, RESET));
        assert_eq!(lab.query(), "button");
        assert_eq!(listed(&lab), found);
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);

        assert!(press(&mut lab, &mut engine, entry_key("C-SPLIT-BUTTON")));
        assert_eq!(lab.entry.catalog_id, "C-SPLIT-BUTTON");
        assert_eq!(lab.query(), "button");
        assert_eq!(listed(&lab), found);
        // The record on show need not be one the search finds.
        lab.set_field(&mut engine, SEARCH, "T-SPACE");
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.entry.catalog_id, "C-SPLIT-BUTTON");
        assert_eq!(listed(&lab), ["T-SPACE"]);
        assert_sound(&lab, "the record on show is not in the list");
    });
}

/// From the field: Down goes to the first row found, Enter shows the first
/// record found, and Escape empties the search before it gives the keyboard
/// up.
#[test]
fn the_search_field_is_driven_from_the_keyboard() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        // Tab reaches the field before the list.
        while lab.focus != Some(SEARCH) {
            assert!(tab(&mut lab, &mut engine));
        }
        type_text(&mut lab, &mut engine, "split button");
        assert_eq!(lab.entry.catalog_id, registry::BUTTON);
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert_eq!(lab.entry.catalog_id, "C-SPLIT-BUTTON");
        assert_eq!(lab.focus, Some(SEARCH), "still typing");
        assert!(
            !key(&mut lab, &mut engine, NamedKey::Enter),
            "already shown"
        );

        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(lab.focus, Some(group_key(Family::Controls)));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(lab.focus, Some(entry_key("C-SPLIT-BUTTON")));
        // Shift+Tab from the list is the field again, its text selected so
        // the next word replaces it.
        assert!(named(
            &mut lab,
            &mut engine,
            NamedKey::Tab,
            ModifiersState::SHIFT
        ));
        assert_eq!(lab.focus, Some(SEARCH));
        type_text(&mut lab, &mut engine, "toggle");
        assert_eq!(lab.query(), "toggle");

        assert!(key(&mut lab, &mut engine, NamedKey::Escape));
        assert_eq!(lab.query(), "", "Escape empties the search first");
        assert_eq!(lab.focus, Some(SEARCH));
        assert!(key(&mut lab, &mut engine, NamedKey::Escape));
        assert_eq!(lab.focus, None);
        // With nothing found there is no row for Down or Enter to go to.
        into_search(&mut lab, &mut engine);
        type_text(&mut lab, &mut engine, "zzzz");
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert!(!key(&mut lab, &mut engine, NamedKey::Enter));
        assert_eq!(lab.entry.catalog_id, "C-SPLIT-BUTTON");
    });
}

/// The search field is a real field: an I-beam over it, a visible mark while
/// it has the keyboard, composed text that only searches once committed, and
/// a caret an input method can open its candidates at.
#[test]
fn the_search_field_takes_composed_text_and_reports_its_caret() {
    koru_i18n::with_current(koru_i18n::Lang::Ja, || {
        let (mut lab, mut engine) = drawn(SIZE);
        let (x, y) = centre(&lab, SEARCH);
        assert_eq!(lab.cursor_icon(x, y), CursorIcon::Text);
        assert_eq!(lab.ime_cursor_area(), None, "nothing is typed in yet");
        into_search(&mut lab, &mut engine);
        // The box round the field wears the focus ring.
        let ring = frame(&lab).named("Search").next().unwrap();
        assert!(ring.state.focused && ring.ring.is_some());
        let field = lab.rect(SEARCH).unwrap();
        assert!(field.x > ring.rect.x && field.y > ring.rect.y);
        let caret = lab.ime_cursor_area().expect("the field is typed in");
        assert!(caret.x >= field.x && caret.x <= field.x + field.w);
        assert!(caret.y >= field.y && caret.y + caret.h <= field.y + field.h);

        let everything = listed(&lab);
        lab.ime(&mut engine, ImeEvent::Enabled);
        assert!(lab.ime(
            &mut engine,
            ImeEvent::Preedit {
                text: "ぼたん",
                cursor: Some((9, 9)),
            },
        ));
        assert_eq!(listed(&lab), everything, "a composition is not a search");
        let name = text(record(registry::BUTTON).name);
        assert!(lab.ime(&mut engine, ImeEvent::Commit { text: name }));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.query(), name);
        assert!(listed(&lab).contains(&registry::BUTTON));
        assert!(listed(&lab).len() < everything.len());
        assert!(
            lab.ime_cursor_area().unwrap().x > caret.x,
            "the caret moved"
        );
        assert_sound(&lab, "a composed search");

        // Another window has the keyboard: no composition is ours to place.
        lab.focus_changed(false);
        assert_eq!(lab.ime_cursor_area(), None);
    });
}

// -------------------------------------------------- the navigator's width

const WIDE: (f32, f32) = (1600.0, 800.0);
const LEAST: f32 = NAVIGATOR_WIDTH.value;
const MOST: f32 = NAVIGATOR_MAX_WIDTH.value;

fn navigator_width(lab: &Lab) -> f32 {
    frame(lab).rect_of("Navigator").unwrap().w
}

/// Take hold of the navigator's edge, drag it to `x`, and draw.
fn drag_edge_to(lab: &mut Lab, engine: &mut TextEngine, x: f32, size: (f32, f32)) -> bool {
    let grip = lab.navigator_grip().expect("the navigator shows");
    let (gx, gy) = grip.centre();
    assert!(
        lab.click(engine, gx, gy, false),
        "the edge is taken hold of"
    );
    let changed = lab.drag(engine, x, gy);
    draw(lab, engine, size);
    lab.mouse_up();
    changed
}

/// The band that resizes the navigator is the app shell's own: its width,
/// straddling the line between the navigator and the centre, the whole
/// height of the column. The pointer says so there and nowhere else, and
/// nothing under the band lights.
#[test]
fn the_navigators_edge_is_the_app_shells_grip() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let navigator = frame(&lab).rect_of("Navigator").unwrap();
        let edge = frame(&lab).rect_of("Navigator edge").unwrap();
        let grip = lab.navigator_grip().unwrap();
        assert_eq!(grip, koru_app_shell::panel::grip(navigator));
        assert_eq!(grip.w, koru_app_shell::panel::GRIP);
        assert_eq!(edge.x, navigator.x + navigator.w, "the line it lies over");
        assert_eq!(grip.x + grip.w * 0.5, edge.x);
        assert_eq!((grip.y, grip.h), (navigator.y, navigator.h));
        assert_eq!(LEAST * 2.0, MOST, "at most twice what it opens at");

        let y = grip.y + grip.h * 0.5;
        for x in [grip.x + 0.5, edge.x, grip.x + grip.w - 0.5] {
            assert_eq!(lab.cursor_icon(x, y), CursorIcon::ColResize, "{x}");
        }
        for x in [grip.x - 2.0, grip.x + grip.w + 2.0] {
            assert_ne!(lab.cursor_icon(x, y), CursorIcon::ColResize, "{x}");
        }
        // A row runs under the inner half of the band; it does not light.
        lab.pointer_moved(grip.x + 1.0, y);
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.hot, None);
        // A press there is the edge's: no row is shown, nothing is focused.
        let shown = lab.entry.catalog_id;
        assert!(lab.click(&mut engine, grip.x + 1.0, y, false));
        assert!(lab.edge_drag.is_some());
        assert_eq!(lab.entry.catalog_id, shown);
        assert_eq!(lab.focus, None);
        lab.mouse_up();
        assert!(lab.edge_drag.is_none());

        // Hidden, there is no edge to take hold of.
        lab.set_column(Side::Navigator, false);
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.navigator_grip(), None);
        assert_ne!(lab.cursor_icon(edge.x, y), CursorIcon::ColResize);
        // Under an open list the press is the list's.
        lab.set_column(Side::Navigator, true);
        draw(&mut lab, &mut engine, SIZE);
        assert!(press(&mut lab, &mut engine, Setting::Theme.key()));
        assert_ne!(lab.cursor_icon(edge.x, y), CursorIcon::ColResize);
        assert!(lab.click(&mut engine, edge.x, y, false));
        assert!(lab.menu.is_none() && lab.edge_drag.is_none());
    });
}

/// Dragging the edge widens the navigator up to twice its width and narrows
/// it back to the width it opens at, and stops at either. The rows are laid
/// out to the new width and the centre starts where the navigator ends.
#[test]
fn dragging_the_edge_resizes_the_navigator_between_its_limits() {
    english(|| {
        let (mut lab, mut engine) = drawn(WIDE);
        assert_eq!(navigator_width(&lab), LEAST);
        let row = |lab: &Lab| frame(lab).node(entry_key(registry::BUTTON)).unwrap().rect;
        let narrow_row = row(&lab);
        let revision = lab.semantics_revision();

        assert!(drag_edge_to(&mut lab, &mut engine, 340.0, WIDE));
        assert_eq!(navigator_width(&lab), 340.0);
        assert_eq!(row(&lab).w, narrow_row.w + 100.0, "the rows follow");
        assert_ne!(lab.semantics_revision(), revision);
        let frame_now = frame(&lab);
        let toolbar = frame_now.rect_of("Toolbar").unwrap();
        let preview = frame_now.rect_of("Preview").unwrap();
        assert_eq!(toolbar.x, 340.0 + theme::BORDER_W as f32);
        assert_eq!((toolbar.x, toolbar.w), (preview.x, preview.w));
        // Only the rows in view are elements, as before.
        let held = rows_in_frame(&lab).len();
        assert!(held <= (WIDE.1 / NAVIGATOR_ROW.value).ceil() as usize + 1);
        assert_sound(&lab, "a wider navigator");

        // Past the widest it stops, and the pointer goes on without it.
        assert!(drag_edge_to(&mut lab, &mut engine, 1200.0, WIDE));
        assert_eq!(navigator_width(&lab), MOST);
        assert!(!drag_edge_to(&mut lab, &mut engine, 1300.0, WIDE));
        assert_eq!(lab.navigator_width, MOST);
        assert_sound(&lab, "the widest navigator");
        // And past the narrowest.
        assert!(drag_edge_to(&mut lab, &mut engine, 20.0, WIDE));
        assert_eq!(navigator_width(&lab), LEAST);
        assert!(!drag_edge_to(&mut lab, &mut engine, -50.0, WIDE));
        assert_eq!(row(&lab), narrow_row);

        // The edge is held where it was taken: a press off its middle does
        // not jump the width.
        let grip = lab.navigator_grip().unwrap();
        let y = grip.y + 40.0;
        assert!(lab.click(&mut engine, grip.x + 1.0, y, false));
        assert!(!lab.drag(&mut engine, grip.x + 1.0, y), "not moved yet");
        assert!(lab.drag(&mut engine, grip.x + 31.0, y));
        assert_eq!(lab.navigator_width, LEAST + 30.0);
        lab.mouse_up();
        // Released: the pointer moving is a hover again.
        lab.drag(&mut engine, 500.0, y);
        assert_eq!(lab.navigator_width, LEAST + 30.0);
    });
}

/// The preview keeps its least width: the edge stops where widening the
/// navigator would take it, and does not move at all when the centre is
/// already there.
#[test]
fn the_edge_stops_where_the_preview_would_lose_its_least() {
    english(|| {
        let (mut lab, mut engine) = drawn(SIZE);
        let preview = |lab: &Lab| frame(lab).rect_of("Preview").unwrap().w;
        let spare = preview(&lab) - PREVIEW_MIN_WIDTH.value;
        assert!(spare > 0.0 && LEAST + spare < MOST);
        assert!(drag_edge_to(&mut lab, &mut engine, 900.0, SIZE));
        assert_eq!(navigator_width(&lab), LEAST + spare);
        assert_eq!(preview(&lab), PREVIEW_MIN_WIDTH.value);
        assert_eq!(regions(&lab), (true, true, true), "no column closed for it");
        // What was dragged to is what shows: not a wish for more.
        assert_eq!(lab.navigator_width, LEAST + spare);
        assert_sound(&lab, "the preview at its least");

        // A window that leaves the centre exactly its least: nowhere to go.
        let tight = (
            LEAST + PROPERTIES_WIDTH.value + 2.0 * theme::BORDER_W as f32 + PREVIEW_MIN_WIDTH.value,
            700.0,
        );
        let (mut lab, mut engine) = drawn(tight);
        assert_eq!(preview(&lab), PREVIEW_MIN_WIDTH.value);
        assert!(!drag_edge_to(&mut lab, &mut engine, 600.0, tight));
        assert_eq!(navigator_width(&lab), LEAST);
        assert_eq!(lab.navigator_width, LEAST);
    });
}

/// The dragged width is the Lab's for as long as it is open: hiding and
/// showing the navigator, a search and a change of language all keep it. A
/// new Lab opens at the default.
#[test]
fn the_dragged_width_lasts_while_the_lab_is_open() {
    english(|| {
        let (mut lab, mut engine) = drawn(WIDE);
        drag_edge_to(&mut lab, &mut engine, 400.0, WIDE);
        assert_eq!(navigator_width(&lab), 400.0);

        assert!(lab.set_column(Side::Navigator, false));
        draw(&mut lab, &mut engine, WIDE);
        assert!(frame(&lab).rect_of("Navigator").is_none());
        assert!(lab.set_column(Side::Navigator, true));
        draw(&mut lab, &mut engine, WIDE);
        assert_eq!(navigator_width(&lab), 400.0);

        lab.set_field(&mut engine, SEARCH, "button");
        assert!(lab.set_language(&mut engine, koru_i18n::Lang::De));
        lab.show(&mut engine, record("T-SPACE"));
        draw(&mut lab, &mut engine, WIDE);
        assert_eq!(navigator_width(&lab), 400.0);
        lab.reset(&mut engine);
        assert_eq!(lab.navigator_width, 400.0);

        let (fresh, _) = drawn(WIDE);
        assert_eq!(navigator_width(&fresh), LEAST);
    });
}

/// As the window narrows, a widened navigator gives its extra width back
/// before any column closes, and has it again when the room returns: the
/// width that was dragged to is never overwritten by a squeeze.
#[test]
fn a_narrowing_window_squeezes_the_navigator_before_it_closes_a_column() {
    english(|| {
        let (mut lab, mut engine) = drawn(WIDE);
        drag_edge_to(&mut lab, &mut engine, 400.0, WIDE);
        let line = theme::BORDER_W as f32;
        let fixed = PROPERTIES_WIDTH.value + 2.0 * line + PREVIEW_MIN_WIDTH.value;
        let resize = |lab: &mut Lab, engine: &mut TextEngine, width: f32| {
            lab.resized(PhysicalSize::new(width as u32, 700), 1.0);
            draw(lab, engine, (width, 700.0));
        };
        // Room for all of it.
        resize(&mut lab, &mut engine, 400.0 + fixed + 50.0);
        assert_eq!(navigator_width(&lab), 400.0);
        assert_eq!(regions(&lab), (true, true, true));
        // Room for some of it: the preview is at its least and the navigator
        // has what is left.
        resize(&mut lab, &mut engine, 330.0 + fixed);
        assert_eq!(navigator_width(&lab), 330.0);
        assert_eq!(regions(&lab), (true, true, true));
        assert_eq!(
            frame(&lab).rect_of("Preview").unwrap().w,
            PREVIEW_MIN_WIDTH.value
        );
        assert_sound(&lab, "a squeezed navigator");
        // Room for none of it: the navigator at its narrowest, both columns
        // still showing.
        resize(&mut lab, &mut engine, LEAST + fixed);
        assert_eq!(navigator_width(&lab), LEAST);
        assert_eq!(regions(&lab), (true, true, true));
        // Narrower still, the existing rule closes the properties; the
        // navigator then has room for the width it was dragged to again.
        resize(&mut lab, &mut engine, LEAST + fixed - 1.0);
        assert_eq!(regions(&lab), (true, true, false));
        assert_eq!(navigator_width(&lab), 400.0);
        // The smallest window: squeezed again, and never under its least.
        resize(&mut lab, &mut engine, SMALLEST.0);
        assert_eq!(regions(&lab), (true, true, false));
        assert_eq!(
            navigator_width(&lab),
            (SMALLEST.0 - line - PREVIEW_MIN_WIDTH.value).min(400.0)
        );
        assert_sound(&lab, "the smallest window");
        // Opening the properties there closes the navigator, as before.
        lab.set_column(Side::Properties, true);
        draw(&mut lab, &mut engine, (SMALLEST.0, 700.0));
        assert_eq!(regions(&lab), (false, true, true));
        // Through all of it the dragged width was kept, and it is back.
        assert_eq!(lab.navigator_width, 400.0);
        resize(&mut lab, &mut engine, WIDE.0);
        assert_eq!(regions(&lab), (true, true, true));
        assert_eq!(navigator_width(&lab), 400.0);
    });
}
