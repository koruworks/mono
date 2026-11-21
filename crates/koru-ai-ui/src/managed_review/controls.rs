// SPDX-License-Identifier: MPL-2.0
//! Compact measured controls, wrapping shared buttons into rows at narrow widths.
use super::*;
use koru_draw::el::{El, FrameCx, Interaction, Key, Length, col, row};
use questions::elements::{self, ButtonSpec};

#[cfg(test)]
mod tests;

const NAME: &str = "Routine review controls";
pub(super) struct Controls {
    tree: El,
    keys: Vec<Key>,
    busy: bool,
}
impl Controls {
    pub fn new(
        engine: &mut TextEngine,
        id: Uuid,
        labels: &[(&'static str, String, bool)],
        focused: Option<usize>,
        busy: bool,
        width: f32,
    ) -> Self {
        let inset = Space::S3.px(Profile::Desktop);
        let gap = Space::S2.px(Profile::Desktop);
        let available = (width - 2.0 * inset).max(1.0);
        let mut tree = col(NAME).pad(Space::S3).gap(Space::S2);
        let mut line = row("Routine review actions").gap(Space::S2).items_center();
        let mut used = 0.0;
        let mut keys = vec![];
        for (index, (part, label, primary)) in labels.iter().enumerate() {
            let style = if *primary {
                ActionStyle::Primary
            } else {
                ActionStyle::Secondary
            };
            let measured = ActionButton::new(label, style, None)
                .width(engine)
                .min(available);
            if used > 0.0 && used + gap + measured > available {
                tree = tree.child(line);
                line = row("Routine review actions").gap(Space::S2).items_center();
                used = 0.0;
            }
            let spec = ButtonSpec {
                key: Key::model(id.as_u128(), part),
                label: label.clone(),
                width: measured,
                primary: *primary,
                disabled: busy && index != 2,
                focused: focused == Some(index),
            };
            keys.push(spec.key);
            line = line.child(
                elements::button("Routine review action", &spec)
                    .w(Length::Runtime(measured, "measured review action")),
            );
            used += measured + if used > 0.0 { gap } else { 0.0 };
        }
        Self {
            tree: col("Routine review control slot").child(tree.child(line)),
            keys,
            busy,
        }
    }
    pub fn height(&self, width: f32) -> f32 {
        elements::layout(
            &self.tree,
            Rect::new(0.0, 0.0, width, f32::from(u16::MAX)),
            FrameCx::root(),
        )
        .rect_of(NAME)
        .expect("review controls exist")
        .h
    }
    pub fn hit(&self, rect: Rect, x: f32, y: f32) -> Option<usize> {
        let hit = elements::layout(&self.tree, rect, FrameCx::root()).hit_at(x, y)?;
        self.keys
            .iter()
            .position(|key| *key == hit)
            .filter(|index| !self.busy || *index == 2)
    }
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        pointer: (f32, f32),
    ) {
        let hot = elements::layout(&self.tree, rect, FrameCx::root()).hit_at(pointer.0, pointer.1);
        let frame = elements::layout(
            &self.tree,
            rect,
            FrameCx::root().with_interaction(Interaction {
                hot,
                pressed: None,
                focused: None,
            }),
        );
        frame.paint(&self.tree, canvas, root, engine);
    }
}
