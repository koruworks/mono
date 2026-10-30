// SPDX-License-Identifier: MPL-2.0

//! The seam, asserted: a result tab under test reaches nothing, it knows
//! which version it holds so the window can find it again, and — the part
//! this file grew for — **the document below the chrome is Koru's document
//! view, and there is exactly one of them at a time** (ADR-372).

use super::*;
use koru_artifacts_ui::{Content, MarkdownDocument, TabRow, VersionPlace};
use koru_editor::Editing;

const FINDINGS: &str = "# Quarterly findings\n\n\
Revenue held. Two things to watch:\n\n\
- the second region\n\
- the renewal in March\n";

fn a_result(version: u128) -> OpenedArtifact {
    OpenedArtifact {
        version: koru_delegate_core::ArtifactVersionId::from_uuid(uuid::Uuid::from_u128(version)),
        title: "The quarterly summary".into(),
        size_bytes: 4_096,
        produced_by: None,
        held_elsewhere: None,
    }
}

fn a_place() -> VersionPlace {
    VersionPlace {
        family: koru_artifacts_ui::ArtifactFamilyId::from_uuid(uuid::Uuid::from_u128(0x11)),
        workspace: koru_artifacts_ui::WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x22)),
        ordinal: 1,
    }
}

/// A view that has read a Markdown version and laid itself out. Shared with
/// the container's tests, which put one in a tab.
pub(crate) fn reading() -> ResultView {
    let mut view = ResultView::new();
    View::set_viewport(&mut view, 1_200.0, 800.0);
    let opened = a_result(0x90);
    let version = opened.version;
    view.open(opened);
    view.take_content(
        version,
        Some(a_place()),
        Content::Document(Box::new(
            MarkdownDocument::of(FINDINGS).expect("a document"),
        )),
    );
    view
}

#[test]
fn a_result_tab_with_no_worker_holds_nothing_and_reaches_nothing() {
    // Under test the link is inert, which is the whole point: this binary's
    // suite must not reach whatever gateway the machine has running.
    let mut view = ResultView::new();
    assert_eq!(view.version(), None);
    assert!(!view.poll());
}

#[test]
fn opening_a_result_names_the_version_and_still_reaches_nothing() {
    let mut view = ResultView::new();
    view.open(a_result(0x90));
    assert_eq!(
        view.version(),
        Some(koru_delegate_core::ArtifactVersionId::from_uuid(
            uuid::Uuid::from_u128(0x90)
        ))
    );
    // No worker answered, so there is nothing to show but the fact that it
    // is being read — a tab that had invented a document would be lying
    // about somebody's work.
    assert!(matches!(
        view.surface_mut().view().content(),
        koru_artifacts_ui::Content::Reading
    ));
    assert!(view.document().is_none());
}

/// **The finding.** A Markdown result opens as a document, and the tab that
/// used to draw a text dump of it draws only the chrome around it.
#[test]
fn a_markdown_result_opens_in_the_document_view() {
    let mut view = reading();
    let page = view.document().expect("a page");
    assert!(page.leaf().view().read_only);
    let rows: Vec<String> = page
        .leaf()
        .body()
        .rows
        .iter()
        .map(|row| row.text.clone())
        .collect();
    assert!(
        rows.iter().any(|row| row == "the second region"),
        "the bullet's own words, with no marker in them: {rows:?}"
    );

    let tab_rows = view.surface_mut().rows();
    assert_eq!(tab_rows[0], TabRow::Title);
    assert_eq!(tab_rows[1], TabRow::Meta);
    assert!(tab_rows.contains(&TabRow::Edit), "{tab_rows:?}");
    assert!(
        !tab_rows.contains(&TabRow::Document),
        "the tab draws no document of its own: {tab_rows:?}"
    );
    // Nothing about the version's bytes is laid out by the tab any more.
    assert_eq!(view.surface_mut().view().work().blocks, 0);
}

/// **The page sits under the chrome, in the same column.** A press below the
/// band reaches the document; a press on the band reaches the control.
#[test]
fn the_chrome_ends_where_the_page_begins() {
    let mut view = reading();
    let band = view.surface_mut().document_top();
    let last = view
        .surface_mut()
        .laid_rows()
        .last()
        .copied()
        .expect("a chrome row");
    assert!(band >= last.1.y + last.1.h, "{band} vs {:?}", last.1);
    let (column_x, column_w) = ResultDocument::text_column(1_200.0);
    assert_eq!(last.1.x, column_x);
    assert_eq!(last.1.w, column_w);
}

/// A workspace this view must never read. Every `View` method is handed the
/// window's own document; a result is not one, so it goes in and is ignored.
fn unread() -> koru_core::schema::WorkspaceDoc {
    koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "unread by this view")
        .expect("a document")
}

/// **The finding, answered** (ADR-378). *Edit* does not swap the page for a
/// box of asterisks and pipes: it opens the same page for typing, and *Stop
/// editing* closes it again. The chrome swaps its controls; the document
/// below stays a document throughout.
#[test]
fn editing_opens_the_same_page_for_typing_and_stopping_closes_it() {
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    let mut view = reading();
    let mut doc = unread();

    // Tab reaches the chrome even though the page has the keyboard, because
    // the read-only page declines Tab.
    assert!(View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty()
    ));
    assert_eq!(view.surface_mut().focus(), Some(TabRow::Edit));
    assert!(View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    assert!(view.surface_mut().is_editing());

    // **The page is still there, and it is now editable.** No composer, no
    // Markdown source on screen: the same bullets, open for typing.
    let page = view.document().expect("the page is still the page");
    assert!(page.is_editable());
    assert!(!page.leaf().view().read_only);
    assert!(
        page.leaf()
            .body()
            .rows
            .iter()
            .any(|row| row.text == "the second region"),
        "the document's own words, not its markers"
    );
    let rows = view.surface_mut().rows();
    assert!(rows.contains(&TabRow::Save), "{rows:?}");
    assert!(rows.contains(&TabRow::StopEditing), "{rows:?}");
    assert!(
        !rows.contains(&TabRow::Document),
        "the tab draws no document of its own: {rows:?}"
    );

    // The ring went with the row it was on; Escape from the page puts it on
    // the first of the controls that replaced it, and Tab walks from there.
    assert_eq!(view.surface_mut().focus(), None);
    assert!(View::key(
        &mut view,
        &mut doc,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert_eq!(view.surface_mut().focus(), Some(TabRow::Save));

    let stop = view
        .surface_mut()
        .laid_rows()
        .into_iter()
        .find(|(row, _)| *row == TabRow::StopEditing)
        .expect("a Stop editing row");
    View::mouse_down(&mut view, &mut doc, stop.1.x + 4.0, stop.1.y + 2.0, false);
    assert!(!view.surface_mut().is_editing());
    let page = view.document().expect("the page is still there");
    assert!(!page.is_editable(), "and it is read-only again");
    assert!(page.leaf().view().read_only);
}

/// **Typing into the page reaches the tab**, which is the only thing it
/// needs from the page: whether *Save* is live.
#[test]
fn what_is_typed_into_the_page_makes_save_live() {
    use winit::keyboard::{Key, ModifiersState};
    let mut view = reading();
    let mut doc = unread();
    view.surface_mut().begin_editing();
    view.settle();
    assert!(!view.surface_mut().document_edited());

    assert!(View::key(
        &mut view,
        &mut doc,
        &Key::Character("N".into()),
        Some("N"),
        ModifiersState::empty()
    ));
    assert!(
        view.surface_mut().document_edited(),
        "the tab knows the page moved"
    );
}

/// **A press on Save hands the tab the document's own Markdown.**
///
/// There is no worker under test, so nothing leaves — what is asserted is
/// the seam: the ask is taken, the export happens here rather than in the
/// tab, and what would be written is the document as the exporter writes it,
/// with the typing in it.
#[test]
fn pressing_save_exports_the_page_and_hands_it_over() {
    use winit::keyboard::{Key, ModifiersState};
    let mut view = reading();
    let mut doc = unread();
    view.surface_mut().begin_editing();
    view.settle();

    for character in ["N", "e", "w"] {
        View::key(
            &mut view,
            &mut doc,
            &Key::Character(character.into()),
            Some(character),
            ModifiersState::empty(),
        );
    }
    let would_write = view
        .document()
        .expect("a page")
        .markdown()
        .expect("markdown");
    assert!(would_write.contains("New"), "{would_write}");
    assert!(
        would_write.contains("- the second region"),
        "the bullets are still bullets: {would_write}"
    );

    let save = view
        .surface_mut()
        .laid_rows()
        .into_iter()
        .find(|(row, _)| *row == TabRow::Save)
        .expect("a Save row");
    View::mouse_down(&mut view, &mut doc, save.1.x + 4.0, save.1.y + 2.0, false);
    assert!(
        !view.surface_mut().take_save_request(),
        "the ask was settled by the view rather than left pending"
    );
    // The tab is still on version 1 and still editing: nothing came back,
    // because there is no worker here to answer.
    assert!(view.surface_mut().is_editing());
}

/// **Nothing that is not a document offers an edit**, and nothing about
/// editing changed that: the states keep the tab's own lines.
#[test]
fn an_empty_version_keeps_the_tabs_own_line_and_offers_no_edit() {
    let mut view = ResultView::new();
    View::set_viewport(&mut view, 1_200.0, 800.0);
    let opened = a_result(0x93);
    let version = opened.version;
    view.open(opened);
    view.take_content(
        version,
        Some(a_place()),
        Content::Document(Box::new(MarkdownDocument::of("   \n").expect("a document"))),
    );
    assert!(view.document().is_none());
    let rows = view.surface_mut().rows();
    assert!(rows.contains(&TabRow::Document), "{rows:?}");
    assert!(!rows.contains(&TabRow::Edit), "{rows:?}");
}

/// A version whose bytes are on another device says so, and opens no page —
/// an empty document would be this view inventing one.
#[test]
fn a_version_held_elsewhere_opens_no_page() {
    let mut view = ResultView::new();
    View::set_viewport(&mut view, 1_200.0, 800.0);
    view.open(OpenedArtifact {
        held_elsewhere: Some(Some("the laptop".into())),
        ..a_result(0x91)
    });
    assert!(view.document().is_none());
    let rows = view.surface_mut().rows();
    assert!(rows.contains(&TabRow::Document), "{rows:?}");
}

/// Anything that is not Markdown keeps what it had: the tab names the type,
/// the size and the filename, and no document view opens over it.
#[test]
fn a_version_that_is_not_markdown_keeps_the_tabs_own_lines() {
    let mut view = ResultView::new();
    View::set_viewport(&mut view, 1_200.0, 800.0);
    let opened = a_result(0x92);
    let version = opened.version;
    view.open(opened);
    view.take_content(
        version,
        Some(a_place()),
        Content::NotMarkdown {
            mime: "application/pdf".into(),
            size_bytes: 4_404_019,
            filename: Some("findings.pdf".into()),
        },
    );
    assert!(view.document().is_none());
    let shown = view.surface_mut().lines().join("\n");
    assert!(shown.contains("application/pdf"), "{shown}");
    assert!(shown.contains("4.2 MB"), "{shown}");
}
