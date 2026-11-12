// SPDX-License-Identifier: MPL-2.0

//! Every row shape a hit can take since words are read from files: a page, no
//! page, live and in the bin — and what each one draws, says, and opens.

use super::*;
use crate::{Catalog, Disposition, SearchSurface, Target};
use koru_draw::inspect;
use koru_draw::scene;
use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};
use koru_store_wire::search::{MatchSpan, field};
use vello::Scene;
use vello::kurbo::Affine;

/// Words read from page `page` of a PDF, as the index answers them: the
/// file's own manifest entry is the document, its root is the node.
fn a_read_hit(page: Option<u32>) -> SearchHit {
    let file = DocumentId::new().to_storage();
    SearchHit {
        workspace: WorkspaceId::new().to_string(),
        workspace_name: "Acme".into(),
        document: Some(file.clone()),
        node: Some(file),
        document_kind: "file".into(),
        mime: Some("application/pdf".into()),
        kind: "file".into(),
        field: field::READ.into(),
        title: "Lease.pdf".into(),
        breadcrumb: "Acme / Files / Lease.pdf".into(),
        breadcrumb_spans: Vec::new(),
        snippet: "…ninety days' termination notice in writing…".into(),
        spans: vec![
            MatchSpan {
                start: "…ninety days' ".len(),
                end: "…ninety days' termination".len(),
            },
            MatchSpan {
                start: "…ninety days' termination ".len(),
                end: "…ninety days' termination notice".len(),
            },
        ],
        lifecycle: Lifecycle::Live,
        trashed_at: None,
        trashed_by: None,
        score: 100.0,
        observed_modified_at_ms: None,
        workspace_last_used_at_ms: None,
        page,
        picture_name: None,
    }
}

/// Words read out of a picture inside a page: the page is the document, the
/// picture's own node is the node, and the kind says it is a picture.
fn a_picture_hit(name: Option<&str>) -> SearchHit {
    SearchHit {
        document_kind: "page".into(),
        mime: None,
        kind: "image".into(),
        title: "Site visit".into(),
        breadcrumb: "Acme / Site visit".into(),
        snippet: "…whiteboard sketch of the pump…".into(),
        spans: vec![MatchSpan {
            start: "…whiteboard sketch of the ".len(),
            end: "…whiteboard sketch of the pump".len(),
        }],
        node: Some(DocumentId::new().to_storage()),
        page: None,
        picture_name: name.map(str::to_owned),
        ..a_read_hit(None)
    }
}

// ------------------------------------------------------------ the picture mark

/// **The mark is decided by the store's tokens**: read words from a picture
/// node. A file's read words are a file's result; a picture's typed name is
/// a typed match.
#[test]
fn only_words_read_from_a_picture_carry_the_mark() {
    assert!(is_picture(&a_picture_hit(None)));
    assert!(
        !is_picture(&a_read_hit(Some(3))),
        "a file is its own result"
    );
    let mut named_by_hand = a_picture_hit(None);
    named_by_hand.field = field::NAME.into();
    assert!(!is_picture(&named_by_hand), "a name somebody typed");
}

/// The name follows the mark only when a person gave the picture one; a
/// blank name is no name.
#[test]
fn the_name_follows_the_mark_only_when_there_is_one() {
    assert_eq!(
        picture_name(&a_picture_hit(Some("Pump room"))),
        Some("Pump room")
    );
    assert_eq!(picture_name(&a_picture_hit(None)), None);
    assert_eq!(picture_name(&a_picture_hit(Some("  "))), None);
    let mut file = a_read_hit(None);
    file.picture_name = Some("stray".into());
    assert_eq!(
        picture_name(&file),
        None,
        "only a picture has a picture's name"
    );
}

/// **A picture hit is announced as one** (EXPERIENCE §3: "A result that came
/// from a picture says so in words to a screen reader and not only with the
/// picture mark"), in every language, with the whole snippet.
#[test]
fn a_picture_hit_is_announced_as_a_picture_in_every_language() {
    let hit = a_picture_hit(None);
    assert_eq!(
        spoken(Lang::En, &hit, false),
        "Site visit, Acme / Site visit, words read from a picture, …whiteboard sketch of the pump…"
    );
    let named = a_picture_hit(Some("Pump room"));
    assert_eq!(
        spoken(Lang::En, &named, false),
        "Site visit, Acme / Site visit, words read from the picture Pump room, …whiteboard sketch of the pump…"
    );
    for lang in Lang::ALL {
        let said = spoken(lang, &hit, false);
        let typed = {
            let mut typed = hit.clone();
            typed.field = field::BODY.into();
            spoken(lang, &typed, false)
        };
        assert_ne!(
            said,
            typed,
            "{}: a picture sounds like a picture",
            lang.tag()
        );
        assert!(said.contains(&hit.snippet), "{}: {said}", lang.tag());
        assert!(said.contains("Site visit"), "{}: {said}", lang.tag());
        assert!(!said.contains("{}"), "{}: {said}", lang.tag());
        let said = spoken(lang, &named, false);
        assert!(said.contains("Pump room"), "{}: {said}", lang.tag());
    }
}

/// The surface's own accessible name for the row is the same sentence, so the
/// day rows are published (OCR QUESTIONS Q11) a picture is heard as one.
#[test]
fn the_surface_names_a_picture_row_as_a_picture() {
    let mut surface = SearchSurface::new();
    surface.page.hits = vec![a_picture_hit(None)];
    let name = koru_i18n::with_current(Lang::De, || surface.result_accessible_name(0)).unwrap();
    assert!(name.contains("aus einem Bild gelesene Wörter"), "{name}");
}

/// **The snippet starts after the mark**, and after the name when there is
/// one; its text and ranges are the store's, unmoved.
#[test]
fn the_snippet_begins_after_the_picture_mark_and_keeps_its_ranges() {
    let unnamed = a_picture_hit(None);
    let named = a_picture_hit(Some("Pump room"));
    let mut surface = surface_showing(vec![unnamed.clone(), named.clone()]);
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let after_mark = surface.paint_snippet_line(&mut canvas, root, &unnamed, 40.0, 50.0, 500.0);
    assert!(after_mark > 40.0, "the mark leads the snippet");
    let after_name = surface.paint_snippet_line(&mut canvas, root, &named, 40.0, 50.0, 500.0);
    let name_width = surface
        .engine
        .simple_layout("Pump room", koru_draw::theme::role::meta())
        .full_width();
    assert!(
        after_name > after_mark + name_width,
        "the name sits between"
    );
    assert_eq!(surface.page.hits[1].snippet, named.snippet);
    assert_eq!(surface.page.hits[1].spans, named.spans);
}

/// The F12 tree: the mark is a part of its own result row, under the modal,
/// and only a picture hit has one.
#[test]
fn the_inspector_nests_the_picture_mark_inside_its_result() {
    use crate::SearchModal;
    let viewport = (900.0, 650.0);
    let modal = SearchModal::new(viewport);
    let mut search = surface_showing(vec![a_picture_hit(Some("Pump room")), a_read_hit(Some(4))]);
    search.set_viewport(modal.bounds().w, modal.bounds().h);
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, viewport.0, viewport.1));
    modal.draw(&mut search, &mut Scene::new(), root, true, (0.0, 0.0));
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let rows: Vec<usize> = (0..records.len())
        .filter(|&i| records[i].name == "Search result")
        .collect();
    let marks: Vec<usize> = (0..records.len())
        .filter(|&i| records[i].name == "Picture mark")
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(marks.len(), 1, "only the picture hit has a mark");
    assert_eq!(records[marks[0]].parent, Some(rows[0]));
    assert!(
        !records
            .iter()
            .any(|r| r.name == "Page prefix" && r.parent == Some(rows[0])),
        "a picture has no page"
    );
}

// ------------------------------------------------------------ the page label

/// *Page 12 ·* is formatted per language like any other number
/// (EXPERIENCE §3 "In eight languages"): the word is the language's own, and
/// so are the digits and the grouping.
#[test]
fn the_page_reads_in_every_language_with_its_own_digits() {
    let cases = [
        (Lang::En, "Page 12", "Page 1,234"),
        (Lang::Es, "Página 12", "Página 1.234"),
        (Lang::PtBr, "Página 12", "Página 1.234"),
        (Lang::De, "Seite 12", "Seite 1.234"),
        (Lang::Fr, "Page 12", "Page 1\u{202f}234"),
        (Lang::Ar, "صفحة ١٢", "صفحة ١٬٢٣٤"),
        (Lang::ZhHans, "第 12 页", "第 1,234 页"),
        (Lang::Ja, "12 ページ", "1,234 ページ"),
    ];
    assert_eq!(cases.len(), Lang::ALL.len());
    for (lang, twelve, thousands) in cases {
        assert_eq!(
            page_label(lang, &a_read_hit(Some(12))).as_deref(),
            Some(twelve),
            "{}",
            lang.tag()
        );
        assert_eq!(
            page_label(lang, &a_read_hit(Some(1_234))).as_deref(),
            Some(thousands),
            "{}",
            lang.tag()
        );
    }
}

/// A picture, a one-page PDF, a text file and every typed field carry no page,
/// and draw no prefix.
#[test]
fn a_hit_with_no_page_has_no_prefix() {
    for lang in Lang::ALL {
        assert_eq!(page_label(lang, &a_read_hit(None)), None);
    }
}

/// **The label is a whole label and nothing else.** Arabic reads right to left
/// inside its own run; a separator inside the same run would be carried to its
/// far side by the bidi algorithm. So no language's label holds the separator,
/// and no language's label is a fragment around a number with the separator
/// expected to finish it.
#[test]
fn the_label_never_carries_the_separator() {
    for lang in Lang::ALL {
        let label = page_label(lang, &a_read_hit(Some(7))).unwrap();
        assert!(!label.contains('·'), "{}: {label}", lang.tag());
        assert!(!label.contains("{}"), "{}: {label}", lang.tag());
        assert_eq!(label.trim(), label, "{}", lang.tag());
    }
    assert!(Lang::Ar.is_rtl());
}

// ------------------------------------------------------------ the context line

/// A live result's context line is where it is, and nothing else: a file's
/// result names no page it is attached to (withdrawn, OCR QUESTIONS Q6).
#[test]
fn a_live_result_shows_its_location_alone() {
    assert_eq!(
        context_line("Acme / Files / Lease.pdf", Lifecycle::Live),
        "Acme / Files / Lease.pdf"
    );
    assert_eq!(
        context(&a_read_hit(Some(3))),
        "Acme / Files / Lease.pdf",
        "a page on the hit changes the snippet's lead, not the context line"
    );
}

/// **The breadcrumb's highlight ranges stay valid**: the bin comes after it,
/// so it is still the line's prefix, byte for byte.
#[test]
fn the_bin_follows_the_location() {
    let breadcrumb = "Acme / Tax / Lease.pdf";
    let line = context_line(breadcrumb, Lifecycle::Trashed);
    assert!(line.starts_with(breadcrumb));
    assert_eq!(line, "Acme / Tax / Lease.pdf · In Trash");
}

// ------------------------------------------------------------ the screen reader

/// A screen reader hears the page **in words**, in the language's own
/// sentence, and the whole snippet rather than the elided run on screen.
#[test]
fn a_read_hit_with_a_page_is_announced_with_the_page_in_words() {
    let hit = a_read_hit(Some(12));
    assert_eq!(
        spoken(Lang::En, &hit, false),
        "Lease.pdf, Acme / Files / Lease.pdf, page 12, …ninety days' termination notice in writing…"
    );
    assert_eq!(
        spoken(Lang::De, &hit, false),
        "Lease.pdf, Acme / Files / Lease.pdf, Seite 12, …ninety days' termination notice in writing…"
    );
    let arabic = spoken(Lang::Ar, &hit, false);
    assert!(arabic.contains("صفحة ١٢"), "{arabic}");
    for lang in Lang::ALL {
        let said = spoken(lang, &hit, false);
        assert!(said.contains("Lease.pdf"), "{}: {said}", lang.tag());
        assert!(said.contains(&hit.snippet), "{}: {said}", lang.tag());
        assert!(!said.contains("{}"), "{}: {said}", lang.tag());
        // The number is the language's own, as on screen.
        assert!(
            said.contains(&koru_i18n::number::decimal(lang, 12)),
            "{}: {said}",
            lang.tag()
        );
    }
}

/// No page: the same sentence without one — not *page 0*, not *page none*.
#[test]
fn a_read_hit_with_no_page_is_announced_without_one() {
    let hit = a_read_hit(None);
    assert_eq!(
        spoken(Lang::En, &hit, false),
        "Lease.pdf, Acme / Files / Lease.pdf, …ninety days' termination notice in writing…"
    );
    for lang in Lang::ALL {
        let said = spoken(lang, &hit, false);
        let with_page = spoken(lang, &a_read_hit(Some(12)), false);
        assert_ne!(said, with_page, "{}", lang.tag());
        assert!(!said.contains("{}"), "{}: {said}", lang.tag());
    }
}

/// A title match is its title and where it is; it has no snippet line.
#[test]
fn a_title_match_is_announced_as_title_and_place() {
    let mut hit = a_read_hit(None);
    hit.field = field::TITLE.into();
    hit.snippet = "Lease.pdf".into();
    assert_eq!(
        spoken(Lang::En, &hit, true),
        "Lease.pdf, Acme / Files / Lease.pdf"
    );
    assert_eq!(
        spoken(Lang::ZhHans, &hit, true),
        "Lease.pdf，Acme / Files / Lease.pdf"
    );
}

/// The surface answers for its own rows, in the process's language.
#[test]
fn the_surface_names_each_row_for_a_screen_reader() {
    let mut surface = SearchSurface::new();
    surface.page.hits = vec![a_read_hit(Some(12)), a_read_hit(None)];
    let first = koru_i18n::with_current(Lang::Fr, || surface.result_accessible_name(0)).unwrap();
    assert!(first.contains("page 12"), "{first}");
    let second = surface.result_accessible_name(1).unwrap();
    assert!(!second.contains("page"), "{second}");
    assert_eq!(surface.result_accessible_name(2), None);
}

// ------------------------------------------------------------ drawing

/// A surface showing `hits` for a typed query, as a completed read leaves it.
fn surface_showing(hits: Vec<SearchHit>) -> SearchSurface {
    let mut surface = SearchSurface::new();
    surface.set_viewport(620.0, 400.0);
    surface
        .query
        .set_text(&mut surface.engine, "termination notice");
    surface.pending_read = None;
    surface.pending_replace = None;
    surface.page.hits = hits;
    surface
}

/// **The snippet starts after the page, and only when there is one.** The
/// snippet itself is handed to the shared run component exactly as the store
/// sent it — the page never enters its text — so its highlight ranges are the
/// store's ranges, unmoved.
#[test]
fn the_snippet_begins_after_the_page_and_keeps_its_ranges() {
    for lang in Lang::ALL {
        koru_i18n::with_current(lang, || {
            let with_page = a_read_hit(Some(12));
            let without = a_read_hit(None);
            let mut surface = surface_showing(vec![with_page.clone(), without.clone()]);
            let mut canvas = Scene::new();
            let root = Affine::IDENTITY;

            let start = surface.paint_snippet_line(&mut canvas, root, &without, 40.0, 50.0, 500.0);
            assert_eq!(start, 40.0, "{}: no page, no lead", lang.tag());

            let start =
                surface.paint_snippet_line(&mut canvas, root, &with_page, 40.0, 50.0, 500.0);
            let label = page_label(lang, &with_page).unwrap();
            let label_width = surface
                .engine
                .simple_layout(&label, koru_draw::theme::role::meta())
                .full_width();
            assert!(
                start > 40.0 + label_width,
                "{}: the snippet starts past the label and its separator",
                lang.tag()
            );
            // What the run component was given is the hit's own text and
            // ranges: the surface holds no modified copy of either.
            assert_eq!(surface.page.hits[0].snippet, with_page.snippet);
            assert_eq!(surface.page.hits[0].spans, with_page.spans);
        });
    }
}

/// A row too narrow for its snippet keeps its page: the page is drawn first
/// and the snippet elides into what is left, never the other way round.
#[test]
fn a_narrow_row_still_draws_its_page() {
    let hit = a_read_hit(Some(12));
    let mut surface = surface_showing(vec![hit.clone()]);
    let start =
        surface.paint_snippet_line(&mut Scene::new(), Affine::IDENTITY, &hit, 0.0, 0.0, 90.0);
    assert!(start > 0.0);
    assert!(
        start <= 90.0 + 1.0,
        "the lead is clamped to the row: {start}"
    );
}

/// The F12 tree: each result is one record, and the page prefix is a part of
/// its row, nested under the modal like everything else the surface draws
/// (DESIGN: every primitive nested inside its parent).
#[test]
fn the_inspector_nests_the_page_prefix_inside_its_result() {
    use crate::SearchModal;
    let viewport = (900.0, 650.0);
    let modal = SearchModal::new(viewport);
    let mut search = surface_showing(vec![a_read_hit(Some(12)), a_read_hit(None)]);
    search.set_viewport(modal.bounds().w, modal.bounds().h);
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, viewport.0, viewport.1));
    modal.draw(&mut search, &mut Scene::new(), root, true, (0.0, 0.0));
    let snapshot = capture.finish();
    let records = &snapshot.records;

    let rows: Vec<usize> = (0..records.len())
        .filter(|&i| records[i].name == "Search result")
        .collect();
    assert_eq!(rows.len(), 2, "one record per result");
    let prefixes: Vec<usize> = (0..records.len())
        .filter(|&i| records[i].name == "Page prefix")
        .collect();
    assert_eq!(prefixes.len(), 1, "only the hit with a page has a prefix");

    let prefix = prefixes[0];
    assert_eq!(
        records[prefix].parent,
        Some(rows[0]),
        "the prefix is its row's"
    );
    assert!(
        records
            .iter()
            .any(|record| record.parent == Some(prefix) || is_under(records, record, prefix)),
        "the label's runs are inside the prefix"
    );
    for row in rows {
        let mut at = records[row].parent;
        let mut owner = None;
        while let Some(index) = at {
            owner = Some(index);
            at = records[index].parent;
        }
        assert_eq!(owner.map(|i| records[i].name), Some("SearchModal"));
    }
}

fn is_under(records: &[inspect::Record], record: &inspect::Record, ancestor: usize) -> bool {
    let mut at = record.parent;
    while let Some(index) = at {
        if index == ancestor {
            return true;
        }
        at = records[index].parent;
    }
    false
}

// ------------------------------------------------------------ picking it

/// **Picking a file's result keeps today's behaviour.** The surface hands the
/// host the file's document and node — the same target a match on the file's
/// name produces — and nothing about the page or the field. The host then
/// sees a `file` manifest entry and reveals it under Explore → Files
/// (`koru_workspace_host::app::open_search_target_in_panes`), which decides by
/// the entry's kind and never by the field.
#[test]
fn picking_a_read_hit_opens_the_file_as_picking_its_name_does() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let open = |hit: SearchHit| {
        let mut surface = surface_showing(vec![hit]);
        surface.catalog = Catalog::for_test(
            vec![(workspace, "Acme".into(), profile)],
            vec![(profile, "Profile".into())],
        );
        surface.selected_profile = Some(profile);
        surface.list.selected = 0;
        surface.request_open(Disposition::CurrentWindow);
        surface.take_target()
    };
    let mut read = a_read_hit(Some(12));
    read.workspace = workspace.to_string();
    let mut by_name = read.clone();
    by_name.field = field::TITLE.into();
    by_name.page = None;
    by_name.snippet = "Lease.pdf".into();
    by_name.spans = vec![MatchSpan { start: 0, end: 5 }];

    let picked = open(read.clone()).expect("a read hit opens");
    let named = open(by_name).expect("a name hit opens");
    let (
        Target::Store {
            document,
            node,
            trashed,
            disposition,
            ..
        },
        Target::Store {
            document: named_document,
            node: named_node,
            ..
        },
    ) = (&picked, &named)
    else {
        panic!("both open in the store: {picked:?} {named:?}");
    };
    assert_eq!(document, named_document);
    assert_eq!(node, named_node);
    assert_eq!(
        document.as_ref().map(|id| id.to_storage()),
        read.document.clone(),
        "the file's own entry"
    );
    assert!(!trashed);
    assert_eq!(*disposition, Disposition::CurrentWindow);
}
