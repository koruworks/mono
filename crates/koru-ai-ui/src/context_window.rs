// SPDX-License-Identifier: MPL-2.0

//! The composer model mark's context report. A current-window percentage is
//! shown only when the agent reports both used and available tokens.

use crate::{ChatSurface, usage};
use koru_i18n::{Key, Lang, fill, t};

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

    fn counts(&self) -> String {
        match (self.used, self.size) {
            (Some(used), Some(size)) if size > 0 => {
                format!("{}/{}", precise_count(used), precise_count(size))
            }
            _ => "—".into(),
        }
    }
}

fn precise_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

impl Snapshot {
    pub(crate) fn report(&self, lang: Lang) -> koru_agent_config_ui::picker::report::Report {
        let mut details = Vec::new();
        if let Some(size) = self.size {
            details.push(fill(
                t(lang, Key::AiContextWindowLimit),
                &[&usage::compact(size)],
            ));
        }
        if let Some(input) = self.last_input {
            details.push(fill(
                t(lang, Key::AiContextWindowLastInput),
                &[&usage::compact(input)],
            ));
        }
        if let Some(tokens) = self.total_processed {
            details.push(fill(
                t(lang, Key::AiContextWindowTotal),
                &[&usage::compact(tokens)],
            ));
        }
        details.push(fill(
            t(lang, Key::AiContextWindowAttached),
            &[&self.attached.to_string()],
        ));
        koru_agent_config_ui::picker::report::Report {
            heading: t(lang, Key::AiContextWindowTitle).to_owned(),
            counts: self.counts(),
            summary: self.summary(lang),
            fraction: self.fraction(),
            details,
        }
    }
}

impl ChatSurface {
    pub(crate) fn reported_context_usage(&self) -> Option<(u64, u64)> {
        let transcript = self.transcript.as_ref()?;
        if let Some(&(used, size)) = self.context_window_usage.get(&transcript.chat.id)
            && size > 0
        {
            return Some((used, size));
        }
        // A running attempt cannot inherit the previous reply's context.
        if self.attempt.is_some() {
            return None;
        }
        let last_reply = transcript
            .messages
            .iter()
            .rev()
            .find(|message| message.role == koru_ai_core::ChatRole::Assistant)?;
        if last_reply.provider.as_ref() != Some(&transcript.chat.current_model.provider)
            || last_reply.requested_model.as_deref()
                != Some(transcript.chat.current_model.requested_model.as_str())
        {
            return None;
        }
        transcript
            .usage
            .iter()
            .find(|entry| entry.message_id == last_reply.id)?
            .context
            .as_ref()
            .filter(|context| context.size > 0)
            .map(|context| (context.used, context.size))
    }

    pub(crate) fn context_window_snapshot(&self) -> Option<Snapshot> {
        let transcript = self.transcript.as_ref()?;
        let chat = transcript.chat.id;
        let (used, size) = self
            .reported_context_usage()
            .map_or((None, None), |(used, size)| (Some(used), Some(size)));
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

    #[test]
    fn the_reported_counts_and_progress_fill_the_context_header() {
        let snapshot = Snapshot {
            used: Some(173_700),
            size: Some(258_400),
            last_input: Some(48_000),
            total_processed: Some(365_000),
            attached: 3,
            model: "6.1 Terra".into(),
        };
        assert_eq!(snapshot.counts(), "173.7k/258.4k");
        let report = snapshot.report(Lang::En);
        let tree = report.tree();
        let height = report.height(330.0);
        let frame = crate::model_picker::layout_elements(
            &tree,
            koru_draw::geometry::Rect::new(0.0, 0.0, 330.0, height),
            koru_draw::el::FrameCx::root(),
        );
        let bar = frame.rect_of("Context progress").expect("progress bar");
        assert!(bar.w >= 305.0);
        assert_eq!(bar.h, 6.0);
        assert_eq!(report.details.len(), 4);
        assert!(height > bar.h);
    }
}
