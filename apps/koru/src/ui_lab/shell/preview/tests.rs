// SPDX-License-Identifier: MPL-2.0

//! The stories beyond the Button, driven through the window: the text
//! input, the row and the column, and the token tables.

use accesskit::{Action, Role as NativeRole, Toggled};
use koru_draw::theme::Profile;
use koru_workspace_host::accessibility::{SemanticNode, ViewSlot, WindowSlot};

use super::super::tests::{
    SIZE, SMALLEST, assert_sound, centre, draw, english, frame, named, press, record, result, tab,
    type_text,
};
use super::*;

mod parts;

const NONE: ModifiersState = ModifiersState::empty();

/// A Lab that opened on the record `catalog_id` and has drawn one frame.
fn showing(catalog_id: &str, size: (f32, f32)) -> (Lab, TextEngine) {
    let mut engine = TextEngine::new();
    let mut lab = Lab::new(
        &mut engine,
        Placement::in_memory(Some(size)),
        record(catalog_id),
    );
    draw(&mut lab, &mut engine, size);
    (lab, engine)
}

fn key(lab: &mut Lab, engine: &mut TextEngine, key: NamedKey) -> bool {
    named(lab, engine, key, NONE)
}

/// Every node of the published tree, parents before children.
fn published(lab: &Lab) -> Vec<SemanticNode> {
    fn flat(nodes: &[SemanticNode], all: &mut Vec<SemanticNode>) {
        for node in nodes {
            all.push(node.clone());
            flat(&node.children, all);
        }
    }
    let semantics = lab
        .window_semantics(WindowSlot::from_raw(1), ViewSlot::from_raw(1), 1.0)
        .expect("a drawn Lab has a tree");
    let mut all = Vec::new();
    flat(&semantics.children, &mut all);
    all
}

fn node(lab: &Lab, locator: &str) -> SemanticNode {
    published(lab)
        .into_iter()
        .find(|node| format!("{}#{}", node.occurrence.object, node.occurrence.part) == locator)
        .unwrap_or_else(|| panic!("the tree has no `{locator}`"))
}

const STORIED: [&str; 9] = [
    "C-BUTTON",
    "C-TEXT-INPUT",
    "P-ROW",
    "P-COLUMN",
    "T-SPACE",
    "T-SURFACE",
    "T-INK",
    "T-LINE",
    "T-TEXT-ROLES",
];

/// Every story lays out with nothing overflowing, in each of the eight
/// languages, at the default size, at the smallest, and at every preview
/// width; and every one of them is the record's registered story.
#[test]
fn every_story_composes_in_every_language_and_at_every_width() {
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            for id in STORIED {
                for size in [SIZE, SMALLEST] {
                    let (mut lab, mut engine) = showing(id, size);
                    let context = format!("{id} in {lang:?} at {size:?}");
                    let story = lab.story.as_ref().expect("the record has a story");
                    assert_eq!(Some(story.id()), record(id).story_id(), "{context}");
                    for width in PreviewWidth::ALL {
                        lab.set_width(width);
                        draw(&mut lab, &mut engine, size);
                        assert_sound(&lab, &format!("{context} at {width:?}"));
                        let page = frame(&lab).node(PAGE).unwrap().rect;
                        let stage = lab.rect(STAGE).unwrap();
                        assert!(
                            stage.x >= page.x && stage.x + stage.w <= page.x + page.w,
                            "{context} at {width:?}: {stage:?} leaves {page:?}"
                        );
                        let subject = lab.rect(SUBJECT).expect("the component is on the stage");
                        assert!(
                            subject.x >= stage.x && subject.x + subject.w <= stage.x + stage.w,
                            "{context} at {width:?}: {subject:?} leaves {stage:?}"
                        );
                    }
                }
            }
        });
    }
    assert_eq!(
        registry::desktop()
            .filter(|entry| entry.story_id().is_some())
            .map(|entry| entry.catalog_id)
            .collect::<Vec<_>>()
            .len(),
        STORIED.len(),
        "every story is exercised here"
    );
}

/// The text input on the stage is the shared field: typing in it is counted
/// under it, its hint and its box are the two inputs, and Reset empties it.
#[test]
fn the_text_input_is_typed_in_and_restyled() {
    english(|| {
        let (mut lab, mut engine) = showing("C-TEXT-INPUT", SIZE);
        let placeholder = control_key(Control::Placeholder);
        let style = control_key(Control::Style);
        let stops = lab.laid.as_ref().unwrap().stops();
        assert_eq!(
            stops[stops.len() - 4..],
            [PAGE, SUBJECT, placeholder, style]
        );
        assert_eq!(lab.result_line().as_deref(), Some("Characters typed: 0"));
        let (x, y) = centre(&lab, SUBJECT);
        assert_eq!(lab.cursor_icon(x, y), CursorIcon::Text);

        assert!(press(&mut lab, &mut engine, SUBJECT));
        assert_eq!(lab.focus, Some(SUBJECT));
        type_text(&mut lab, &mut engine, "hello");
        let Some(Story::Field(story)) = &lab.story else {
            panic!("the text input's story is a field");
        };
        assert_eq!(story.typed(), "hello");
        assert_eq!(lab.result_line().as_deref(), Some("Characters typed: 5"));
        // An input method opens its candidates at this field's caret.
        let field = lab.rect(SUBJECT).unwrap();
        let caret = lab.ime_cursor_area().expect("the field is typed in");
        assert!(caret.x > field.x && caret.x <= field.x + field.w);
        assert!(lab.ime(&mut engine, ImeEvent::Commit { text: "世界" }));
        assert_eq!(lab.result_line().as_deref(), Some("Characters typed: 7"));
        // Enter in a field presses nothing.
        assert!(!key(&mut lab, &mut engine, NamedKey::Enter));

        // The hint is the placeholder input's text.
        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(placeholder));
        type_text(&mut lab, &mut engine, "Your name");
        let Some(Story::Field(story)) = &lab.story else {
            unreachable!()
        };
        assert_eq!(story.placeholder(), "Your name");
        assert_eq!(
            story.typed(),
            "hello世界",
            "typing a hint is not typing in the field"
        );

        // The box is chosen on segments, by pointer and by keyboard.
        assert!(tab(&mut lab, &mut engine));
        assert_eq!(lab.focus, Some(style));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        let Some(Story::Field(story)) = &lab.story else {
            unreachable!()
        };
        assert_eq!(story.style(), FieldStyle::Search);
        let segments = lab
            .laid
            .as_ref()
            .unwrap()
            .segments_of(Control::Style)
            .cloned()
            .unwrap();
        assert_eq!(segments.segments.len(), 3);
        let (sx, sy) = segments.segments[2].centre();
        assert!(lab.click(&mut engine, sx, sy, false));
        draw(&mut lab, &mut engine, SIZE);
        let Some(Story::Field(story)) = &lab.story else {
            unreachable!()
        };
        assert_eq!(story.style(), FieldStyle::Inline);
        assert_sound(&lab, "a restyled text input");

        // To a screen reader the component is a text field with its value.
        let subject = node(&lab, "s:ui-lab#subject");
        assert_eq!(subject.role, NativeRole::TextInput);
        assert_eq!(subject.label.as_deref(), Some("Text input"));
        assert_eq!(subject.value.as_deref(), Some("hello世界"));
        assert!(subject.actions.contains(&Action::SetValue));
        assert_eq!(
            node(&lab, "s:ui-lab.control#placeholder").value.as_deref(),
            Some("Your name")
        );
        assert_eq!(
            node(&lab, "s:ui-lab.style#inline").toggled,
            Some(Toggled::True)
        );
        assert_eq!(
            node(&lab, "s:ui-lab#result").label.as_deref(),
            Some("Characters typed: 7")
        );

        assert!(press(&mut lab, &mut engine, RESET));
        assert_eq!(lab.story, Story::open(registry::TEXT_INPUT_STORY));
        assert_eq!(lab.field(SUBJECT).unwrap().raw_text(), "");
        assert_eq!(lab.field(placeholder).unwrap().raw_text(), "Type something");
        assert_eq!(lab.result_line().as_deref(), Some("Characters typed: 0"));

        // Showing another record drops what was typed; coming back starts
        // the story again.
        type_text_into(&mut lab, &mut engine, SUBJECT, "kept?");
        lab.show(&mut engine, record("T-SPACE"));
        lab.show(&mut engine, record("C-TEXT-INPUT"));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(lab.story, Story::open(registry::TEXT_INPUT_STORY));
        assert_eq!(lab.field(SUBJECT).unwrap().raw_text(), "");
    });
}

fn type_text_into(lab: &mut Lab, engine: &mut TextEngine, key: Key, typed: &str) {
    press(lab, engine, key);
    type_text(lab, engine, typed);
}

/// Where the three boxes of a layout story stand, along the stack's axis:
/// the space before the first, between each pair, and after the last.
fn spacing(lab: &Lab, horizontal: bool) -> (f32, Vec<f32>, f32) {
    let stack = lab.rect(SUBJECT).unwrap();
    let items: Vec<Rect> = frame(lab)
        .named("Layout item")
        .map(|node| node.rect)
        .collect();
    assert_eq!(items.len(), 3);
    let along = |rect: Rect| {
        if horizontal {
            (rect.x, rect.w)
        } else {
            (rect.y, rect.h)
        }
    };
    let (stack_at, stack_len) = along(stack);
    let before = along(items[0]).0 - stack_at;
    let between = items
        .windows(2)
        .map(|pair| along(pair[1]).0 - (along(pair[0]).0 + along(pair[0]).1))
        .collect();
    let last = along(items[2]);
    (before, between, stack_at + stack_len - (last.0 + last.1))
}

/// The row and the column are the element layer's own: the space between
/// their boxes is the gap step chosen and the space round them the padding
/// step, each chosen from the whole spacing scale in a list.
#[test]
fn a_layout_storys_gap_and_padding_are_drawn_by_the_element_layer() {
    english(|| {
        let px = |step: Space| step.px(Profile::Desktop);
        for (id, horizontal) in [("P-ROW", true), ("P-COLUMN", false)] {
            let (mut lab, mut engine) = showing(id, SIZE);
            let (before, between, after) = spacing(&lab, horizontal);
            assert_eq!(before, px(Space::S4), "{id}: the padding");
            assert_eq!(after, px(Space::S4), "{id}");
            assert_eq!(between, [px(Space::S2), px(Space::S2)], "{id}: the gap");
            // The boxes line up across the other axis.
            let items: Vec<Rect> = frame(&lab)
                .named("Layout item")
                .map(|node| node.rect)
                .collect();
            if horizontal {
                assert!(items.iter().all(|item| item.y == items[0].y));
            } else {
                assert!(items.iter().all(|item| item.x == items[0].x));
            }
            assert_eq!(
                lab.result_line().as_deref(),
                Some(if horizontal {
                    "el::row(\"Layout under test\").gap(Space::S2).pad(Space::S4)"
                } else {
                    "el::col(\"Layout under test\").gap(Space::S2).pad(Space::S4)"
                })
            );

            // The gap, from the list, with the pointer.
            let gap = control_key(Control::Gap);
            assert!(lab.is_dropdown(gap));
            assert_eq!(lab.chosen(gap).unwrap().label, "S2 (8)");
            assert!(press(&mut lab, &mut engine, gap));
            let open = lab.menu.as_ref().expect("the gap's list is open");
            assert_eq!(open.choices.len(), Space::ALL.len());
            assert_eq!(open.menu.hover, Some(4), "on the step in force");
            let slot = Space::ALL.iter().position(|s| *s == Space::S8).unwrap();
            let (x, y) = open.menu.item_rect(SIZE, slot).centre();
            assert!(lab.click(&mut engine, x, y, false));
            draw(&mut lab, &mut engine, SIZE);
            assert_eq!(spacing(&lab, horizontal).1, [px(Space::S8), px(Space::S8)]);
            assert_eq!(
                spacing(&lab, horizontal).0,
                px(Space::S4),
                "padding untouched"
            );

            // The padding, from the keyboard.
            let padding = control_key(Control::Padding);
            while lab.focus != Some(padding) {
                assert!(tab(&mut lab, &mut engine));
            }
            assert!(key(&mut lab, &mut engine, NamedKey::Enter));
            assert!(key(&mut lab, &mut engine, NamedKey::Home));
            assert!(key(&mut lab, &mut engine, NamedKey::Enter));
            assert_eq!(lab.focus, Some(padding));
            let (before, _, after) = spacing(&lab, horizontal);
            assert_eq!((before, after), (px(Space::S0), px(Space::S0)));
            assert!(
                lab.result_line()
                    .unwrap()
                    .ends_with(".gap(Space::S8).pad(Space::S0)")
            );
            assert_sound(&lab, id);

            // Announced as a combo box with its value, its list a menu
            // named for the input.
            let field = node(&lab, "s:ui-lab.control#gap");
            assert_eq!(field.role, NativeRole::ComboBox);
            assert_eq!(field.label.as_deref(), Some("Gap"));
            assert_eq!(field.value.as_deref(), Some("S8 (32)"));
            assert!(press(&mut lab, &mut engine, gap));
            let menu = node(&lab, "s:ui-lab#menu");
            assert_eq!(menu.label.as_deref(), Some("Gap"));
            assert_eq!(menu.children.len(), Space::ALL.len());
            assert_eq!(
                node(&lab, "s:ui-lab.option#S8").toggled,
                Some(Toggled::True)
            );
            assert!(key(&mut lab, &mut engine, NamedKey::Escape));
            let subject = node(&lab, "s:ui-lab#subject");
            assert_eq!(subject.role, NativeRole::Group);

            assert!(press(&mut lab, &mut engine, RESET));
            let (before, between, _) = spacing(&lab, horizontal);
            assert_eq!(before, px(Space::S4));
            assert_eq!(between, [px(Space::S2), px(Space::S2)]);
        }
    });
}

/// The spacing scale's page is the scale: a row a step, in the table's
/// order, each with a bar exactly as long as the step.
#[test]
fn the_spacing_scale_is_drawn_from_the_scale() {
    english(|| {
        let (lab, _) = showing("T-SPACE", SIZE);
        let frame = frame(&lab);
        let rows: Vec<Key> = frame.named("Token").filter_map(|node| node.key).collect();
        let expected: Vec<Key> = Space::ALL
            .into_iter()
            .map(|step| Key::surface(TOKEN, step.name()))
            .collect();
        assert_eq!(rows, expected);
        let bars: Vec<f32> = frame.named("Space bar").map(|node| node.rect.w).collect();
        let lengths: Vec<f32> = Space::ALL
            .into_iter()
            .map(|step| step.px(Profile::Desktop))
            .collect();
        assert_eq!(bars, lengths, "a bar is as long as its step");
        assert!(bars.iter().all(|bar| *bar <= TOKEN_SAMPLE_WIDTH.value));

        // A table has nothing to change: no inputs, and Reset is off.
        assert_eq!(frame.named("Property").count(), 0);
        assert!(frame.node(RESET).unwrap().disabled);
        assert_eq!(lab.result_line(), None);
        assert_eq!(frame.named("Click result").count(), 0);
        // Each step is announced by its name and what it comes to.
        let step = node(&lab, "s:ui-lab.token#S4");
        assert_eq!(step.role, NativeRole::Label);
        assert_eq!(step.label.as_deref(), Some("S4: 16"));
        assert_eq!(
            node(&lab, "s:ui-lab.token#S0_5").label.as_deref(),
            Some("S0_5: 2")
        );
        assert_eq!(
            published(&lab)
                .iter()
                .filter(|node| node.occurrence.object.to_string() == "s:ui-lab.token")
                .count(),
            Space::ALL.len()
        );
    });
}

/// A color role's page paints each role with the role itself, so it shows
/// whatever the theme in force makes of it; a text role's page sets a sample
/// in each role.
#[test]
fn the_color_and_type_pages_are_drawn_with_the_roles() {
    english(|| {
        // Surfaces: each role as a swatch, on the ground and on a raised
        // surface.
        let (lab, _) = showing("T-SURFACE", SIZE);
        let swatches: Vec<Option<Surface>> = frame(&lab)
            .named("Surface swatch")
            .map(|node| node.look.bg)
            .collect();
        let expected: Vec<Option<Surface>> = Surface::ALL
            .iter()
            .flat_map(|role| [Some(*role), Some(*role)])
            .collect();
        assert_eq!(swatches, expected);
        let backings: Vec<Option<Surface>> = frame(&lab)
            .named("Surface backing")
            .map(|node| node.look.bg)
            .take(2)
            .collect();
        assert_eq!(backings, [Some(Surface::Ground), Some(Surface::Raised)]);
        assert_eq!(
            node(&lab, "s:ui-lab.token#Surface::Raised")
                .label
                .as_deref(),
            Some("Surface::Raised: panel_bg")
        );

        // Inks and lines: a row a role.
        let (lab, _) = showing("T-INK", SIZE);
        assert_eq!(frame(&lab).named("Ink sample").count(), Ink::ALL.len());
        assert_eq!(frame(&lab).named("Token").count(), Ink::ALL.len());
        let (lab, _) = showing("T-LINE", SIZE);
        assert_eq!(frame(&lab).named("Line sample").count(), Line::ALL.len());
        let hairline = frame(&lab)
            .nodes
            .iter()
            .position(|node| node.name == "Line sample")
            .map(|at| frame(&lab).nodes[at + 1].rect)
            .unwrap();
        assert_eq!(hairline.h, theme::BORDER_W as f32, "a hairline, drawn");
        assert_eq!(hairline.w, TOKEN_SAMPLE_WIDTH.value);

        // Text roles: the sample is set in the role, so a larger role's
        // sample is a taller line.
        let (lab, _) = showing("T-TEXT-ROLES", SIZE);
        let samples: Vec<f32> = frame(&lab)
            .named("Token sample")
            .map(|node| node.rect.h)
            .collect();
        let heights: Vec<f32> = roles::ALL
            .into_iter()
            .map(|role| role.style().metrics.line_height)
            .collect();
        assert_eq!(samples.len(), roles::ALL.len());
        for (sample, height) in samples.iter().zip(&heights) {
            assert!(sample >= height, "{sample} for a {height} line");
        }
        assert!(
            heights.iter().any(|height| *height != heights[0]),
            "the roles differ in size"
        );
        assert!(
            node(&lab, "s:ui-lab.token#subtitle")
                .label
                .as_deref()
                .is_some_and(|label| label.starts_with("subtitle: "))
        );
    });
}

/// A token page follows the theme the toolbar chooses: its swatches resolve
/// under the new theme like everything else in the window.
#[test]
fn a_color_page_is_shown_in_the_theme_in_force() {
    use koru_draw::theme::themes;
    struct Restore(&'static themes::Theme);
    impl Drop for Restore {
        fn drop(&mut self) {
            themes::set_active(self.0);
        }
    }
    let _lock = crate::tests::lock_theme();
    let _restore = Restore(themes::active());
    themes::set_active(&themes::LIGHT_THEME);
    english(|| {
        let (mut lab, mut engine) = showing("T-SURFACE", SIZE);
        let themed = |lab: &Lab| -> Vec<&'static str> {
            frame(lab)
                .named("Surface swatch")
                .map(|node| node.cx.theme.key)
                .collect()
        };
        assert!(themed(&lab).iter().all(|key| *key == "light"));
        assert!(lab.set_theme(&mut engine, &themes::DARK_THEME));
        draw(&mut lab, &mut engine, SIZE);
        let dark = themed(&lab);
        assert_eq!(dark.len(), Surface::ALL.len() * 2);
        assert!(dark.iter().all(|key| *key == "dark"));
        assert_sound(&lab, "the surfaces in the dark theme");
    });
}
