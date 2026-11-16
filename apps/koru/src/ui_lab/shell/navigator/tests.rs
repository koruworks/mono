// SPDX-License-Identifier: MPL-2.0

//! The navigator's list: what it lists, which rows are elements, and how the
//! pointer, the wheel, the scrollbar and the keyboard move through it. Driven
//! through the window, as the shell's own tests are.

use super::super::tests::{
    SIZE, SMALLEST, assert_sound, centre, draw, drawn, english, entry_key, frame, group_key, named,
    press, record, tab, type_text,
};
use super::*;

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
