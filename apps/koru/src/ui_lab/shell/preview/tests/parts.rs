// SPDX-License-Identifier: MPL-2.0

//! The story machinery itself, driven through the window with a widget
//! story that exists only here: a recipe with free text, two choices and a
//! yes-or-no input, and a component of three parts — a button, a button
//! that stays down and a field that opens a list. What it draws is beside
//! the point; what is checked is that every kind of input and every kind of
//! part is laid out, pressed, stepped, published and reset from one place.

use koru_draw::geometry::Rect;
use koru_workspace_host::accessibility::Resolved;

use super::*;
use crate::ui_lab::registry::{A2ui, Declared, Member, OptionSet, Recipe, Styles};
use crate::ui_lab::stories::{Canvas, Part, PartRole, Pointed, Widget};
use crate::ui_lab::story::{OptionLabel, StoryOption, WidgetStory};

const fn member(ident: &'static str) -> Member {
    Member { ident, label: None }
}

static SETS: [OptionSet; 2] = [
    OptionSet {
        name: koru_i18n::Key::UiLabControlTone,
        source: "probe::Tone",
        file: "",
        declared: Declared::Enum,
        members: &[member("Default"), member("Danger")],
    },
    OptionSet {
        name: koru_i18n::Key::UiLabControlSize,
        source: "probe::Size",
        file: "",
        declared: Declared::Consts,
        members: &[member("STANDARD"), member("INLINE"), member("COMPACT")],
    },
];

static RECIPE: Recipe = Recipe {
    symbol: "probe::Probe",
    styles: Styles::Unstyled,
    controls: &[
        Control::Label,
        Control::Tone,
        Control::Size,
        Control::Disabled,
    ],
    options: &SETS,
    a2ui: A2ui::NotSupported,
    story: registry::Story::NoStoryYet,
};

struct Probe;

const LETTERS: [&str; 3] = ["a", "b", "c"];

impl Widget for Probe {
    fn id(&self) -> &'static str {
        "PROBE/parts"
    }

    fn sample(&self, control: Control) -> Option<koru_i18n::Key> {
        (control == Control::Label).then_some(koru_i18n::Key::UiLabButtonSampleLabel)
    }

    fn opens_at(&self, control: Control) -> usize {
        usize::from(control == Control::Size)
    }

    fn size(&self, _: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        (300.0, 30.0)
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let at = |slot: f32| Rect::new(rect.x + slot * 100.0, rect.y, 100.0, rect.h);
        vec![
            Part::button("first", at(0.0), story.text(Control::Label)),
            Part::button("second", at(1.0), "Second")
                .role(PartRole::Pressed(story.held.on))
                .disabled(story.flag(Control::Disabled))
                .ringed(),
            Part::button("list", at(2.0), "List")
                .role(PartRole::Combo)
                .value(story.held.chosen.map_or("", |slot| LETTERS[slot])),
        ]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        ActionButton::new(story.text(Control::Label), ActionStyle::Secondary, None).draw_focused(
            canvas.scene,
            canvas.root,
            canvas.engine,
            Rect::new(rect.x, rect.y, 100.0, rect.h),
            pointed.hovered == Some(0),
            pointed.focused == Some(0),
        );
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        match part {
            0 => story.held.presses[0] += 1,
            _ => story.held.on = !story.held.on,
        }
        true
    }

    fn list(&self, story: &WidgetStory, part: usize) -> Option<Vec<StoryOption>> {
        (part == 2).then(|| {
            LETTERS
                .iter()
                .enumerate()
                .map(|(slot, letter)| StoryOption {
                    part: letter,
                    label: OptionLabel::Literal((*letter).to_owned()),
                    selected: story.held.chosen == Some(slot),
                })
                .collect()
        })
    }

    fn pick(&self, story: &mut WidgetStory, slot: usize) -> bool {
        story.held.chosen = Some(slot);
        true
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(format!(
            "{} {} {:?}",
            story.held.presses[0], story.held.on, story.held.chosen
        ))
    }
}

/// A Lab running the probe, one frame drawn.
fn probing() -> (Lab, TextEngine) {
    let (mut lab, mut engine) = showing("C-BUTTON", SIZE);
    lab.start(
        &mut engine,
        Some(Story::Widget(WidgetStory::open(&Probe, &RECIPE))),
    );
    draw(&mut lab, &mut engine, SIZE);
    (lab, engine)
}

fn probe(lab: &Lab) -> &WidgetStory {
    lab.story
        .as_ref()
        .and_then(Story::as_widget)
        .expect("the probe is on the stage")
}

fn click(lab: &mut Lab, engine: &mut TextEngine, at: (f32, f32)) -> bool {
    let changed = lab.click(engine, at.0, at.1, false);
    lab.mouse_up();
    draw(lab, engine, SIZE);
    changed
}

#[test]
fn a_story_offers_free_text_several_choices_and_a_switch() {
    english(|| {
        let (mut lab, mut engine) = probing();
        assert_sound(&lab, "the probe");
        // Free text: a field holding the sample.
        let label = control_key(Control::Label);
        assert_eq!(lab.field(label).unwrap().raw_text(), "Save changes");
        assert_eq!(probe(&lab).text(Control::Label), "Save changes");

        // Two choices, each its own segmented control with its own options.
        let laid = lab.laid.as_ref().unwrap();
        assert_eq!(laid.segments.len(), 2);
        let tone = laid.segments_of(Control::Tone).cloned().unwrap();
        let size = laid.segments_of(Control::Size).cloned().unwrap();
        assert_eq!((tone.segments.len(), size.segments.len()), (2, 3));
        assert_ne!(tone.bounds.y, size.bounds.y);
        assert_eq!(
            probe(&lab).choice(Control::Size),
            1,
            "it opens where it says"
        );
        assert_eq!(
            node(&lab, "s:ui-lab.size#INLINE").toggled,
            Some(Toggled::True)
        );
        assert_eq!(
            node(&lab, "s:ui-lab.tone#Default").toggled,
            Some(Toggled::True)
        );

        // The pointer lights a segment of the choice it is over, and a
        // press there chooses in that choice and no other.
        let over = tone.segments[1].centre();
        lab.pointer_moved(over.0, over.1);
        assert_eq!(lab.style_hover, Some((Control::Tone, 1)));
        assert!(click(&mut lab, &mut engine, size.segments[2].centre()));
        assert_eq!(probe(&lab).choice(Control::Size), 2);
        assert_eq!(probe(&lab).choice(Control::Tone), 0);
        assert_eq!(lab.focus, Some(control_key(Control::Size)));
        // The arrows step the choice the keyboard is on.
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        assert_eq!(probe(&lab).choice(Control::Size), 1);

        // A yes or a no: the shared switch, off; a press and Space flip it.
        let switch = control_key(Control::Disabled);
        let published = node(&lab, "s:ui-lab.control#disabled");
        assert_eq!(published.role, NativeRole::Switch);
        assert_eq!(published.toggled, Some(Toggled::False));
        assert!(published.actions.contains(&Action::Click));
        let slot = lab.rect(switch).unwrap();
        assert_eq!((slot.w, slot.h), (Toggle::TRACK_W, Toggle::H));
        assert!(press(&mut lab, &mut engine, switch));
        assert!(probe(&lab).flag(Control::Disabled));
        assert_eq!(
            node(&lab, "s:ui-lab.control#disabled").toggled,
            Some(Toggled::True)
        );
        assert_eq!(lab.focus, Some(switch));
        assert!(key(&mut lab, &mut engine, NamedKey::Space));
        assert!(!probe(&lab).flag(Control::Disabled));

        // Reset puts every input back.
        press(&mut lab, &mut engine, switch);
        assert!(press(&mut lab, &mut engine, RESET));
        assert_eq!(
            lab.story,
            Some(Story::Widget(WidgetStory::open(&Probe, &RECIPE)))
        );
    });
}

#[test]
fn a_component_is_pressed_stepped_and_published_by_its_parts() {
    english(|| {
        let (mut lab, mut engine) = probing();
        let subject = lab.rect(SUBJECT).unwrap();
        let parts = lab.laid.as_ref().unwrap().parts.clone();
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(part.rect.x >= subject.x && part.rect.y >= subject.y);
            assert!(part.rect.x + part.rect.w <= subject.x + subject.w + 0.01);
        }

        // The pointer: a part lights under it, and a press there presses
        // that part and puts the keyboard on it.
        let second = parts[1].rect.centre();
        lab.pointer_moved(second.0, second.1);
        assert_eq!(lab.part_hover, Some(1));
        assert!(click(&mut lab, &mut engine, second));
        assert!(probe(&lab).held.on);
        assert_eq!((lab.focus, probe(&lab).cursor()), (Some(SUBJECT), 1));
        assert!(click(&mut lab, &mut engine, parts[0].rect.centre()));
        assert_eq!(probe(&lab).held.presses[0], 1);
        assert_eq!(result(&lab), "1 true None");

        // The keyboard: the arrows move between parts, stopping at the
        // ends; Enter and Space press the one it is on.
        assert!(!key(&mut lab, &mut engine, NamedKey::ArrowLeft));
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        assert_eq!(probe(&lab).cursor(), 1);
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert!(!probe(&lab).held.on);

        // A part that is switched off is skipped by the arrows and does
        // nothing when pressed, and is published as unavailable.
        press(&mut lab, &mut engine, control_key(Control::Disabled));
        assert!(node(&lab, "s:ui-lab.part#second").disabled);
        click(&mut lab, &mut engine, second);
        assert!(!probe(&lab).held.on);
        click(&mut lab, &mut engine, parts[0].rect.centre());
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowRight));
        assert_eq!(probe(&lab).cursor(), 2);

        // A part that opens a list: the shared menu, hung under the part.
        assert!(key(&mut lab, &mut engine, NamedKey::Space));
        let open = lab.menu.as_ref().expect("the part opened its list");
        assert_eq!(open.owner, SUBJECT);
        assert_eq!(open.choices.len(), 3);
        assert_eq!(open.menu.panel(SIZE).x, parts[2].rect.x);
        assert!(key(&mut lab, &mut engine, NamedKey::ArrowDown));
        assert!(key(&mut lab, &mut engine, NamedKey::Enter));
        assert!(lab.menu.is_none());
        let chosen = probe(&lab).held.chosen.expect("an option was chosen");

        // The screen reader's tree: each part where it was drawn, with its
        // role, name, value, state and action.
        let first = node(&lab, "s:ui-lab.part#first");
        assert_eq!(first.role, NativeRole::Button);
        assert_eq!(first.label.as_deref(), Some("Save changes"));
        assert!(first.actions.contains(&Action::Click));
        let bounds = first.bounds.expect("a part is placed");
        assert_eq!((bounds.x, bounds.w), (parts[0].rect.x, parts[0].rect.w));
        assert_eq!(
            node(&lab, "s:ui-lab.part#second").toggled,
            Some(Toggled::False)
        );
        let list = node(&lab, "s:ui-lab.part#list");
        assert_eq!(list.role, NativeRole::ComboBox);
        assert_eq!(list.value.as_deref(), Some(LETTERS[chosen]));
        assert_eq!(node(&lab, "s:ui-lab#subject").role, NativeRole::Group);

        // A request on a part is the press the pointer makes.
        let request = Resolved {
            occurrence: first.occurrence,
            action: Action::Click,
            data: None,
        };
        assert!(lab.accessibility_action(&mut engine, &request));
        draw(&mut lab, &mut engine, SIZE);
        assert_eq!(probe(&lab).held.presses[0], 3);
        assert_eq!(probe(&lab).cursor(), 0);

        // Reset: the inputs, what the component remembers and where the
        // keyboard is among its parts.
        assert!(press(&mut lab, &mut engine, RESET));
        assert_eq!(
            lab.story,
            Some(Story::Widget(WidgetStory::open(&Probe, &RECIPE)))
        );
        assert_eq!(result(&lab), "0 false None");
    });
}
