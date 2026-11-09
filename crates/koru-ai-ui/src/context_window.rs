// SPDX-License-Identifier: MPL-2.0

//! The composer model mark's context report. A current-window percentage is
//! shown only when the agent reports both used and available tokens.

use std::time::Instant;

use koru_draw::el::{El, ElState, FrameCx, Leaf, col, leaf, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Metric, Space, roles};
use koru_i18n::{Key, Lang, fill, t};
use koru_ui::popover::{PopoverAlign, PopoverShell};
use koru_ui::progress_bar::{ProgressBar, ProgressMode};
use vello::Scene;
use vello::kurbo::Affine;

use crate::{ChatSurface, usage};

const PANEL_W: Metric = Metric::new("CONTEXT_WINDOW_W", 330.0);
const PANEL_H: Metric = Metric::new("CONTEXT_WINDOW_H", 174.0);
const BAR_H: Metric = Metric::new("CONTEXT_WINDOW_BAR_H", 4.0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub used: Option<u64>,
    pub size: Option<u64>,
    pub last_input: Option<u64>,
    pub total_processed: Option<u64>,
    pub attached: usize,
    pub model: String,
}

impl Snapshot {
    pub fn fraction(&self) -> Option<f32> {
        match (self.used, self.size) {
            (Some(used), Some(size)) if size > 0 => {
                Some((used as f32 / size as f32).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    fn summary(&self, lang: Lang) -> String {
        match (self.used, self.size) {
            (Some(used), Some(size)) if size > 0 => format!(
                "{} · {}",
                fill(
                    t(lang, Key::AiContextWindowUsed),
                    &[
                        &(used.saturating_mul(100) / size).min(100).to_string(),
                        &usage::compact(used),
                        &usage::compact(size),
                    ],
                ),
                self.model
            ),
            _ => format!("{} · {}", t(lang, Key::AiContextWindowUnknown), self.model),
        }
    }
}

struct Bar(Option<f32>);

impl Leaf for Bar {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, BAR_H.value)
    }

    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        ProgressBar::new(
            ProgressMode::Determinate(self.0.unwrap_or(0.0)),
            Instant::now(),
        )
        .draw(canvas, root, at, Instant::now(), 1.0);
    }
}

fn tree(snapshot: &Snapshot, lang: Lang) -> El {
    let mut panel = col("ContextWindow")
        .pad_x(Space::S3)
        .pad_top(Space::S3)
        .pad_bottom(Space::S3)
        .gap(Space::S2)
        .child(
            text(t(lang, Key::AiContextWindowTitle))
                .named("Context heading")
                .role(roles::META)
                .ink(Ink::Secondary),
        )
        .child(
            text(snapshot.summary(lang))
                .named("Context usage")
                .role(roles::ROW)
                .ink(Ink::Primary)
                .truncate(),
        )
        .child(
            leaf("Context progress", Bar(snapshot.fraction()))
                .decorative()
                .h(BAR_H),
        );
    if let Some(size) = snapshot.size {
        panel = panel.child(
            text(fill(
                t(lang, Key::AiContextWindowLimit),
                &[&usage::compact(size)],
            ))
            .named("Context limit")
            .role(roles::META)
            .ink(Ink::Secondary),
        );
    }
    if let Some(input) = snapshot.last_input {
        panel = panel.child(
            text(fill(
                t(lang, Key::AiContextWindowLastInput),
                &[&usage::compact(input)],
            ))
            .named("Last input")
            .role(roles::META)
            .ink(Ink::Secondary),
        );
    }
    if let Some(tokens) = snapshot.total_processed {
        panel = panel.child(
            text(fill(
                t(lang, Key::AiContextWindowTotal),
                &[&usage::compact(tokens)],
            ))
            .named("Total processed")
            .role(roles::META)
            .ink(Ink::Secondary),
        );
    }
    panel.child(
        text(fill(
            t(lang, Key::AiContextWindowAttached),
            &[&snapshot.attached.to_string()],
        ))
        .named("Attached items")
        .role(roles::META)
        .ink(Ink::Secondary),
    )
}

impl ChatSurface {
    pub(crate) fn context_window_snapshot(&self) -> Option<Snapshot> {
        let transcript = self.transcript.as_ref()?;
        let chat = transcript.chat.id;
        let (used, size) = self
            .context_window_usage
            .filter(|(id, _, size)| *id == chat && *size > 0)
            .map_or((None, None), |(_, used, size)| (Some(used), Some(size)));
        let last_input = transcript
            .usage
            .iter()
            .rev()
            .find_map(|entry| entry.usage.prompt_tokens);
        let total_processed = transcript
            .usage
            .iter()
            .filter_map(|entry| entry.usage.total_tokens)
            .fold(None, |sum: Option<u64>, tokens| {
                Some(sum.unwrap_or(0).saturating_add(tokens))
            });
        let attached = self.strips.get(chat).len() + usize::from(self.context_document.is_some());
        Some(Snapshot {
            used,
            size,
            last_input,
            total_processed,
            attached,
            model: self.chip_label(),
        })
    }

    pub(crate) fn context_window_panel(&self) -> Option<PopoverShell> {
        if !self.context_window_open {
            return None;
        }
        Some(PopoverShell::anchored(
            self.context_indicator_button()?,
            self.viewport,
            PANEL_W.value,
            PANEL_H.value,
            PopoverAlign::End,
        ))
    }

    pub(crate) fn draw_context_window(&mut self, canvas: &mut Scene, root: Affine) {
        let (Some(shell), Some(snapshot)) =
            (self.context_window_panel(), self.context_window_snapshot())
        else {
            return;
        };
        let tree = tree(&snapshot, self.lang);
        let frame = crate::model_picker::layout_elements(&tree, shell.rect, FrameCx::root());
        shell.draw_containing(canvas, root, |canvas, root| {
            frame.paint(&tree, canvas, root, &mut self.engine);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_fraction_requires_a_reported_used_count_and_limit() {
        let mut snapshot = Snapshot {
            used: None,
            size: None,
            last_input: Some(48_000),
            total_processed: Some(365_000),
            attached: 3,
            model: "6.1 Terra".into(),
        };
        assert_eq!(snapshot.fraction(), None);
        snapshot.used = Some(48_000);
        snapshot.size = Some(475_000);
        assert!((snapshot.fraction().unwrap() - 0.101).abs() < 0.001);
        assert!(snapshot.summary(Lang::En).contains("10%"));
    }
}
