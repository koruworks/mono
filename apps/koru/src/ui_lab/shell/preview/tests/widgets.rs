// SPDX-License-Identifier: MPL-2.0

//! The widget stories, driven through the window. One test holds every
//! registered story to the rules they share; the rest check what is
//! distinctive about each component.

use koru_ui::{Dropdown, Toggle};

use super::*;
use crate::ui_lab::stories;
use crate::ui_lab::story::WidgetStory;

/// A Lab showing the record whose story is `story_id`, one frame drawn.
fn staged(story_id: &str) -> (Lab, TextEngine) {
    let (entry, _) = registry::story(story_id).expect("the story is registered");
    showing(entry.catalog_id, SIZE)
}

fn widget(lab: &Lab) -> &WidgetStory {
    lab.story
        .as_ref()
        .and_then(Story::as_widget)
        .expect("a widget story is on the stage")
}

fn click(lab: &mut Lab, engine: &mut TextEngine, at: (f32, f32)) -> bool {
    let changed = lab.click(engine, at.0, at.1, false);
    lab.mouse_up();
    draw(lab, engine, SIZE);
    changed
}

/// Where the part called `part` was drawn.
fn part_at(lab: &Lab, part: &str) -> (f32, f32) {
    lab.laid
        .as_ref()
        .unwrap()
        .parts
        .iter()
        .find(|placed| placed.part == part)
        .unwrap_or_else(|| panic!("the component has no `{part}`"))
        .rect
        .centre()
}

/// Press the part called `part` where it was drawn.
fn click_part(lab: &mut Lab, engine: &mut TextEngine, part: &str) -> bool {
    let at = part_at(lab, part);
    click(lab, engine, at)
}

/// **What every widget story owes.** The production component is what was
/// drawn, under its own inspector identity, inside the slot; its parts lie
/// inside that slot, are each published to a screen reader where they were
/// drawn with a name and what can be done to them, and are each pressed by
/// the pointer where they were drawn; the component is a Tab stop; every
/// input the recipe lists is drawn and changes the story; and Reset puts
/// all of it back.
#[test]
fn every_widget_story_is_drawn_pressed_and_published_from_one_geometry() {
    english(|| {
        for story in stories::ALL {
            let id = story.id();
            let (entry, recipe) = registry::story(id).unwrap();
            let (mut lab, mut engine) = staged(id);
            assert_sound(&lab, id);
            assert_eq!(widget(&lab).widget().id(), id);

            // The production component, by the name it gives the inspector.
            let records = inspected(&mut lab, &mut engine, SIZE);
            let slot = lab.rect(SUBJECT).unwrap();
            for identity in entry.identities {
                let drawn = records.iter().any(|record| {
                    record.name == *identity
                        && record.kind == "component"
                        && f64::from(slot.x) - 8.0 <= record.bounds[0]
                        && record.bounds[2] <= f64::from(slot.x + slot.w) + 8.0
                        && f64::from(slot.y) - 8.0 <= record.bounds[1]
                        && record.bounds[3] <= f64::from(slot.y + slot.h) + 8.0
                });
                assert!(drawn, "{id}: no `{identity}` was drawn in the slot");
            }

            // Its parts, inside the slot, each with a name of its own.
            let parts = lab.laid.as_ref().unwrap().parts.clone();
            assert!(!parts.is_empty(), "{id}");
            let mut names: Vec<&str> = parts.iter().map(|part| part.part).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), parts.len(), "{id}: a part is named twice");
            for part in &parts {
                let r = part.rect;
                assert!(
                    r.x >= slot.x - 0.01
                        && r.y >= slot.y - 0.01
                        && r.x + r.w <= slot.x + slot.w + 0.01
                        && r.y + r.h <= slot.y + slot.h + 0.01,
                    "{id}: `{}` at {r:?} leaves the slot {slot:?}",
                    part.part
                );
                // Published where it was drawn, with a name, and pressable
                // exactly when the pointer can press it.
                let published = node(&lab, &format!("s:ui-lab.part#{}", part.part));
                assert!(
                    published.label.as_deref().is_some_and(|l| !l.is_empty()),
                    "{id}: `{}` has no name",
                    part.part
                );
                let bounds = published.bounds.expect("a part is placed");
                assert_eq!(
                    (bounds.x, bounds.y, bounds.w, bounds.h),
                    (r.x, r.y, r.w, r.h)
                );
                assert_eq!(
                    published.actions.contains(&Action::Click),
                    part.pressable(),
                    "{id}: `{}`",
                    part.part
                );
                assert_eq!(published.disabled, part.disabled);
            }
            assert!(parts.iter().any(|part| part.pressable()), "{id}");

            // One Tab stop, published as a group named for the record.
            assert!(
                lab.laid.as_ref().unwrap().stops().contains(&SUBJECT),
                "{id}"
            );
            let subject = node(&lab, "s:ui-lab#subject");
            assert_eq!(subject.role, NativeRole::Group, "{id}");
            assert_eq!(
                subject.label.as_deref(),
                Some(koru_i18n::t(koru_i18n::Lang::En, entry.name))
            );

            // The pointer presses each part where it was drawn, and the
            // keyboard is then on that part.
            for (at, part) in parts
                .iter()
                .enumerate()
                .filter(|(_, part)| part.pressable())
            {
                click(&mut lab, &mut engine, part.rect.centre());
                assert_eq!(lab.focus, Some(SUBJECT), "{id}");
                assert_eq!(widget(&lab).cursor(), at, "{id}: `{}`", part.part);
                if lab.menu.is_some() {
                    assert!(key(&mut lab, &mut engine, NamedKey::Escape));
                }
                assert_sound(&lab, id);
            }
            // The keyboard: arrows, then Enter, never leave it unsound.
            for pressed in [NamedKey::ArrowRight, NamedKey::ArrowLeft, NamedKey::Enter] {
                key(&mut lab, &mut engine, pressed);
                if lab.menu.is_some() {
                    key(&mut lab, &mut engine, NamedKey::Escape);
                }
                assert_sound(&lab, id);
            }
            assert!(result(&lab).chars().count() > 1, "{id}");

            // Every input is drawn, and changes the story.
            for control in recipe.controls {
                let at = control_key(*control);
                assert!(frame(&lab).contains(at), "{id}: {control:?} is not drawn");
                let before = lab.story.clone();
                match control.input() {
                    registry::Input::Toggle => {
                        assert!(press(&mut lab, &mut engine, at), "{id}: {control:?}");
                    }
                    registry::Input::Choice => {
                        lab.set_focus(&mut engine, Some(at));
                        let story = lab.story.as_mut().unwrap();
                        assert!(
                            story.step(*control, true) || story.step(*control, false),
                            "{id}: {control:?}"
                        );
                    }
                    registry::Input::Text => {
                        press(&mut lab, &mut engine, at);
                        type_text(&mut lab, &mut engine, "x");
                    }
                }
                draw(&mut lab, &mut engine, SIZE);
                assert_ne!(lab.story, before, "{id}: {control:?} changed nothing");
                assert_sound(&lab, &format!("{id} after {control:?}"));
            }

            // Reset: the story as it opened.
            press(&mut lab, &mut engine, RESET);
            assert_eq!(lab.story, Story::open(id), "{id}");
        }
    });
}

#[test]
fn the_switch_is_flipped_and_takes_no_press_while_it_is_off_or_waiting() {
    english(|| {
        let (mut lab, mut engine) = staged(registry::TOGGLE_STORY);
        assert_eq!(result(&lab), "Off");
        let switch = node(&lab, "s:ui-lab.part#switch");
        assert_eq!(switch.role, NativeRole::Switch);
        assert_eq!(switch.toggled, Some(Toggled::False));
        assert!(press(&mut lab, &mut engine, SUBJECT));
        assert_eq!(result(&lab), "On");
        assert!(key(&mut lab, &mut engine, NamedKey::Space));
        assert_eq!(result(&lab), "Off");

        // The leading glyph widens the control by its slot and its gap.
        let bare = lab.rect(SUBJECT).unwrap().w;
        assert_eq!(bare, Toggle::TRACK_W);
        press(&mut lab, &mut engine, control_key(Control::Leading));
        assert_eq!(
            lab.rect(SUBJECT).unwrap().w,
            Toggle::TRACK_W + Toggle::ICON_GAP + Toggle::ICON_W
        );

        // Waiting on a change, or switched off: drawn as such, and inert.
        for inert in [Control::Pending, Control::Disabled] {
            press(&mut lab, &mut engine, control_key(inert));
            let records = inspected(&mut lab, &mut engine, SIZE);
            let drawn = component_at(&records, "KoruToggle", lab.rect(SUBJECT).unwrap());
            let said = property(drawn, "on / enabled / pending");
            assert!(said == "false / true / true" || said == "false / false / false");
            assert!(node(&lab, "s:ui-lab.part#switch").disabled);
            click_part(&mut lab, &mut engine, "switch");
            assert_eq!(result(&lab), "Off", "{inert:?}");
            press(&mut lab, &mut engine, control_key(inert));
        }
    });
}

#[test]
fn a_chip_is_chosen_and_the_first_one_is_relabelled() {
    english(|| {
        let (mut lab, mut engine) = staged(registry::CHIP_STORY);
        assert_eq!(result(&lab), "Chosen: Compact");
        assert!(click_part(&mut lab, &mut engine, "third"));
        assert_eq!(result(&lab), "Chosen: Standard");
        assert_eq!(
            node(&lab, "s:ui-lab.part#third").toggled,
            Some(Toggled::True)
        );
        // Each chip is the production one, and says which is chosen.
        let records = inspected(&mut lab, &mut engine, SIZE);
        let chips: Vec<&str> = records
            .iter()
            .filter(|record| record.name == "KoruChip")
            .map(|record| property(record, "selected / hovered / focused"))
            .collect();
        assert_eq!(
            chips,
            [
                "false / false / false",
                "false / false / false",
                "true / false / true"
            ]
        );
        // The label input is the first chip's label; the leading slot
        // widens every chip.
        let before = lab.laid.as_ref().unwrap().parts[0].rect.w;
        press(&mut lab, &mut engine, control_key(Control::Label));
        type_text(&mut lab, &mut engine, " wider");
        assert_eq!(
            node(&lab, "s:ui-lab.part#first").label.as_deref(),
            Some("Compact wider")
        );
        let relabelled = lab.laid.as_ref().unwrap().parts[0].rect.w;
        assert!(relabelled > before);
        press(&mut lab, &mut engine, control_key(Control::Leading));
        assert!(lab.laid.as_ref().unwrap().parts[0].rect.w > relabelled);
    });
}

#[test]
fn the_segmented_control_is_one_stop_whose_arrows_choose() {
    english(|| {
        let (mut lab, mut engine) = staged(registry::SEGMENTED_STORY);
        assert_eq!(result(&lab), "Chosen: Compact");
        lab.set_focus(&mut engine, Some(SUBJECT));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        assert!(
            !key(&mut lab, &mut engine, NamedKey::ArrowRight),
            "it stops"
        );
        assert_eq!(result(&lab), "Chosen: Standard");
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        let records = inspected(&mut lab, &mut engine, SIZE);
        let drawn = component_at(&records, "KoruSegmented", lab.rect(SUBJECT).unwrap());
        assert_eq!(property(drawn, "selected"), "1");
        // A press on a segment chooses it.
        assert!(click_part(&mut lab, &mut engine, "first"));
        assert_eq!(result(&lab), "Chosen: Compact");
    });
}

#[test]
fn the_dropdown_opens_its_own_list_and_shows_what_was_chosen() {
    english(|| {
        let (mut lab, mut engine) = staged(registry::DROPDOWN_STORY);
        assert_eq!(result(&lab), "Chosen: English");
        let field = lab.rect(SUBJECT).unwrap();
        assert_eq!(field.h, Dropdown::H);
        // A press opens the shared list under the field, on the value in
        // force; the field is drawn open while it shows.
        assert!(click_part(&mut lab, &mut engine, "field"));
        let open = lab.menu.as_ref().expect("the field opened its list");
        assert_eq!(open.owner, SUBJECT);
        assert_eq!(open.choices.len(), koru_i18n::Lang::ALL.len());
        assert_eq!(open.menu.hover, Some(0));
        assert_eq!(open.menu.panel(SIZE).x, field.x);
        let records = inspected(&mut lab, &mut engine, SIZE);
        let drawn = component_at(&records, "KoruDropdown", field);
        assert!(property(drawn, "open / hovered / enabled").starts_with("true"));
        // The arrows step it and Enter chooses.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert!(lab.menu.is_none());
        let chosen = koru_i18n::Lang::ALL[1].endonym();
        assert_eq!(result(&lab), format!("Chosen: {chosen}"));
        let published = node(&lab, "s:ui-lab.part#field");
        assert_eq!(published.role, NativeRole::ComboBox);
        assert_eq!(published.value.as_deref(), Some(chosen));
        // Switched off, it is drawn so and opens nothing.
        press(&mut lab, &mut engine, control_key(Control::Disabled));
        click_part(&mut lab, &mut engine, "field");
        assert!(lab.menu.is_none());
        assert!(node(&lab, "s:ui-lab.part#field").disabled);
    });
}

#[test]
fn a_color_is_chosen_from_the_swatches_and_from_the_row() {
    english(|| {
        for id in [registry::SWATCH_STORY, registry::COLOR_PICKER_ROW_STORY] {
            let (mut lab, mut engine) = staged(id);
            assert_eq!(result(&lab), "Chosen: none", "{id}");
            assert!(click_part(&mut lab, &mut engine, "success"));
            assert_eq!(result(&lab), "Chosen: success", "{id}");
            // Four production swatches: the first hollow, one chosen, and
            // the keyboard's ring on the one that was pressed.
            let records = inspected(&mut lab, &mut engine, SIZE);
            let swatches: Vec<(&str, &str)> = records
                .iter()
                .filter(|record| record.name == "KoruSwatch")
                .map(|record| {
                    (
                        property(record, "color"),
                        property(record, "selected / hovered / focused"),
                    )
                })
                .collect();
            assert_eq!(swatches.len(), 4, "{id}");
            assert_eq!(swatches[0], ("none", "false / false / false"), "{id}");
            assert_eq!(swatches[2].1, "true / false / true", "{id}");
            // The rings reach outside a square, and stay inside the slot.
            let slot = lab.rect(SUBJECT).unwrap();
            for part in &lab.laid.as_ref().unwrap().parts {
                let reach = koru_ui::swatch::Swatch::RING_REACH;
                assert!(part.rect.x - reach >= slot.x - 0.01, "{id}");
                assert!(part.rect.y - reach >= slot.y - 0.01, "{id}");
                assert!(part.rect.x + part.rect.w + reach <= slot.x + slot.w + 0.01);
                assert!(part.rect.y + part.rect.h + reach <= slot.y + slot.h + 0.01);
            }
        }
        // The row's label is the caller's text; without one the row is
        // just its swatches, and shorter by the label's line.
        let (mut lab, mut engine) = staged(registry::COLOR_PICKER_ROW_STORY);
        let records = inspected(&mut lab, &mut engine, SIZE);
        let row = records
            .iter()
            .find(|record| record.name == "ColorPickerRow")
            .unwrap();
        assert_eq!(property(row, "label"), "Color");
        let labelled = lab.rect(SUBJECT).unwrap().h;
        lab.set_field(&mut engine, control_key(Control::Label), "");
        let records = inspected(&mut lab, &mut engine, SIZE);
        let row = records
            .iter()
            .find(|record| record.name == "ColorPickerRow")
            .unwrap();
        assert_eq!(property(row, "label"), "none");
        assert!(lab.rect(SUBJECT).unwrap().h < labelled);
    });
}

#[test]
fn the_stepper_is_nudged_to_either_end_of_its_scale() {
    english(|| {
        let (mut lab, mut engine) = staged(registry::STEPPER_STORY);
        assert_eq!(result(&lab), "Value: 3");
        assert!(click_part(&mut lab, &mut engine, "more"));
        assert!(click_part(&mut lab, &mut engine, "more"));
        assert_eq!(result(&lab), "Value: 5");
        // At the top, More is dimmed and takes no press; the readout never
        // does.
        assert!(node(&lab, "s:ui-lab.part#more").disabled);
        click_part(&mut lab, &mut engine, "more");
        click_part(&mut lab, &mut engine, "value");
        assert_eq!(result(&lab), "Value: 5");
        let records = inspected(&mut lab, &mut engine, SIZE);
        let drawn = component_at(&records, "KoruStepper", lab.rect(SUBJECT).unwrap());
        assert_eq!(property(drawn, "value"), "5");
        assert_eq!(property(drawn, "less / more enabled"), "true / false");
        let value = node(&lab, "s:ui-lab.part#value");
        assert_eq!(value.role, NativeRole::Label);
        assert_eq!(value.value.as_deref(), Some("5"));
        // The keyboard: from More the arrow goes back to Less, past the
        // readout, and Enter nudges.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        assert_eq!(widget(&lab).cursor(), 0);
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert_eq!(result(&lab), "Value: 4");
    });
}

#[test]
fn a_preview_tile_is_chosen_and_shares_the_row_with_the_other() {
    english(|| {
        let (mut lab, mut engine) = staged(registry::PREVIEW_TILE_STORY);
        assert_eq!(result(&lab), "Chosen: Light");
        assert!(click_part(&mut lab, &mut engine, "dark"));
        assert_eq!(result(&lab), "Chosen: Dark");
        let records = inspected(&mut lab, &mut engine, SIZE);
        let tiles: Vec<(&str, &str)> = records
            .iter()
            .filter(|record| record.name == "KoruPreviewTile")
            .map(|record| {
                (
                    property(record, "label"),
                    property(record, "selected / hovered"),
                )
            })
            .collect();
        assert_eq!(
            tiles,
            [("Light", "false / false"), ("Dark", "true / false")]
        );
        let parts = &lab.laid.as_ref().unwrap().parts;
        assert_eq!(parts[0].rect.w, parts[1].rect.w);
        assert_eq!(
            parts[1].rect.x - (parts[0].rect.x + parts[0].rect.w),
            koru_ui::preview_tile::PreviewTile::GAP
        );
    });
}
