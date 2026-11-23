// SPDX-License-Identifier: MPL-2.0

//! **`koru-markdown-view`**: its surface and the document boundary it draws,
//! a record each, written from the source they name. `registry/MANIFEST.md`
//! lists the files read, the modules excluded and why, and what building
//! each surface needs.

use super::*;

const OWNER: &str = "koru-markdown-view";

const DOCUMENT: &str = "crates/koru-markdown-view/src/document.rs";
const VIEW: &str = "crates/koru-markdown-view/src/view.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: new() holds no engine, worker or file; the host
    // hands it a TextEngine to lay out and draw with, and a document parsed
    // on the host's own worker
    record(
        "C-MARKDOWN-VIEW",
        "MarkdownView",
        Key::UiLabMarkdownViewName,
        Key::UiLabMarkdownViewPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_markdown_view", VIEW),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_markdown_view::MarkdownView"),
            painter("koru_markdown_view::MarkdownView::new"),
            painter("koru_markdown_view::MarkdownView::draw"),
            support_with(
                "koru_markdown_view::MarkdownView::set_content",
                &[values(
                    Key::UiLabControlState,
                    "koru_markdown_view::Content",
                    VIEW,
                    Declared::Enum,
                    &[
                        labelled("Reading", Key::UiLabOptionReading),
                        labelled("Document", Key::UiLabOptionMarkdownDocument),
                        labelled("NotMarkdown", Key::UiLabOptionNotMarkdown),
                        labelled("Elsewhere", Key::UiLabOptionElsewhere),
                        labelled("Unreadable", Key::UiLabOptionUnreadable),
                    ],
                )],
            ),
            support("koru_markdown_view::MarkdownView::relayout"),
            support("koru_markdown_view::MarkdownView::height"),
            support("koru_markdown_view::MarkdownView::lang"),
            support("koru_markdown_view::MarkdownView::set_lang"),
            support("koru_markdown_view::MarkdownView::content"),
            support("koru_markdown_view::MarkdownView::document"),
            support("koru_markdown_view::MarkdownView::is_document"),
            support("koru_markdown_view::MarkdownView::remaining"),
            support("koru_markdown_view::MarkdownView::reveal_more"),
            support("koru_markdown_view::MarkdownView::is_editing"),
            support("koru_markdown_view::MarkdownView::begin_edit"),
            support("koru_markdown_view::MarkdownView::cancel_edit"),
            support("koru_markdown_view::MarkdownView::edited"),
            support("koru_markdown_view::MarkdownView::changed"),
            support("koru_markdown_view::MarkdownView::accept_saved"),
            support("koru_markdown_view::MarkdownView::mouse_down"),
            support("koru_markdown_view::MarkdownView::mouse_up"),
            support("koru_markdown_view::MarkdownView::key"),
            support("koru_markdown_view::MarkdownView::work"),
            support("koru_markdown_view::MarkdownView::lines"),
            support("koru_markdown_view::Content"),
            support("koru_markdown_view::Work"),
            support("koru_markdown_view::BLOCKS_SHOWN"),
            support("koru_markdown_view::text::reading"),
            support("koru_markdown_view::text::empty_document"),
            support("koru_markdown_view::text::show_rest"),
            support("koru_markdown_view::text::as_koru_reads_it"),
            support("koru_markdown_view::text::editing"),
            support("koru_markdown_view::text::not_markdown"),
            support("koru_markdown_view::text::unreadable"),
            support("koru_markdown_view::text::held_elsewhere"),
            support("koru_markdown_view::text::type_label"),
            support("koru_markdown_view::text::size_label"),
            support("koru_markdown_view::text::file_label"),
            support("koru_markdown_view::text::size"),
        ],
    ),
    // Construction: pure: of(text) imports and exports in memory, in a
    // workspace nobody owns, with a blob source that holds nothing; the cost
    // is proportional to the file, so a host calls it on its own worker
    record(
        "C-MARKDOWN-DOCUMENT",
        "MarkdownDocument",
        Key::UiLabMarkdownDocumentName,
        Key::UiLabMarkdownDocumentPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_markdown_view", DOCUMENT),
        DESKTOP_NOT_ON_PHONES,
        &[
            boundary("koru_markdown_view::MarkdownDocument"),
            boundary("koru_markdown_view::MarkdownDocument::of"),
            boundary("koru_markdown_view::MarkdownDocument::of_document"),
            boundary("koru_markdown_view::MarkdownDocument::snapshot"),
            boundary("koru_markdown_view::MarkdownDocument::markdown"),
            boundary("koru_markdown_view::MarkdownDocument::blocks"),
            boundary("koru_markdown_view::MarkdownDocument::is_empty"),
            boundary("koru_markdown_view::MarkdownDocument::is_faithful"),
            boundary("koru_markdown_view::NotADocument"),
            boundary("koru_markdown_view::is_comment"),
        ],
    ),
];
