// SPDX-License-Identifier: MPL-2.0

//! A record's page: what it states for a record with a story and for one
//! without, that it never draws a stand-in, and that a long one scrolls.

use super::super::tests::{
    NO_STORY, SIZE, SMALLEST, assert_sound, draw, drawn, english, frame, named, record, tab,
};
use super::*;

const NONE: ModifiersState = ModifiersState::empty();

/// A Lab that opened on `entry` and has drawn one frame.
fn showing_record(entry: &'static Entry, size: (f32, f32)) -> (Lab, TextEngine) {
    let mut engine = TextEngine::new();
    let mut lab = Lab::new(&mut engine, Placement::in_memory(Some(size)), entry);
    draw(&mut lab, &mut engine, size);
    (lab, engine)
}

/// The first record of the gallery in each status a page can state.
fn one_of_each_status() -> Vec<&'static Entry> {
    let mut found: Vec<&'static Entry> = Vec::new();
    for entry in registry::desktop() {
        let status = std::mem::discriminant(&entry.status());
        if !found
            .iter()
            .any(|seen| std::mem::discriminant(&seen.status()) == status)
        {
            found.push(entry);
        }
    }
    found
}

#[test]
fn an_identifier_wraps_at_its_separators_and_reads_as_written() {
    let path = "koru_ui::button::ActionButton::draw_focused_elided";
    let wrapped = breakable(path);
    assert_eq!(wrapped.replace('\u{200B}', ""), path, "nothing is changed");
    assert_eq!(
        wrapped,
        "koru_ui::\u{200B}button::\u{200B}ActionButton::\u{200B}draw_focused_elided"
    );
    // A name is never broken inside, and nothing is added at the end.
    assert_eq!(breakable("S0_5"), "S0_5");
    assert_eq!(breakable("koru-ui"), "koru-ui");
    assert_eq!(breakable("layout.gap"), "layout.\u{200B}gap");
    assert_eq!(breakable("a::"), "a::");
    assert_eq!(
        breakable("crates/koru-ui/src/button.rs"),
        "crates/\u{200B}koru-ui/\u{200B}src/\u{200B}button.\u{200B}rs"
    );
}

/// The gallery has a record in each of the five states, and a page for each:
/// a story runs, and each of the other four says which it is and why, with
/// nothing drawn in the component's place.
#[test]
fn a_record_without_a_story_says_so_and_draws_no_stand_in() {
    english(|| {
        let records = one_of_each_status();
        assert_eq!(records.len(), 5, "the gallery has every status");
        for entry in records {
            let (lab, _) = showing_record(entry, SIZE);
            let frame = frame(&lab);
            let context = entry.catalog_id;
            assert_sound(&lab, context);
            let runs = matches!(entry.status(), registry::Story::Available(_));
            assert_eq!(lab.story.is_some(), runs, "{context}");
            assert_eq!(frame.contains(SUBJECT), runs, "{context}");
            assert_eq!(frame.contains(STAGE), runs, "{context}");
            assert_eq!(frame.contains(STATUS), !runs, "{context}");
            assert_eq!(frame.contains(REASON), !runs, "{context}");
            assert_eq!(frame.named("Click result").count(), usize::from(runs));
            // Without a story there is no control to operate and nothing to
            // reset; the page is still a page.
            assert_eq!(frame.named("Property").count() > 0, runs, "{context}");
            assert_eq!(frame.node(RESET).unwrap().disabled, !runs, "{context}");
            assert_eq!(
                lab.laid.as_ref().unwrap().stops().contains(&RESET),
                runs,
                "{context}"
            );
            assert!(frame.contains(NAME) && frame.contains(PURPOSE), "{context}");
            assert_eq!(frame.named("Fact").count(), facts(entry).len());
            // No leaf stands where a component would: the only leaves on a
            // page without a story are the window's own controls.
            if !runs {
                for node in &frame.nodes {
                    assert!(
                        !matches!(node.name, "Component under test" | "Preview stage"),
                        "{context} shows a stand-in"
                    );
                }
            }
        }
    });
}

/// What a page states about a record is read from the record: its kind, its
/// source, the three desktop platforms and no phone, A2UI and its status.
#[test]
fn the_facts_are_the_records_own() {
    english(|| {
        let button = record(registry::BUTTON);
        let stated: Vec<(&str, String, Vec<String>)> = facts(button)
            .into_iter()
            .map(|fact| (fact.key.part, fact.name, fact.lines))
            .collect();
        let line = |text: &str| text.to_owned();
        assert_eq!(
            stated,
            [
                ("kind", line("Kind"), vec![line("Shared component")]),
                (
                    "source",
                    line("Source"),
                    vec![line("koru-ui"), line("koru_ui::button")]
                ),
                (
                    "availability",
                    line("Availability"),
                    vec![
                        line("Windows: Runs in this Lab"),
                        line("macOS: Same source builds; not verified"),
                        line("Linux: Same source builds; not verified"),
                    ]
                ),
                (
                    "a2ui",
                    line("A2UI"),
                    vec![
                        line("Component “Button”; the host picks its style"),
                        line("Used by the host only; a declaration cannot name it"),
                        line("Not supported by A2UI"),
                    ]
                ),
                ("story", line("Story"), vec![line("Story available")]),
            ]
        );
        assert_eq!(
            facts(record(NO_STORY)).last().unwrap().lines,
            ["No story yet"]
        );
    });
    // In no language, for no record, does a page name a phone.
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            for entry in registry::desktop() {
                let stated = facts(entry);
                assert_eq!(stated[2].lines.len(), registry::Platform::DESKTOP.len());
                for fact in &stated {
                    let spoken = fact.spoken();
                    assert!(!spoken.contains("{}"), "{lang:?}: {spoken}");
                    assert!(
                        !spoken.contains("Android") && !spoken.contains("iOS"),
                        "{lang:?} {}: {spoken}",
                        entry.catalog_id
                    );
                    assert!(fact.lines.iter().all(|line| !line.trim().is_empty()));
                }
            }
        });
    }
}

/// A path too long for the narrowest page wraps at its separators: the
/// longest symbol in the gallery, in the smallest window with the properties
/// open beside it.
#[test]
fn a_long_symbol_wraps_inside_the_narrowest_page() {
    english(|| {
        let (entry, long) = registry::desktop()
            .flat_map(|entry| entry.symbols().map(move |symbol| (entry, symbol.symbol)))
            .max_by_key(|(_, symbol)| symbol.len())
            .unwrap();
        assert!(long.len() > 60, "{long}");
        let (mut lab, mut engine) = showing_record(entry, SMALLEST);
        assert!(lab.set_column(Side::Properties, true));
        draw(&mut lab, &mut engine, SMALLEST);
        let frame = frame(&lab);
        let block = frame
            .nodes
            .iter()
            .position(|node| node.key == Some(Key::surface(SYMBOL, long)))
            .expect("the record's page lists the symbol");
        // The block's first child is the path itself.
        let path = &frame.nodes[block + 1];
        assert_eq!(path.name, "Symbol path");
        assert!(
            path.lines() > Some(1),
            "{:?} lines for {long}",
            path.lines()
        );
        let page = frame.node(PAGE).unwrap().rect;
        assert!(path.rect.w < page.w);
        assert_sound(&lab, "the narrowest page");
    });
}

/// Every symbol a record names is on its page as the source writes it, with
/// its option sets as their own identifiers, in every language.
#[test]
fn symbols_and_option_sets_are_listed_as_written() {
    let entry = record("C-TEXT-INPUT");
    let symbols: Vec<registry::Symbol> = entry.symbols().collect();
    assert!(symbols.len() >= 3);
    let mut lines: Vec<Vec<String>> = Vec::new();
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let (lab, _) = showing_record(entry, SIZE);
            let frame = frame(&lab);
            assert_eq!(frame.named("Symbol").count(), symbols.len(), "{lang:?}");
            for symbol in &symbols {
                assert!(
                    frame.contains(Key::surface(SYMBOL, symbol.symbol)),
                    "{lang:?}: {}",
                    symbol.symbol
                );
            }
            let options: usize = symbols.iter().map(|symbol| symbol.options.len()).sum();
            assert!(options > 0);
            assert_eq!(frame.named("Option set").count(), options, "{lang:?}");
            assert_sound(&lab, "the text input's page");
            lines.push(
                symbols
                    .iter()
                    .flat_map(|symbol| symbol.options)
                    .map(option_line)
                    .collect(),
            );
            for symbol in &symbols {
                assert!(!symbol_status(symbol).contains("{}"), "{lang:?}");
            }
        });
    }
    // The identifiers are in every language's line, whole and in order.
    for line in lines.iter().flatten() {
        assert!(line.contains("koru_ui::text_input::"), "{line}");
    }
    assert!(
        lines[0]
            .contains(&"koru_ui::text_input::InputStyle: search, inline, form, bare".to_owned())
    );
    assert!(lines.iter().all(|per_language| {
        per_language
            .iter()
            .any(|line| line.ends_with("search, inline, form, bare"))
    }));
}

/// Every desktop record's page lays out with nothing overflowing and no
/// layout complaint, at the default size and at the smallest.
#[test]
fn every_desktop_record_has_a_sound_page() {
    english(|| {
        let mut engine = TextEngine::new();
        let first = registry::desktop().next().unwrap();
        for size in [SIZE, SMALLEST] {
            let mut lab = Lab::new(&mut engine, Placement::in_memory(Some(size)), first);
            for entry in registry::desktop() {
                lab.show(&mut engine, entry);
                draw(&mut lab, &mut engine, size);
                assert_eq!(lab.entry.catalog_id, entry.catalog_id);
                assert_sound(&lab, &format!("{} at {size:?}", entry.catalog_id));
                let frame = frame(&lab);
                assert_eq!(frame.named("Symbol").count(), entry.symbols().count());
                // Everything the page holds is inside the page's width.
                let page = frame.node(PAGE).unwrap().rect;
                for node in frame.named("Symbol path") {
                    assert!(
                        node.rect.x >= page.x && node.rect.x + node.rect.w <= page.x + page.w,
                        "{} at {size:?}: a symbol leaves the page",
                        entry.catalog_id
                    );
                }
            }
        }
    });
}

/// A page longer than the window scrolls: by the wheel, by its scrollbar's
/// range, and from the keyboard once the page has it.
#[test]
fn a_long_page_scrolls_from_the_keyboard() {
    english(|| {
        let longest = registry::desktop()
            .max_by_key(|entry| entry.symbols().count())
            .unwrap();
        assert!(longest.symbols().count() > 20, "{}", longest.catalog_id);
        let (mut lab, mut engine) = showing_record(longest, SMALLEST);
        let (max, viewport) = lab.scroll_range(PAGE).unwrap();
        assert!(max > 0.0, "the page overflows");
        assert!(frame(&lab).scrollbar(PAGE).is_some());
        assert_eq!(lab.page_scroll, 0.0);

        while lab.focus != Some(PAGE) {
            assert!(tab(&mut lab, &mut engine));
        }
        assert!(frame(&lab).node(PAGE).unwrap().state.focused);
        let key = |lab: &mut Lab, engine: &mut TextEngine, key| {
            let changed = lab.key(engine, &KeyboardKey::Named(key), None, NONE);
            draw(lab, engine, SMALLEST);
            changed
        };
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowUp), "at the top");
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert_eq!(lab.page_scroll, LINE_STEP);
        assert!(key(&mut lab, &mut engine, NamedKey::PageDown));
        assert_eq!(lab.page_scroll, (LINE_STEP + viewport - LINE_STEP).min(max));
        assert!(key(&mut lab, &mut engine, NamedKey::End));
        assert_eq!(lab.page_scroll, max);
        assert!(
            !key(&mut lab, &mut engine, NamedKey::ArrowDown),
            "at the end"
        );
        assert!(key(&mut lab, &mut engine, NamedKey::PageUp));
        assert!(key(&mut lab, &mut engine, NamedKey::Home));
        assert_eq!(lab.page_scroll, 0.0);

        // The wheel over the page moves the page and leaves the list alone.
        let list = lab.nav.scroll;
        let page = frame(&lab).node(PAGE).unwrap().rect;
        lab.pointer_moved(page.x + page.w * 0.5, page.y + page.h * 0.5);
        assert!(lab.scroll_by(-120.0, NONE));
        assert_eq!(lab.page_scroll, 120.0);
        assert_eq!(lab.nav.scroll, list);

        // Showing another record starts its page at the top.
        assert!(lab.show(&mut engine, record(NO_STORY)));
        draw(&mut lab, &mut engine, SMALLEST);
        assert_eq!(lab.page_scroll, 0.0);
        assert!(frame(&lab).scrollbar(PAGE).is_none());
        assert!(!named(&mut lab, &mut engine, NamedKey::ArrowDown, NONE));
    });
}

/// The properties column scrolls when the window is too short for it, and a
/// control reached by Tab is brought into view.
#[test]
fn the_properties_column_brings_a_tabbed_control_into_view() {
    english(|| {
        let size = (SMALLEST.0, 340.0);
        let (mut lab, mut engine) = drawn(size);
        // Too narrow for the properties until they are asked for.
        assert!(lab.set_column(Side::Properties, true));
        draw(&mut lab, &mut engine, size);
        assert!(
            frame(&lab).scrollbar(PANEL).is_some(),
            "the column is taller than a 340 high window"
        );
        let style = control_key(Control::Style);
        let viewport = frame(&lab).node(PANEL).unwrap().viewport.unwrap();
        let before = lab.rect(style).unwrap();
        assert!(before.y + before.h > viewport.y + viewport.h, "cut off");
        while lab.focus != Some(style) {
            assert!(lab.key(&mut engine, &KeyboardKey::Named(NamedKey::Tab), None, NONE));
            draw(&mut lab, &mut engine, size);
        }
        assert!(lab.panel_scroll > 0.0);
        let after = lab.rect(style).unwrap();
        assert!(
            after.y >= viewport.y && after.y + after.h <= viewport.y + viewport.h,
            "{after:?} is not inside {viewport:?}"
        );
        assert_sound(&lab, "a scrolled properties column");
    });
}
