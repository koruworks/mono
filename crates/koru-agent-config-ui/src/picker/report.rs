// SPDX-License-Identifier: MPL-2.0

//! Optional read-only usage report below the model controls.
use koru_draw::el::{El, ElState, FrameCx, Leaf, col, leaf, row, spacer, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Line, Metric, Space, roles};
use koru_ui::progress_bar::{ProgressBar, ProgressMode};
use vello::{Scene, kurbo::Affine};

const BAR_H: Metric = Metric::new("MODEL_USAGE_BAR_H", 6.0);

#[derive(Clone)]
pub struct Report {
    pub heading: String,
    pub counts: String,
    pub summary: String,
    pub fraction: Option<f32>,
    pub details: Vec<String>,
}

struct Bar(Option<f32>);
impl Leaf for Bar {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (0.0, BAR_H.value)
    }
    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _: &mut TextEngine,
        at: Rect,
        _: &FrameCx,
        _: ElState,
    ) {
        let now = std::time::Instant::now();
        ProgressBar::new(ProgressMode::Determinate(self.0.unwrap_or(0.0)), now)
            .draw(scene, root, at, now, 1.0);
    }
}

impl Report {
    pub fn tree(&self) -> El {
        let content = col("Model usage content")
            .pad(Space::S3)
            .gap(Space::S2)
            .child(
                row("Context summary")
                    .child(text(&self.heading).role(roles::ROW))
                    .child(spacer())
                    .child(text(&self.counts).role(roles::META).ink(Ink::Secondary)),
            )
            .child(
                leaf("Context progress", Bar(self.fraction))
                    .decorative()
                    .h(BAR_H),
            )
            .child(
                text(&self.summary)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .wrap(),
            )
            .children(
                self.details
                    .iter()
                    .map(|line| text(line).role(roles::META).ink(Ink::Secondary).wrap()),
            );
        col("Model usage report")
            .child(koru_draw::el::divider(Line::Divider))
            .child(content)
    }

    pub fn height(&self, width: f32) -> f32 {
        let tree = col("Usage measurement").child(self.tree());
        super::elements::layout(
            &tree,
            Rect::new(0.0, 0.0, width, u16::MAX as f32),
            FrameCx::root(),
        )
        .rect_of("Model usage report")
        .expect("report exists")
        .h
    }
}
