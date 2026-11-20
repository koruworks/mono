// SPDX-License-Identifier: MPL-2.0

//! The same avatar/name pill used in a draft, sized for a transcript hint.
use koru_draw::{
    el::{ElState, FrameCx, Key, Leaf},
    geometry::Rect,
    pill::{PillKind, PillTextStyle},
    text::TextEngine,
    theme,
};
use vello::{Scene, kurbo::Affine};

pub(super) struct BotChip {
    face: u8,
    name: String,
    size: (f32, f32),
    hover_parent: Option<Key>,
}

impl BotChip {
    pub fn bounded(engine: &mut TextEngine, face: u8, name: &str, width: f32) -> Self {
        let mut shown = name.to_owned();
        loop {
            let chip = Self::new(engine, face, &shown);
            if chip.size.0 <= width || shown.chars().count() <= 2 {
                return chip;
            }
            let keep = shown.chars().count() / 2;
            shown = shown.chars().take(keep).collect();
            shown.push('…');
        }
    }

    pub fn new(engine: &mut TextEngine, face: u8, name: &str) -> Self {
        let kind = PillKind::Bot(face);
        engine.measure_pill_with_style(kind, name, None, Self::style());
        let visual = engine
            .pills
            .get_with_style(kind, name, None, Self::style())
            .expect("measured pill");
        let size = (visual.width, visual.height);
        Self {
            face,
            name: name.into(),
            size,
            hover_parent: None,
        }
    }
    pub fn with_hover_parent(mut self, key: Key) -> Self {
        self.hover_parent = Some(key);
        self
    }
    fn style() -> PillTextStyle {
        let metrics = theme::role::meta().metrics;
        PillTextStyle::new(metrics.size, metrics.line_height)
    }
}

impl Leaf for BotChip {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        self.size
    }
    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let kind = PillKind::Bot(self.face);
        engine.measure_pill_with_style(kind, &self.name, None, Self::style());
        if let Some(visual) = engine
            .pills
            .get_with_style(kind, &self.name, None, Self::style())
        {
            koru_draw::pill::draw(
                scene,
                root,
                kind,
                false,
                false,
                visual,
                at.x,
                at.y,
                false,
                state.hovered
                    || state.focused
                    || self.hover_parent.is_some_and(|key| {
                        cx.interaction.hot == Some(key) || cx.interaction.focused == Some(key)
                    }),
                false,
            );
        }
    }
}
