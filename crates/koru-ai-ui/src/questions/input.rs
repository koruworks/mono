// SPDX-License-Identifier: MPL-2.0
//! Question form interaction and rendering, independent of the turn owner.
use super::*;

pub(crate) enum Effect {
    Handled(bool),
    Leave,
    Activate(Control),
}

pub(crate) struct Actions<'a> {
    pub id: uuid::Uuid,
    pub heading: &'a str,
    pub body: &'a str,
    pub labels: &'a [(String, bool)],
    pub focused: Option<usize>,
    pub disabled: bool,
}

impl Actions<'_> {
    fn tree(&self, engine: &mut TextEngine) -> koru_draw::el::El {
        let buttons = self
            .labels
            .iter()
            .enumerate()
            .map(|(index, (label, primary))| elements::ButtonSpec {
                key: control_key(self.id, Control::Option(0, index)),
                label: label.clone(),
                width: ActionButton::new(
                    label,
                    if *primary {
                        ActionStyle::Primary
                    } else {
                        ActionStyle::Secondary
                    },
                    None,
                )
                .width(engine),
                primary: *primary,
                disabled: self.disabled,
                focused: self.focused == Some(index),
            })
            .collect::<Vec<_>>();
        elements::actions_tree(self.heading, self.body, &buttons)
    }
    pub fn height(&self, engine: &mut TextEngine, width: f32) -> f32 {
        elements::height(&self.tree(engine), width)
    }
    pub fn hit(&self, engine: &mut TextEngine, row: Rect, x: f32, y: f32) -> Option<usize> {
        if self.disabled {
            return None;
        }
        let tree = self.tree(engine);
        let hit = elements::layout(&tree, row, FrameCx::root()).hit_at(x, y)?;
        (0..self.labels.len()).find(|index| control_key(self.id, Control::Option(0, *index)) == hit)
    }
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        row: Rect,
        pointer: (f32, f32),
    ) {
        let tree = self.tree(engine);
        let hot = elements::layout(&tree, row, FrameCx::root()).hit_at(pointer.0, pointer.1);
        let frame = elements::layout(
            &tree,
            row,
            FrameCx::root().with_interaction(Interaction {
                hot,
                pressed: None,
                focused: self
                    .focused
                    .map(|index| control_key(self.id, Control::Option(0, index))),
            }),
        );
        frame.paint(&tree, canvas, root, engine);
    }
}

impl Card {
    pub(crate) fn choose(&mut self, control: Control) -> bool {
        if self.sent {
            return false;
        }
        self.cursor = control;
        if let Control::Option(question, option) = control {
            if let Some(pick) = self.picks.get_mut(question) {
                *pick = Some(Pick::Option(option));
            }
        }
        true
    }

    pub(crate) fn key(
        &mut self,
        engine: &mut TextEngine,
        questions: &[AgentQuestion],
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Effect {
        let order = controls(questions);
        let cursor = self.cursor;
        let step = |forward: bool| {
            let at = order
                .iter()
                .position(|control| *control == cursor)
                .unwrap_or(0);
            order[if forward {
                (at + 1) % order.len()
            } else {
                (at + order.len() - 1) % order.len()
            }]
        };
        match key {
            WinitKey::Named(NamedKey::Escape) => return Effect::Leave,
            WinitKey::Named(NamedKey::Tab) if !koru_keys::is_chord(mods) => {
                self.cursor = step(!mods.shift_key());
                return Effect::Handled(true);
            }
            _ => {}
        }
        if self.sent {
            return Effect::Handled(false);
        }
        if let Control::Field(question) = cursor {
            if matches!(key, WinitKey::Named(NamedKey::Enter)) {
                if self.answers(questions).is_some() {
                    return Effect::Activate(Control::Submit);
                }
                self.cursor = step(true);
                return Effect::Handled(true);
            }
            let Some(input) = self.inputs.get(question).and_then(Clone::clone) else {
                return Effect::Leave;
            };
            let typed = input.0.borrow_mut().key(engine, key, text, mods);
            if !questions[question].options.is_empty() {
                self.picks[question] = if input.value().trim().is_empty() {
                    None
                } else {
                    Some(Pick::Words)
                };
            }
            return Effect::Handled(typed == Typed::Handled);
        }
        let activates = match key {
            WinitKey::Named(NamedKey::Enter | NamedKey::Space) => true,
            WinitKey::Character(value) => value.as_str() == " ",
            _ => false,
        };
        if activates && mods.is_empty() {
            return Effect::Activate(cursor);
        }
        if mods.is_empty()
            && let Control::Option(..) = cursor
            && let WinitKey::Named(direction @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) = key
        {
            self.cursor = step(*direction == NamedKey::ArrowDown);
            return Effect::Handled(true);
        }
        if text.is_some_and(|text| !text.trim().is_empty()) && !koru_keys::is_chord(mods) {
            return Effect::Leave;
        }
        Effect::Handled(false)
    }
}

pub(crate) fn height(
    engine: &mut TextEngine,
    lang: Lang,
    id: uuid::Uuid,
    questions: &[AgentQuestion],
    card: &Card,
    width: f32,
) -> f32 {
    let spec = spec(engine, lang, id, questions, card, None);
    elements::height(&elements::tree(&spec), width)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    lang: Lang,
    id: uuid::Uuid,
    questions: &[AgentQuestion],
    card: &Card,
    focused: bool,
    pointer: (f32, f32),
    row: Rect,
) {
    let focus = focused.then_some(card.cursor);
    let tree = elements::tree(&spec(engine, lang, id, questions, card, focus));
    let hot = elements::layout(&tree, row, FrameCx::root()).hit_at(pointer.0, pointer.1);
    let frame = elements::layout(
        &tree,
        row,
        FrameCx::root().with_interaction(Interaction {
            hot,
            pressed: None,
            focused: focus.map(|control| control_key(id, control)),
        }),
    );
    frame.paint(&tree, canvas, root, engine);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn press(
    engine: &mut TextEngine,
    lang: Lang,
    id: uuid::Uuid,
    questions: &[AgentQuestion],
    card: &mut Card,
    row: Rect,
    x: f32,
    y: f32,
) -> Option<Control> {
    let tree = elements::tree(&spec(engine, lang, id, questions, card, None));
    let frame = elements::layout(&tree, row, FrameCx::root());
    let hit = frame.hit_at(x, y)?;
    let control = controls(questions)
        .into_iter()
        .find(|control| control_key(id, *control) == hit)?;
    if card.sent {
        return None;
    }
    card.cursor = control;
    if let Control::Field(index) = control {
        let rect = frame.node(control_key(id, control))?.rect;
        if let Some(input) = card.inputs.get(index).and_then(Clone::clone) {
            input.0.borrow_mut().click(engine, rect, x, y);
        }
    }
    Some(control)
}

#[cfg(test)]
mod tests;
