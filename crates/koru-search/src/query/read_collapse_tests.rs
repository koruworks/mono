// SPDX-License-Identifier: MPL-2.0

//! A file contributes at most one `read` hit to a page of results, and nothing
//! that is not a `read` hit moves.
//!
//! The words read out of a file are one fragment per page, and each matching
//! fragment is a hit. Before the collapse, a word on every page of one long PDF
//! filled the whole page of 50 and the surface — one row per document — showed
//! that one file and nothing else.

use std::collections::BTreeSet;

use super::*;
use crate::model::{AttrRow, Fragment, SearchHit};
use koru_projection::model::ListedDocument;
use koru_store_core::ids::{DocumentId, WorkspaceId};

// ------------------------------------------------------------- the corpus

/// A deterministic id, so a tie that falls through to `stable_key` falls the
/// same way on every run.
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

struct Doc {
    id: String,
    title: String,
    kind: &'static str,
    parent: Option<String>,
    trashed: bool,
    at: i64,
    fragments: Vec<Fragment>,
    values: Vec<AttrRow>,
}

impl Doc {
    fn page(n: u32, title: &str, at: i64) -> Self {
        Doc {
            id: id(n),
            title: title.to_owned(),
            kind: "page",
            parent: None,
            trashed: false,
            at,
            fragments: Vec::new(),
            values: Vec::new(),
        }
    }

    fn file(n: u32, title: &str, at: i64) -> Self {
        Doc {
            kind: "file",
            ..Doc::page(n, title, at)
        }
    }

    fn under(mut self, parent: u32) -> Self {
        self.parent = Some(id(parent));
        self
    }

    fn trashed(mut self) -> Self {
        self.trashed = true;
        self
    }

    /// A node's own text, under `field`.
    fn text(mut self, node: &str, field: &str, text: &str) -> Self {
        let seq = self.fragments.len() as i64 + 1;
        self.fragments.push(Fragment {
            key: format!("node:{}:{node}:primary", self.id),
            workspace: String::new(),
            document: Some(self.id.clone()),
            node: Some(node.to_owned()),
            seq,
            kind: "paragraph".to_owned(),
            field: field.to_owned(),
            text: text.to_owned(),
            page: None,
        });
        self
    }

    fn tag(mut self, node: &str, tag: &str) -> Self {
        let seq = self.fragments.len() as i64 + 1;
        self.fragments.push(Fragment {
            key: format!("node:{}:{node}:attr:{tag}", self.id),
            workspace: String::new(),
            document: Some(self.id.clone()),
            node: Some(node.to_owned()),
            seq,
            kind: "paragraph".to_owned(),
            field: TAG_FIELD.clone(),
            text: tag.to_owned(),
            page: None,
        });
        self
    }

    fn value(mut self, node: &str, role: &str, kind: &str, value: &str) -> Self {
        self.values.push(AttrRow {
            workspace: String::new(),
            document: self.id.clone(),
            node: node.to_owned(),
            role: role.to_owned(),
            kind: kind.to_owned(),
            value: value.to_owned(),
            num: None,
            zone: None,
        });
        self
    }

    /// Words read out of a picture or a file: one fragment per page, keyed and
    /// numbered the way [`crate::fragments::reading_fragments`] writes them.
    fn read(mut self, node: &str, kind: &str, seq: i64, page: Option<u32>, text: &str) -> Self {
        self.fragments.push(Fragment {
            key: format!("node:{}:{node}:read:{}", self.id, page.unwrap_or(1)),
            workspace: String::new(),
            document: Some(self.id.clone()),
            node: Some(node.to_owned()),
            seq,
            kind: kind.to_owned(),
            field: field::READ.to_owned(),
            text: text.to_owned(),
            page,
        });
        self
    }

    /// A file node whose reading has these pages, numbered from 1.
    fn pages(self, texts: impl IntoIterator<Item = String>) -> Self {
        let texts: Vec<String> = texts.into_iter().collect();
        let numbered = texts.len() > 1;
        let mut doc = self;
        for (i, text) in texts.iter().enumerate() {
            let page = i as u32 + 1;
            doc = doc.read("file", "file", 1, numbered.then_some(page), text);
        }
        doc
    }
}

/// Workspaces, each `(id, name, documents)`.
fn corpus(workspaces: Vec<(String, &str, Vec<Doc>)>) -> SearchIndex {
    let mut index = SearchIndex::in_memory().unwrap();
    for (workspace, name, docs) in workspaces {
        let listing: Vec<ListedDocument> = docs
            .iter()
            .enumerate()
            .map(|(ordinal, doc)| ListedDocument {
                document: DocumentId::parse(&doc.id).unwrap(),
                parent: doc.parent.as_deref().map(|p| DocumentId::parse(p).unwrap()),
                ordinal: ordinal as u32,
                title: doc.title.clone(),
                kind: doc.kind.to_owned(),
                mime: (doc.kind == "file").then(|| "application/pdf".to_owned()),
                trashed_at: doc.trashed.then_some(5),
                trashed_by: None,
            })
            .collect();
        index
            .set_listing(&workspace, name, b"d1", &listing)
            .unwrap();
        for doc in docs {
            let fragments: Vec<Fragment> = doc
                .fragments
                .into_iter()
                .map(|f| Fragment {
                    workspace: workspace.clone(),
                    ..f
                })
                .collect();
            let values: Vec<AttrRow> = doc
                .values
                .into_iter()
                .map(|v| AttrRow {
                    workspace: workspace.clone(),
                    ..v
                })
                .collect();
            index
                .replace_document(&workspace, &doc.id, &fragments, &values, b"r1", doc.at)
                .unwrap();
        }
    }
    index.warm_fuzzy_vocabulary().unwrap();
    index
}

fn ws(n: u32) -> String {
    WorkspaceId::parse(&id(900 + n)).unwrap().to_storage()
}

fn ask(index: &SearchIndex, query: SearchQuery) -> SearchPage {
    index.query(&query, &[]).unwrap()
}

fn words(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        page_size: 50,
        ..SearchQuery::default()
    }
}

fn reads(page: &SearchPage) -> Vec<&SearchHit> {
    page.hits
        .iter()
        .filter(|hit| hit.field == field::READ)
        .collect()
}

fn titles(page: &SearchPage) -> BTreeSet<String> {
    page.hits.iter().map(|hit| hit.title.clone()).collect()
}

fn highlighted(hit: &SearchHit) -> Vec<&str> {
    hit.spans
        .iter()
        .map(|s| &hit.snippet[s.start..s.end])
        .collect()
}

/// Six files carry the word. One is a 300-page PDF with it on every page, and
/// it is both the newest file and the best-ranked one, so it is first under
/// either sort — exactly the shape the real run met.
fn six_files() -> SearchIndex {
    let mut docs = vec![
        Doc::file(1, "survey.pdf", 1_000)
            .pages((1..=300).map(|p| format!("Zebrafish survey, page {p}"))),
    ];
    for n in 2..=6u32 {
        docs.push(
            Doc::file(n, &format!("notes-{n}.pdf"), 100 + n as i64).pages([
                format!("Feeding notes {n} for the tank"),
                format!("notes on the zebrafish tank {n}"),
            ]),
        );
    }
    corpus(vec![(ws(1), "Lab", docs)])
}

// ------------------------------------------------------------- the defect

/// **The defect, reproduced.** A word in six files, on 300 pages of one of
/// them: the first page of 50 results must name all six.
#[test]
fn a_word_on_every_page_of_one_file_does_not_hide_the_other_files() {
    let index = six_files();
    for sort in [
        SearchSort::Relevance,
        SearchSort::ObservedModified,
        SearchSort::LastUsedWorkspace,
    ] {
        let page = ask(
            &index,
            SearchQuery {
                sort,
                ..words("zebrafish")
            },
        );
        assert_eq!(
            titles(&page),
            BTreeSet::from(
                [
                    "survey.pdf",
                    "notes-2.pdf",
                    "notes-3.pdf",
                    "notes-4.pdf",
                    "notes-5.pdf",
                    "notes-6.pdf",
                ]
                .map(str::to_owned)
            ),
            "{sort:?}: every file that holds the word is on the first page"
        );
        assert_eq!(page.hits.len(), 6, "{sort:?}: one hit per file");
        assert_eq!(page.next_offset, None, "{sort:?}: and nothing is left");
    }
}

/// The survivor is the page the ranking already puts first for that file,
/// and it carries **its own** page, snippet and highlight.
#[test]
fn the_page_that_survives_is_the_best_ranked_one_with_its_own_evidence() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![Doc::file(1, "scan.pdf", 10).pages([
            "the zebrafish tank".to_owned(),
            "an unrelated page".to_owned(),
            "zebrafish tank".to_owned(),
            // An exact match of the whole fragment: +30, the best.
            "Zebrafish".to_owned(),
            "more about the zebrafish".to_owned(),
        ])],
    )]);
    let page = ask(&index, words("zebrafish"));
    let read = reads(&page);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].page, Some(4));
    assert_eq!(read[0].snippet, "Zebrafish");
    assert_eq!(highlighted(read[0]), ["Zebrafish"]);
}

/// **Among equals, the lowest page** — even where the old tie-break, the
/// text, would have preferred a later one.
#[test]
fn among_equally_ranked_pages_the_lowest_survives() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![Doc::file(1, "scan.pdf", 10).pages([
            "c zebrafish".to_owned(),
            "b zebrafish".to_owned(),
            "a zebrafish".to_owned(),
        ])],
    )]);
    let page = ask(&index, words("zebrafish"));
    let read = reads(&page);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].page, Some(1));
    assert_eq!(read[0].snippet, "c zebrafish");
    assert_eq!(highlighted(read[0]), ["zebrafish"]);
}

/// The next page continues after the collapsed set: no file twice, none
/// skipped, and `next_offset` counts what is left after the collapse.
#[test]
fn paging_continues_after_the_collapsed_set() {
    let index = six_files();
    for sort in [SearchSort::Relevance, SearchSort::ObservedModified] {
        let mut seen: Vec<SearchHit> = Vec::new();
        let mut offset = 0;
        let mut pages = 0;
        loop {
            let page = ask(
                &index,
                SearchQuery {
                    sort,
                    page_size: 4,
                    offset,
                    ..words("zebrafish")
                },
            );
            pages += 1;
            seen.extend(page.hits.iter().cloned());
            match page.next_offset {
                Some(next) => {
                    assert_eq!(next, offset + 4, "{sort:?}");
                    offset = next;
                }
                None => break,
            }
            assert!(pages < 10, "{sort:?}: paging must end");
        }
        assert_eq!(pages, 2, "{sort:?}: six hits are two pages of four");
        let documents: Vec<_> = seen.iter().map(|hit| hit.document.clone()).collect();
        let distinct: BTreeSet<_> = documents.iter().cloned().collect();
        assert_eq!(documents.len(), 6, "{sort:?}: {documents:?}");
        assert_eq!(
            distinct.len(),
            6,
            "{sort:?}: no file repeated, none skipped"
        );
    }
}

// ------------------------------------------------------------- what is kept apart

/// A file's name and its words are two fields, and two hits: the surface's own
/// rule picks the row.
#[test]
fn a_title_hit_and_a_read_hit_are_not_collapsed_into_one() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![
            Doc::file(1, "Zebrafish report.pdf", 10)
                .pages((1..=40).map(|p| format!("zebrafish count {p}"))),
        ],
    )]);
    let page = ask(&index, words("zebrafish"));
    let fields: Vec<&str> = page.hits.iter().map(|hit| hit.field.as_str()).collect();
    assert_eq!(fields, [field::TITLE, field::READ]);
}

/// **Per document, not per node.** A page whose three pictures all hold the
/// word answers with one `read` hit — the best picture — beside its typed
/// line, which is not a `read` fragment and is untouched.
#[test]
fn several_pictures_in_one_document_are_one_read_hit() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![
            Doc::page(1, "Aquarium", 10)
                .text("n1", field::BODY, "zebrafish care")
                .read("p1", "image", 2, None, "zebrafish label")
                .read("p2", "image", 3, None, "Zebrafish")
                .read("p3", "image", 4, None, "a zebrafish photo"),
        ],
    )]);
    let page = ask(&index, words("zebrafish"));
    let read = reads(&page);
    assert_eq!(read.len(), 1, "one read hit for the document");
    assert_eq!(read[0].node.as_deref(), Some("p2"), "the exact match wins");
    assert_eq!(read[0].page, None, "a picture has no page");
    assert_eq!(
        page.hits
            .iter()
            .filter(|hit| hit.field == field::BODY)
            .count(),
        1
    );
}

/// Two documents never collapse into each other.
#[test]
fn two_files_are_two_hits() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![
            Doc::file(1, "a.pdf", 10).pages((1..=20).map(|p| format!("zebrafish {p}"))),
            Doc::file(2, "b.pdf", 20).pages((1..=20).map(|p| format!("zebrafish {p}"))),
        ],
    )]);
    assert_eq!(reads(&ask(&index, words("zebrafish"))).len(), 2);
}

// ------------------------------------------------------------- filters, as before

/// Trash, live and all: each answers with what it did, one hit per file.
#[test]
fn the_trash_filter_still_decides_which_files_answer() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![
            Doc::file(1, "kept.pdf", 10).pages((1..=20).map(|p| format!("zebrafish {p}"))),
            Doc::file(2, "binned.pdf", 20)
                .trashed()
                .pages((1..=20).map(|p| format!("zebrafish {p}"))),
        ],
    )]);
    let with = |lifecycle| {
        titles(&ask(
            &index,
            SearchQuery {
                lifecycle,
                ..words("zebrafish")
            },
        ))
    };
    assert_eq!(
        with(LifecycleFilter::Live),
        BTreeSet::from(["kept.pdf".to_owned()])
    );
    assert_eq!(
        with(LifecycleFilter::Trash),
        BTreeSet::from(["binned.pdf".to_owned()])
    );
    let all = ask(
        &index,
        SearchQuery {
            lifecycle: LifecycleFilter::All,
            ..words("zebrafish")
        },
    );
    assert_eq!(all.hits.len(), 2);
}

/// The tag narrows first and the collapse picks among what is left: the
/// tagged picture answers even though an untagged one in the same document
/// ranks higher.
#[test]
fn a_tag_filter_narrows_before_the_collapse() {
    let index = corpus(vec![(
        ws(1),
        "Lab",
        vec![
            Doc::page(1, "Aquarium", 10)
                .read("p1", "image", 1, None, "Zebrafish")
                .read("p2", "image", 2, None, "zebrafish tank")
                .tag("p2", "fish"),
        ],
    )]);
    let page = ask(&index, words("zebrafish #fish"));
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].node.as_deref(), Some("p2"));
    let unfiltered = ask(&index, words("zebrafish"));
    assert_eq!(reads(&unfiltered)[0].node.as_deref(), Some("p1"));
}

/// The workspace scope still decides which files answer.
#[test]
fn the_workspace_scope_still_decides_which_files_answer() {
    let index = corpus(vec![
        (
            ws(1),
            "Lab",
            vec![Doc::file(1, "lab.pdf", 10).pages((1..=30).map(|p| format!("zebrafish {p}")))],
        ),
        (
            ws(2),
            "Home",
            vec![Doc::file(2, "home.pdf", 20).pages((1..=30).map(|p| format!("zebrafish {p}")))],
        ),
    ]);
    let scoped = ask(
        &index,
        SearchQuery {
            workspaces: vec![WorkspaceId::parse(&ws(2)).unwrap()],
            ..words("zebrafish")
        },
    );
    assert_eq!(titles(&scoped), BTreeSet::from(["home.pdf".to_owned()]));
    let excluded = ask(
        &index,
        SearchQuery {
            excluded_workspaces: vec![WorkspaceId::parse(&ws(2)).unwrap()],
            ..words("zebrafish")
        },
    );
    assert_eq!(titles(&excluded), BTreeSet::from(["lab.pdf".to_owned()]));
    assert_eq!(reads(&ask(&index, words("zebrafish"))).len(), 2);
}

/// A misread or mistyped letter still finds the file, once, and the
/// correction is still spelled back.
#[test]
fn a_mistyped_word_still_finds_each_file_once() {
    let index = six_files();
    let page = ask(&index, words("zebrafsh"));
    assert_eq!(page.hits.len(), 6, "{:?}", titles(&page));
    assert_eq!(titles(&page).len(), 6);
    assert_eq!(page.corrected_query.as_deref(), Some("zebrafish"));
    assert!(
        reads(&page)
            .iter()
            .all(|hit| highlighted(hit) == ["zebrafish"] || highlighted(hit) == ["Zebrafish"])
    );
}

// ------------------------------------------------------------- nothing else moved

/// A corpus with **no `read` fragment at all**: titles, headings, bodies,
/// tasks, tags, typed attributes, a nested page, a binned page and two
/// workspaces.
fn typed_corpus() -> SearchIndex {
    corpus(vec![
        (
            ws(1),
            "Acme",
            vec![
                Doc::page(1, "Quarterly plan", 100)
                    .text("h", &field::heading(1), "Plan for the quarter")
                    .text("b1", field::BODY, "The quarterly plan covers hiring")
                    .text("t1", field::TASK, "Draft the plan outline")
                    .text("b2", field::BODY, "Unrelated text about lunch")
                    .tag("b1", "fun")
                    .value("t1", "koru.task", "text", "explicit")
                    .value("t1", "koru.priority", "text", "high"),
                Doc::page(2, "Roadmap", 200)
                    .text("b1", field::BODY, "plan B is the fallback plan")
                    .text("h2", &field::heading(2), "Planning notes")
                    .text("b2", field::BODY, "Ship the quarterly report")
                    .value("b2", "koru.priority", "text", "low"),
                Doc::page(3, "Planets", 300)
                    .under(2)
                    .text("b1", field::BODY, "Saturn has rings")
                    .text("b2", field::BODY, "A planned trip to the observatory")
                    .tag("b2", "fun/home"),
                Doc::page(4, "Old plan", 50).trashed().text(
                    "b1",
                    field::BODY,
                    "The abandoned plan for spring",
                ),
            ],
        ),
        (
            ws(2),
            "Beta",
            vec![
                Doc::page(5, "Meeting notes", 400)
                    .text("b1", field::BODY, "We discussed the plan with Acme")
                    .text("h3", &field::heading(3), "Next steps for the plan")
                    .tag("b1", "fun"),
                Doc::page(6, "Plants", 150)
                    .text("b1", field::BODY, "Water the plants daily")
                    .text("b2", field::BODY, "plan to repot the fern")
                    .text("b3", "attr:koru.status", "planned"),
            ],
        ),
    ])
}

fn render(page: &SearchPage) -> String {
    let mut out = String::new();
    for hit in &page.hits {
        out.push_str(&format!(
            "{} | {} | {} | {:?} | {} | {} {:?} | {} {:?}\n",
            hit.title,
            hit.field,
            hit.node.as_deref().unwrap_or("-"),
            hit.page,
            hit.score,
            hit.snippet,
            hit.spans
                .iter()
                .map(|s| (s.start, s.end))
                .collect::<Vec<_>>(),
            hit.breadcrumb,
            hit.breadcrumb_spans
                .iter()
                .map(|s| (s.start, s.end))
                .collect::<Vec<_>>(),
        ));
    }
    out.push_str(&format!(
        "next={:?} corrected={:?}\n",
        page.next_offset, page.corrected_query
    ));
    out
}

/// Every query shape the index answers, against [`typed_corpus`].
fn battery(index: &SearchIndex) -> String {
    index
        .record_open(&ws(1), Some(&id(1)), None, 1_000)
        .unwrap();
    index
        .record_open(&ws(2), Some(&id(5)), None, 2_000)
        .unwrap();
    let base = |text: &str| SearchQuery {
        text: text.to_owned(),
        ..SearchQuery::default()
    };
    let mut cases: Vec<(String, SearchQuery)> = vec![
        ("plan".into(), base("plan")),
        (
            "plan, observed".into(),
            SearchQuery {
                sort: SearchSort::ObservedModified,
                ..base("plan")
            },
        ),
        (
            "plan, workspace recency".into(),
            SearchQuery {
                sort: SearchSort::LastUsedWorkspace,
                ..base("plan")
            },
        ),
        ("quarterly plan".into(), base("quarterly plan")),
        ("quartrly".into(), base("quartrly")),
        ("acme plan".into(), base("acme plan")),
        ("pla".into(), base("pla")),
        ("plan #fun".into(), base("plan #fun")),
        ("#fun".into(), base("#fun")),
        ("nothing".into(), base("zzzz")),
        (
            "plan, trash".into(),
            SearchQuery {
                lifecycle: LifecycleFilter::Trash,
                ..base("plan")
            },
        ),
        (
            "plan, all".into(),
            SearchQuery {
                lifecycle: LifecycleFilter::All,
                ..base("plan")
            },
        ),
        (
            "plan, scoped".into(),
            SearchQuery {
                workspaces: vec![WorkspaceId::parse(&ws(1)).unwrap()],
                ..base("plan")
            },
        ),
        (
            "plan, excluded".into(),
            SearchQuery {
                excluded_workspaces: vec![WorkspaceId::parse(&ws(1)).unwrap()],
                ..base("plan")
            },
        ),
        (
            "priority filter alone".into(),
            SearchQuery {
                attributes: vec![AttrFilter {
                    key: "koru.priority".into(),
                    op: AttrOp::Is,
                    values: vec!["high".into()],
                    numeric_values: Vec::new(),
                    kinds: vec!["text".into()],
                }],
                ..base("")
            },
        ),
        (
            "picker".into(),
            SearchQuery {
                entity_picker: true,
                ..base("plan")
            },
        ),
    ];
    for offset in [0, 3, 6, 9, 12] {
        cases.push((
            format!("plan, page of 3 at {offset}"),
            SearchQuery {
                page_size: 3,
                offset,
                ..base("plan")
            },
        ));
    }
    let mut out = String::new();
    for (label, query) in cases {
        out.push_str(&format!("## {label}\n"));
        out.push_str(&render(&index.query(&query, &[]).unwrap()));
    }
    out
}

/// **The pin.** Rendered by the statement as it was before the collapse
/// existed (commit 9b1d8409), and compared byte for byte: same hits, same
/// scores, same order, same snippets and highlights, same paging.
#[test]
fn a_corpus_with_no_read_fragment_answers_exactly_as_before() {
    let actual = battery(&typed_corpus());
    // `.gitattributes` keeps it LF, and a checkout that did not is not a
    // change in the answers.
    let expected = include_str!("read_collapse_tests.expected").replace("\r\n", "\n");
    assert!(actual == expected, "the answers moved:\n{actual}");
}

// ---------------------------------------------------- one read hit per task

/// A page holding two marked pictures and one unmarked, a marked three-page
/// file, and an unmarked PDF beside it. Every one of them reads *invoice*.
fn marked_pictures() -> SearchIndex {
    let page = Doc::page(1, "Receipts", 10)
        .read("img-a", "image", 1, None, "invoice from the bakery")
        .value("img-a", "koru.task", "bool", "true")
        .read("img-b", "image", 2, None, "invoice from the garage")
        .value("img-b", "koru.due", "date", "2026-12-01")
        .read("img-c", "image", 3, None, "invoice nobody marked")
        .read("img-d", "image", 4, None, "another unmarked invoice")
        .read("scan", "file", 5, Some(1), "cover page of the invoice")
        .read("scan", "file", 5, Some(2), "invoice totals")
        .read("scan", "file", 5, Some(3), "appendix to the invoice")
        .value("scan", "koru.task", "bool", "true");
    let pdf =
        Doc::file(2, "Statement.pdf", 9).pages((1..=4).map(|page| format!("invoice line {page}")));
    corpus(vec![(ws(1), "Acme", vec![page, pdf])])
}

fn tasks(text: &str) -> SearchQuery {
    SearchQuery {
        task_results: true,
        ..words(text)
    }
}

/// With tasks asked for, a marked picture is a result of its own: two marked
/// pictures in one document that both match are two rows, not one.
#[test]
fn two_marked_pictures_in_one_document_are_two_read_hits() {
    let index = marked_pictures();
    let page = ask(&index, tasks("invoice"));
    let found = reads(&page);
    let by_task: BTreeSet<&str> = found
        .iter()
        .filter_map(|hit| hit.task.as_ref().map(|task| task.node.as_str()))
        .collect();
    assert_eq!(
        by_task,
        BTreeSet::from(["img-a", "img-b", "scan"]),
        "each marked node keeps its own best read fragment"
    );
    // Several matching pages of one marked file are one row, the best page.
    let scan: Vec<_> = found
        .iter()
        .filter(|hit| hit.node.as_deref() == Some("scan"))
        .collect();
    assert_eq!(scan.len(), 1);
    assert_eq!(scan[0].page, Some(2), "with the winning page's evidence");
    assert_eq!(highlighted(scan[0]), ["invoice"]);
    assert!(scan[0].snippet.contains("totals"));
    // Every unmarked node of the document shares one row.
    let unmarked: Vec<_> = found
        .iter()
        .filter(|hit| hit.task.is_none() && hit.document.as_deref() == Some(id(1).as_str()))
        .collect();
    assert_eq!(unmarked.len(), 1);
    // And an ordinary PDF's pages still collapse to one.
    let pdf: Vec<_> = found
        .iter()
        .filter(|hit| hit.document.as_deref() == Some(id(2).as_str()))
        .collect();
    assert_eq!(pdf.len(), 1);
    assert_eq!(found.len(), 5);
}

/// With a page size of one the second task is on the second page, not lost
/// to a collapse that ran after the cut.
#[test]
fn a_second_marked_picture_is_on_the_next_page_not_lost() {
    let index = marked_pictures();
    let mut seen = BTreeSet::new();
    let mut offset = 0;
    loop {
        let page = ask(
            &index,
            SearchQuery {
                page_size: 1,
                offset,
                ..tasks("invoice")
            },
        );
        for hit in &page.hits {
            if let Some(task) = &hit.task {
                seen.insert(task.node.clone());
            }
        }
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(seen.len(), 3, "{seen:?}");
}

/// Without the flag the collapse is per document, as it was: the marks are
/// there and change nothing.
#[test]
fn without_the_flag_marked_pictures_collapse_per_document() {
    let index = marked_pictures();
    let page = ask(&index, words("invoice"));
    let found = reads(&page);
    assert_eq!(found.len(), 2, "one per document");
    assert!(found.iter().all(|hit| hit.task.is_none()));
}
