// SPDX-License-Identifier: MPL-2.0

//! A document chip, once it is a message: `[[koru://doc/<id>]]` drawn as a
//! document icon and title link.
//!
//! The composer sends a chip in the Markdown exporter's own spelling
//! (`koru_export_markdown::message_text`), so the agent, the importer and this
//! renderer read one token. What a person reads in the conversation is the
//! icon and title, in the link color, and a click opens the document — never
//! the raw token.
//!
//! **Titles are looked up, never fetched here.** They come from what the
//! surface already knows: the chip's own caption when this window sent it, the
//! chat's pinned documents, and answers the reader thread brings back for any
//! id still unknown (`Command::DocumentTitles`). Until one arrives the link
//! reads as the fallback word, and the layout is rebuilt when the title lands,
//! because the displayed text is part of what the layout cache compares.

use std::collections::HashMap;

use koru_ai_core::DocumentId;
use koru_markdown::LinkSpan;

/// **`markdown` with every mentioned Bot read as `@` and its name**, the
/// rest untouched: the first pass over a message of the person's, before
/// its document chips are titled.
///
/// The name is the one the host knows the Bot by now (`names`, by id), so a
/// rename shows; for a Bot the host no longer names (archived, in another
/// workspace, on another computer) it is the name the link carried; and for
/// a link that carried none it is `unnamed`. **Never the id.** A mention is
/// words, not a link: there is nothing to open.
pub(crate) fn mentions_named(
    markdown: &str,
    names: &HashMap<uuid::Uuid, String>,
    unnamed: &str,
) -> String {
    let found = koru_link::wrapped(markdown);
    if !found
        .iter()
        .any(|token| matches!(token.link, koru_link::KoruLink::Bot { .. }))
    {
        return markdown.to_owned();
    }
    let mut text = String::with_capacity(markdown.len());
    let mut at = 0;
    for token in found {
        let koru_link::KoruLink::Bot { bot, name } = token.link else {
            continue;
        };
        text.push_str(&markdown[at..token.range.start]);
        let name = names
            .get(&bot)
            .map(String::as_str)
            .map(str::trim)
            .filter(|known| !known.is_empty())
            .or(Some(name.trim()).filter(|carried| !carried.is_empty()))
            .unwrap_or(unnamed);
        text.push('@');
        text.push_str(name);
        at = token.range.end;
    }
    text.push_str(&markdown[at..]);
    text
}

/// Nonbreaking space reserves an icon's width in a sent message's text.
pub(crate) const CHIP_LEAD: &str = "\u{00a0}\u{00a0}\u{00a0}\u{00a0}";

/// Every document a message's chips name, in order, each once.
pub(crate) fn documents(markdown: &str) -> Vec<DocumentId> {
    let mut out: Vec<DocumentId> = Vec::new();
    for found in koru_link::wrapped(markdown) {
        if let koru_link::KoruLink::Document { document, .. } = found.link {
            let id = DocumentId::from_uuid(document);
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// `markdown` with every document token replaced by its title, and where
/// each title now sits. The link's URL is the token's own `koru://doc/…`, so
/// a heading chip still opens at its heading.
pub(crate) fn titled(
    markdown: &str,
    titles: &HashMap<DocumentId, String>,
    fallback: &str,
) -> (String, Vec<LinkSpan>) {
    titled_with_lead(markdown, titles, fallback, "")
}

/// A sent message keeps a document icon beside every titled link. The lead
/// occupies real layout width so wrapping and hit testing agree with drawing.
pub(crate) fn titled_for_message(
    markdown: &str,
    titles: &HashMap<DocumentId, String>,
    fallback: &str,
) -> (String, Vec<LinkSpan>) {
    titled_with_lead(markdown, titles, fallback, CHIP_LEAD)
}

fn titled_with_lead(
    markdown: &str,
    titles: &HashMap<DocumentId, String>,
    fallback: &str,
    lead: &str,
) -> (String, Vec<LinkSpan>) {
    let found = koru_link::wrapped(markdown);
    if found.is_empty() {
        return (markdown.to_owned(), Vec::new());
    }
    let mut text = String::with_capacity(markdown.len());
    let mut links = Vec::new();
    let mut at = 0;
    for token in found {
        let koru_link::KoruLink::Document { document, node } = token.link else {
            continue;
        };
        text.push_str(&markdown[at..token.range.start]);
        let title = titles
            .get(&DocumentId::from_uuid(document))
            .map(String::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(fallback);
        let start = text.len();
        text.push_str(lead);
        text.push_str(title);
        links.push(LinkSpan {
            range: start..text.len(),
            url: koru_link::document_link(document, node),
        });
        at = token.range.end;
    }
    text.push_str(&markdown[at..]);
    (text, links)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chip_reads_as_its_title_and_links_to_its_document() {
        let document = DocumentId::new();
        let markdown = format!("Summarise [[koru://doc/{document}]] please");
        let titles = HashMap::from([(document, "Budget 2026".to_owned())]);

        let (text, links) = titled(&markdown, &titles, "Document");

        assert_eq!(text, "Summarise Budget 2026 please");
        assert_eq!(&text[links[0].range.clone()], "Budget 2026");
        assert_eq!(links[0].url, format!("koru://doc/{document}"));
        assert_eq!(documents(&markdown), vec![document]);
    }

    #[test]
    fn an_unknown_title_reads_as_the_fallback_until_it_arrives() {
        let document = DocumentId::new();
        let markdown = format!("[[koru://doc/{document}]]");
        let (text, links) = titled(&markdown, &HashMap::new(), "Document");
        assert_eq!(text, "Document");
        assert_eq!(links[0].range, 0..8);
    }

    #[test]
    fn plain_text_and_other_brackets_are_left_alone() {
        let (text, links) = titled("a [[b]] c", &HashMap::new(), "Document");
        assert_eq!(text, "a [[b]] c");
        assert!(links.is_empty());
    }

    /// A mentioned Bot reads as its current name; one the host no longer
    /// names reads as the name its link carried; one with no name reads as
    /// the fallback word. Never the id, and never a link.
    #[test]
    fn bot_mention_in_a_sent_message_reads_as_a_name_never_an_id() {
        let known: uuid::Uuid = "0b8f6f0e-3c5e-4d0a-9a51-7d5b7c1f2a11".parse().unwrap();
        let gone: uuid::Uuid = "7c1d2e3f-4a5b-4c6d-8e7f-901a2b3c4d5e".parse().unwrap();
        let bare: uuid::Uuid = "5d6e7f80-1a2b-4c3d-9e4f-a1b2c3d4e5f6".parse().unwrap();
        let document = DocumentId::new();
        let markdown = format!(
            "Ask [[{}]] and [[{}]] and [[koru://bot/{bare}]] about [[koru://doc/{document}]]",
            koru_link::bot_link(known, "CFO"),
            koru_link::bot_link(gone, "Old Legal"),
        );
        // The host renamed the first since it was sent.
        let names = HashMap::from([(known, "Finance".to_owned())]);

        let read = mentions_named(&markdown, &names, "Bot");
        assert_eq!(
            read,
            format!("Ask @Finance and @Old Legal and @Bot about [[koru://doc/{document}]]")
        );
        for id in [known, gone, bare] {
            assert!(!read.contains(&id.to_string()));
        }
        assert!(!read.contains("koru://bot"));

        // Then the documents are titled as they always were, and the
        // mention is words: the only link is the document's.
        let titles = HashMap::from([(document, "Budget".to_owned())]);
        let (text, links) = titled(&read, &titles, "Document");
        assert_eq!(text, "Ask @Finance and @Old Legal and @Bot about Budget");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, format!("koru://doc/{document}"));

        // A message that mentions nobody is returned as it was.
        let plain = format!("see [[koru://doc/{document}]] and [[not a link]]");
        assert_eq!(mentions_named(&plain, &names, "Bot"), plain);
    }

    #[test]
    fn a_sent_chip_reserves_an_icon_and_keeps_the_document_link() {
        let document = DocumentId::new();
        let markdown = format!("[[koru://doc/{document}]] review");
        let titles = HashMap::from([(document, "Top Priorities".to_owned())]);
        let (text, links) = titled_for_message(&markdown, &titles, "Document");
        assert_eq!(text, format!("{CHIP_LEAD}Top Priorities review"));
        assert_eq!(
            &text[links[0].range.clone()],
            format!("{CHIP_LEAD}Top Priorities")
        );
        assert_eq!(links[0].url, format!("koru://doc/{document}"));
    }
}
