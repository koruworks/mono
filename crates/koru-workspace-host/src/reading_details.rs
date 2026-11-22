// SPDX-License-Identifier: MPL-2.0

//! Read-only, user-facing inspection of the text used by search. Formatting
//! happens on the fetching worker, never during a frame.
use koru_api_client::ReadingAnswer;
use koru_i18n::{Key, Lang, fill, t};

#[derive(Debug)]
pub struct ReadingDetails {
    pub name: String,
    pub text: String,
    pub extracted_text: String,
    pub target: Option<(
        koru_store_core::ids::WorkspaceId,
        koru_store_core::model::BlobHash,
    )>,
    pub corrections: koru_store_wire::readings::ReadingCorrections,
    pub snapshot: Vec<u8>,
    pub lines: Vec<String>,
}

pub fn format(name: String, answer: Option<&ReadingAnswer>, lang: Lang) -> ReadingDetails {
    let status = match answer {
        None => Key::ReadingUnread,
        Some(a) => {
            let local = a.local.as_ref();
            if local.is_some_and(|l| l.pending) {
                Key::ReadingQueued
            } else if a.failed_here.is_some() || a.status.as_deref() == Some("failed") {
                Key::ReadingFailed
            } else if local.is_some_and(|l| !l.available_here) {
                Key::ReadingMissing
            } else if local.is_some_and(|l| !l.supported) {
                Key::ReadingUnsupported
            } else if local.is_some_and(|l| l.needs_reader && !l.reader_ready) {
                Key::ReadingNeedsOcr
            } else {
                match a.status.as_deref() {
                    Some("read") => Key::ReadingReady,
                    Some("empty") => Key::ReadingEmpty,
                    Some("partial") => Key::ReadingPartial,
                    _ => Key::ReadingUnread,
                }
            }
        }
    };
    let mut text = t(lang, status).to_owned();
    if let Some(a) = answer {
        let count = a.pages.len();
        let characters: usize = a.pages.iter().map(|p| p.text.chars().count()).sum();
        text.push_str("\n\n");
        text.push_str(&fill(
            t(lang, Key::ReadingCounts),
            &[&count.to_string(), &characters.to_string()],
        ));
        if let Some(done) = a.pages_done {
            text.push('\n');
            text.push_str(&fill(
                t(lang, Key::SettingsReaderPagesRead),
                &[&done.to_string()],
            ));
        }
        let source = match a.source.as_deref() {
            Some("text-layer" | "plain") => Some(Key::ReadingNative),
            Some("recognized") => Some(Key::ReadingRecognized),
            Some("mixed") => Some(Key::ReadingMixed),
            _ => None,
        };
        if let Some(source) = source {
            text.push('\n');
            text.push_str(t(lang, source));
        }
        // Explain the failure without paths or internal exception messages.
        if let Some(reason) = a.failed_here.as_deref().or(a.reason.as_deref()) {
            let reason = t(
                lang,
                match reason {
                    "password" => Key::ReaderReasonPassword,
                    "damaged" => Key::ReaderReasonDamaged,
                    "unsupported" => Key::ReaderReasonUnsupported,
                    "page-cap" => Key::ReadingPartial,
                    "needs-reader" => Key::ReadingNeedsOcr,
                    _ => Key::ReaderReasonHere,
                },
            );
            text.push('\n');
            text.push_str(&fill(t(lang, Key::ReadingReason), &[reason]));
        }
        text.push_str("\n\n");
        text.push_str(t(lang, Key::ReadingViewText));
        for page in &a.pages {
            text.push_str("\n\n");
            text.push_str(&fill(t(lang, Key::ReadingPage), &[&page.page.to_string()]));
            text.push('\n');
            text.push_str(&page.text);
        }
    }
    // Bound each displayed layout on this worker. Copy/save retain `text`
    // exactly, including very long lines; drawing never shapes an entire PDF
    // page worth of text as one line on the UI thread.
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            lines.push(String::new());
        }
        let mut rest = line;
        while !rest.is_empty() {
            let end = rest
                .char_indices()
                .nth(120)
                .map_or(rest.len(), |(at, _)| at);
            lines.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    let extracted_text = answer
        .map(|answer| {
            answer
                .pages
                .iter()
                .map(|page| page.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let corrections = answer.map(|a| a.corrections.clone()).unwrap_or_default();
    let target = answer.and_then(|a| {
        koru_store_core::model::BlobHash::parse(&a.hash)
            .ok()
            .map(|hash| (a.workspace, hash))
    });
    let effective = corrections
        .versions
        .iter()
        .find(|v| Some(&v.id) == corrections.selected.as_ref())
        .and_then(|v| v.text.as_deref())
        .unwrap_or(&extracted_text);
    let snapshot = plain_snapshot(effective).unwrap_or_default();
    ReadingDetails {
        name,
        text,
        lines,
        extracted_text,
        target,
        corrections,
        snapshot,
    }
}

/// **The words a picture carries**, as in-page find, *Copy text* and a
/// screen reader use them: the correction a person selected when there is
/// one, else the pages' words, a blank line between pages (the same text the
/// reading view shows as current). `None` without a reading, or when it holds
/// nothing but whitespace. Built on the fetching worker, never in a frame.
pub fn picture_text(answer: Option<&ReadingAnswer>) -> Option<String> {
    let answer = answer?;
    let corrections = &answer.corrections;
    let corrected = corrections
        .versions
        .iter()
        .find(|v| Some(&v.id) == corrections.selected.as_ref())
        .and_then(|v| v.text.clone());
    let text = corrected.unwrap_or_else(|| {
        answer
            .pages
            .iter()
            .map(|page| page.text.trim_end())
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    });
    (!text.trim().is_empty()).then_some(text)
}

/// Literal paragraphs, built on the reading worker. OCR is never Markdown.
pub fn plain_snapshot(text: &str) -> Result<Vec<u8>, String> {
    use koru_core::{
        Node, WorkspaceId,
        schema::{Parent, WorkspaceDoc},
    };
    let build = || -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "OCR")?;
        let root = doc.insert(Parent::Root, Node::document("OCR")?)?;
        for line in text.split('\n') {
            doc.insert(Parent::Node(root), Node::paragraph(line)?)?;
        }
        Ok(doc.export_snapshot()?)
    };
    build().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
