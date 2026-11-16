// SPDX-License-Identifier: MPL-2.0

//! Task lists and tasks as search results (nested-lists §7, §12 *C*): what is
//! indexed, who owns which rows, what an upgrade is owed, and that a caller
//! who asked for neither gets the answer it always got.

use std::sync::{Arc, Mutex};

use koru_projection::model::ProjectedAttr;

use super::*;
use crate::lists::{ListNames, ListRow};
use crate::model::{AttrFilter, AttrOp, SearchHit, SearchQuery, SearchSort, field};

/// The host's list table, as a test sets it. `None` is a read that failed.
#[derive(Clone, Default)]
struct FakeLists(Arc<Mutex<Option<Vec<ListRow>>>>);

impl FakeLists {
    fn set(&self, lists: Option<Vec<ListRow>>) {
        *self.0.lock().unwrap() = lists;
    }
}

impl ListNames for FakeLists {
    fn list_names(&self, _workspace: WorkspaceId) -> Option<Vec<ListRow>> {
        self.0.lock().unwrap().clone()
    }
}

fn a_list(id: &str, parent: Option<&str>, name: &str) -> ListRow {
    ListRow {
        id: id.to_owned(),
        parent: parent.map(str::to_owned),
        ordinal: 0,
        name: name.to_owned(),
        color: Some("teal".to_owned()),
    }
}

fn attr(role: &str, kind: &str, value: &str) -> ProjectedAttr {
    ProjectedAttr {
        id: format!("{role}-id"),
        role: role.to_owned(),
        kind: kind.to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    }
}

fn node(id: &str, kind: &str, text: &str, attrs: Vec<ProjectedAttr>) -> ProjectedNode {
    ProjectedNode {
        id: id.to_owned(),
        kind: kind.to_owned(),
        attrs,
        ..a_node(text)
    }
}

fn task(id: &str, text: &str) -> ProjectedNode {
    node(
        id,
        "paragraph",
        text,
        vec![attr("koru.task", "bool", "true")],
    )
}

/// One workspace named *Acme* with a system document, and whatever documents
/// a test adds.
struct World {
    workspace: WorkspaceId,
    system: DocumentId,
    listing: Vec<ListedDocument>,
    lists: FakeLists,
    indexer: Indexer,
    revision: u8,
}

impl World {
    fn new() -> Self {
        let workspace = WorkspaceId::new();
        let system = DocumentId::new();
        let lists = FakeLists::default();
        let indexer = Indexer::new(
            SearchIndex::in_memory().unwrap(),
            Projections::in_memory().unwrap(),
            Queue::default(),
        )
        .with_list_names(Box::new(lists.clone()));
        let mut world = Self {
            workspace,
            system,
            listing: vec![system_entry(system)],
            lists,
            indexer,
            revision: 0,
        };
        world.project(system, Vec::new());
        world.relist();
        world
    }

    fn relist(&mut self) {
        self.revision += 1;
        self.indexer
            .projections
            .set_listing(self.workspace, "Acme", &self.listing, self.revision as i64)
            .unwrap();
    }

    fn project(&mut self, document: DocumentId, nodes: Vec<ProjectedNode>) {
        self.revision += 1;
        let projection = DocumentProjection {
            workspace: self.workspace,
            document,
            source_revision: vec![self.revision],
            nodes,
        };
        self.indexer
            .projections
            .replace(&projection, self.revision as i64, None)
            .unwrap();
    }

    fn page(&mut self, title: &str, nodes: Vec<ProjectedNode>) -> DocumentId {
        let document = DocumentId::new();
        self.listing.push(listed(document, title));
        self.project(document, nodes);
        self.relist();
        document
    }

    /// Run until idle, far enough ahead that a reconcile is due.
    fn settle(&mut self) {
        self.revision += 1;
        settle(
            &mut self.indexer,
            self.revision as i64 * 10 * RECONCILE_MILLIS,
        );
    }

    /// The system document reprojects: what a list edit looks like from here.
    fn lists_changed(&mut self, lists: Vec<ListRow>) {
        self.lists.set(Some(lists));
        self.project(self.system, Vec::new());
        self.settle();
    }

    fn ask(&self, query: SearchQuery) -> Vec<SearchHit> {
        self.indexer
            .index()
            .query(
                &SearchQuery {
                    workspaces: vec![self.workspace],
                    ..query
                },
                &[],
            )
            .unwrap()
            .hits
    }

    fn find(&self, text: &str, lists: bool, tasks: bool) -> Vec<SearchHit> {
        self.ask(SearchQuery {
            text: text.into(),
            include_lists: lists,
            task_results: tasks,
            ..SearchQuery::default()
        })
    }

    fn indexed(&self) -> u64 {
        self.indexer.queue().stats().indexed
    }

    fn count(&self, sql: &str) -> i64 {
        self.indexer
            .index()
            .connection
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }
}

/// Everything about an answer that a caller could see change.
fn shape(hits: &[SearchHit]) -> Vec<(Option<String>, Option<String>, String, String, i64)> {
    hits.iter()
        .map(|hit| {
            (
                hit.document.clone(),
                hit.node.clone(),
                hit.field.clone(),
                hit.snippet.clone(),
                hit.score as i64,
            )
        })
        .collect()
}

// --------------------------------------------------------------------- lists

/// Rung four: typing a list's name finds the list, above everything else, in
/// every sort — and only for a caller that asked.
#[test]
fn a_list_is_a_result_of_its_own_and_leads_every_sort() {
    let mut world = World::new();
    world.page(
        "Backlog grooming notes",
        vec![a_node("the backlog is long")],
    );
    world.settle();
    world.lists_changed(vec![
        a_list("l-top", None, "Top 3"),
        a_list("l-back", Some("l-top"), "Backlog"),
    ]);

    for sort in [
        SearchSort::Relevance,
        SearchSort::ObservedModified,
        SearchSort::LastUsedWorkspace,
    ] {
        let hits = world.ask(SearchQuery {
            text: "backlog".into(),
            include_lists: true,
            sort,
            ..SearchQuery::default()
        });
        let first = &hits[0];
        assert_eq!(first.kind, "task_list", "{sort:?}");
        assert_eq!(first.title, "Backlog");
        assert_eq!(first.document, None);
        let list = first.list.as_ref().expect("a list hit says which list");
        assert_eq!(list.id, "l-back");
        assert_eq!(list.color.as_deref(), Some("teal"));
        assert_eq!(
            list.path,
            vec!["Top 3".to_owned()],
            "its path, from the mirror"
        );
        assert_eq!(first.breadcrumb, "Acme / Top 3 / Backlog");
        assert!(
            hits[1..].iter().all(|hit| hit.list.is_none()),
            "and the documents follow"
        );
        assert!(hits.len() >= 2);
    }

    let unasked = world.find("backlog", false, false);
    assert!(!unasked.is_empty());
    assert!(
        unasked
            .iter()
            .all(|hit| hit.list.is_none() && hit.field != field::TASK_LIST),
        "a caller that did not ask sees no list"
    );
}

/// An index that was caught up before any of this existed is not re-read, so
/// nothing but the bootstrap would ever index its lists.
#[test]
fn an_index_already_caught_up_gains_its_lists_with_no_edit() {
    let mut world = World::new();
    world.page("Roadmap", vec![a_node("plans")]);
    // The read fails at first: the store has only just started.
    world.lists.set(None);
    world.settle();
    assert!(world.find("backlog", true, false).is_empty());
    let before = world.indexed();

    // Nothing is edited. The source can answer now, and a reconcile comes.
    world
        .lists
        .set(Some(vec![a_list("l-back", None, "Backlog")]));
    world.settle();
    assert_eq!(world.find("backlog", true, false).len(), 1);
    assert_eq!(
        world.indexed(),
        before,
        "and no document was re-read for it"
    );
}

/// **A read that fails is not an empty table.**
#[test]
fn a_failed_read_keeps_the_lists_already_indexed() {
    let mut world = World::new();
    world.settle();
    world.lists_changed(vec![a_list("l-back", None, "Backlog")]);
    assert_eq!(world.find("backlog", true, false).len(), 1);

    world.lists.set(None);
    world.project(world.system, Vec::new());
    world.settle();
    assert_eq!(
        world.find("backlog", true, false).len(),
        1,
        "the rows it had stay"
    );

    world.lists_changed(vec![a_list("l-back", None, "Sprint")]);
    assert!(world.find("backlog", true, false).is_empty());
    assert_eq!(
        world.find("sprint", true, false).len(),
        1,
        "and it recovers"
    );
}

/// Renaming, nesting or deleting a list re-indexes no other document: only
/// the system document itself, which is what changed.
#[test]
fn a_list_change_re_indexes_no_other_document() {
    let mut world = World::new();
    world.page("Roadmap", vec![a_node("plans")]);
    world.page("Notes", vec![a_node("more plans")]);
    world.settle();
    world.lists_changed(vec![
        a_list("l-top", None, "Top 3"),
        a_list("l-back", None, "Backlog"),
    ]);
    let before = world.indexed();

    world.lists_changed(vec![
        a_list("l-top", None, "Top 5"),
        a_list("l-back", Some("l-top"), "Backlog"),
    ]);
    assert_eq!(world.indexed(), before + 1, "the system document alone");
    let hit = &world.find("backlog", true, false)[0];
    assert_eq!(hit.list.as_ref().unwrap().path, vec!["Top 5".to_owned()]);

    world.lists_changed(vec![a_list("l-top", None, "Top 5")]);
    assert_eq!(world.indexed(), before + 2);
    assert!(world.find("backlog", true, false).is_empty(), "deleted");
    assert_eq!(world.count("SELECT COUNT(*) FROM task_lists"), 1);
}

/// The listing path owns the workspace's name and the titles, by key. It used
/// to own every fragment with no document, which is what a list's is.
#[test]
fn list_hits_survive_a_listing_refresh() {
    let mut world = World::new();
    let page = world.page("Roadmap", vec![a_node("plans")]);
    world.settle();
    world.lists_changed(vec![a_list("l-back", None, "Backlog")]);

    // A document is renamed: the listing is rewritten.
    for entry in &mut world.listing {
        if entry.document == page {
            entry.title = "Roadmap 2027".to_owned();
        }
    }
    world.relist();
    world.settle();
    assert_eq!(
        world.find("2027", false, false).len(),
        1,
        "the rename landed"
    );
    assert_eq!(
        world.find("backlog", true, false).len(),
        1,
        "the list is still there"
    );
    assert_eq!(
        world.find("acme", false, false).len(),
        1,
        "and so is the workspace"
    );
}

/// **A list's name is never anybody else's evidence.** *Backlog budget* must
/// not match a paragraph holding only *budget* because a list is named
/// *Backlog* — with either flag.
#[test]
fn a_lists_name_lends_its_words_to_nothing_else() {
    let mut world = World::new();
    world.page("Finance", vec![a_node("the budget is tight")]);
    world.settle();
    world.lists_changed(vec![a_list("l-back", None, "Backlog")]);

    for lists in [false, true] {
        let hits = world.find("backlog budget", lists, false);
        assert!(
            hits.iter().all(|hit| hit.document.is_none()),
            "lists={lists}: {:?}",
            shape(&hits)
        );
    }
    // The workspace's own name still explains a term, as it always did.
    assert_eq!(world.find("acme budget", false, false).len(), 1);
}

/// Two lists are two results with their own highlights: a list's id is its
/// fragment's node, so their evidence never mixes.
#[test]
fn two_matching_lists_are_two_rows_with_their_own_highlights() {
    let mut world = World::new();
    world.settle();
    world.lists_changed(vec![
        a_list("l-a", None, "Plan alpha"),
        a_list("l-b", None, "Beta plan"),
    ]);
    let hits = world.find("plan", true, false);
    assert_eq!(hits.len(), 2);
    for hit in &hits {
        assert_eq!(hit.snippet, hit.title);
        let span = &hit.spans[0];
        assert_eq!(
            hit.snippet[span.start..span.end].to_lowercase(),
            "plan",
            "{}",
            hit.title
        );
    }
    assert_ne!(hits[0].spans[0].start, hits[1].spans[0].start);
}

#[test]
fn removing_a_workspace_removes_its_lists_and_their_words() {
    let mut world = World::new();
    world.settle();
    world.lists_changed(vec![a_list("l-back", None, "Backlog")]);
    world.indexer.index().warm_fuzzy_vocabulary().unwrap();
    assert_eq!(
        world.find("backlgo", true, false).len(),
        1,
        "a typo finds it"
    );

    let workspace = world.workspace.to_storage();
    world.indexer.index.remove_workspace(&workspace).unwrap();
    assert_eq!(world.count("SELECT COUNT(*) FROM task_lists"), 0);
    assert_eq!(
        world.count("SELECT COUNT(*) FROM fragments WHERE field = 'task-list'"),
        0
    );
    let stats = world.indexer.index().fuzzy_stats();
    assert_eq!(stats.active_terms, 0, "no word of it is left active");
}

/// **Typo tolerance for a caller that did not ask for lists is what it was.**
/// A document holds *cook*; the query is *book*. Eight lists are then named
/// with one-edit neighbours of *book* that sort ahead of *cook* — enough to
/// fill the eight alternatives on their own — and the answer does not move.
#[test]
fn list_names_do_not_change_anyone_elses_typo_corrections() {
    let mut world = World::new();
    world.page("Kitchen", vec![a_node("cook")]);
    world.settle();
    world.indexer.index().warm_fuzzy_vocabulary().unwrap();
    let before = shape(&world.find("book", false, false));
    assert_eq!(before.len(), 1, "cook, corrected from book");

    let neighbours = [
        "bock", "bonk", "boog", "boom", "boon", "boor", "boos", "boot",
    ];
    let lists: Vec<ListRow> = neighbours
        .iter()
        .map(|name| a_list(&format!("l-{name}"), None, name))
        .collect();
    world.lists_changed(lists.clone());
    assert_eq!(shape(&world.find("book", false, false)), before);

    // ...after a restart (warming), a rename, a delete and a document edit.
    world.indexer.index().warm_fuzzy_vocabulary().unwrap();
    assert_eq!(shape(&world.find("book", false, false)), before, "warmed");
    let mut renamed = lists.clone();
    renamed[0].name = "cook".to_owned();
    world.lists_changed(renamed);
    assert_eq!(shape(&world.find("book", false, false)), before, "renamed");
    world.lists_changed(lists[1..].to_vec());
    assert_eq!(shape(&world.find("book", false, false)), before, "deleted");
    world.page("Other", vec![a_node("unrelated words")]);
    world.settle();
    assert_eq!(shape(&world.find("book", false, false)), before, "edited");

    // With lists asked for, the lists' words are candidates.
    let with_lists = world.find("book", true, false);
    assert!(with_lists.iter().any(|hit| hit.list.is_some()));
}

// --------------------------------------------------------------------- tasks

/// Rung five: each matching task is its own result and says which node it is.
#[test]
fn two_tasks_in_one_document_are_two_task_hits() {
    let mut world = World::new();
    world.page(
        "Sprint",
        vec![
            task("t1", "implement new design"),
            task("t2", "review new design"),
            node("p1", "paragraph", "the new design is bold", Vec::new()),
        ],
    );
    world.settle();

    let hits = world.find("design", false, true);
    let tasks: Vec<&str> = hits
        .iter()
        .filter_map(|hit| hit.task.as_ref().map(|task| task.node.as_str()))
        .collect();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.contains(&"t1") && tasks.contains(&"t2"));
    let plain: Vec<&SearchHit> = hits.iter().filter(|hit| hit.task.is_none()).collect();
    assert_eq!(plain.len(), 1, "and the paragraph is an ordinary hit");
    for hit in hits.iter().filter(|hit| hit.task.is_some()) {
        assert_eq!(hit.task.as_ref().unwrap().state, "open");
        assert!(hit.title.contains("new design"), "titled by its own text");
        assert!(
            hit.score > plain[0].score,
            "a task's own text ranks like a title"
        );
    }
    // Unasked: the same three fragments, no task named, the old weights.
    let unasked = world.find("design", false, false);
    assert_eq!(unasked.len(), 3);
    assert!(unasked.iter().all(|hit| hit.task.is_none()));
    let scores: Vec<i64> = unasked.iter().map(|hit| hit.score as i64).collect();
    assert!(
        scores.windows(2).all(|pair| pair[0] == pair[1]),
        "{scores:?}"
    );
}

/// A completed task stays searchable and says it is completed.
#[test]
fn a_completed_task_is_a_task_hit_that_says_so() {
    let mut world = World::new();
    world.page(
        "Sprint",
        vec![node(
            "t1",
            "paragraph",
            "ship the release",
            vec![
                attr("koru.task", "bool", "true"),
                attr("koru.task_state", "text", "completed"),
            ],
        )],
    );
    world.settle();
    let hits = world.find("release", false, true);
    assert_eq!(hits[0].task.as_ref().unwrap().state, "completed");
}

/// A task matching on its text and two attributes is one task, three times:
/// every fragment names the same node, which is what a surface groups by.
#[test]
fn every_fragment_of_one_task_names_the_same_task() {
    let mut world = World::new();
    world.page(
        "Sprint",
        vec![node(
            "t1",
            "paragraph",
            "urgent fix",
            vec![
                attr("koru.task", "bool", "true"),
                attr("custom.note", "text", "urgent"),
                attr("custom.tier", "text", "urgent"),
            ],
        )],
    );
    world.settle();
    let hits = world.find("urgent", false, true);
    assert_eq!(hits.len(), 3);
    assert!(
        hits.iter()
            .all(|hit| hit.task.as_ref().map(|t| t.node.as_str()) == Some("t1"))
    );
    assert_eq!(hits[0].field, field::BODY, "its own text first");
}

/// A page that is itself a task. Its root's text equals its title, so the
/// index holds no fragment naming the root node — only the title, which names
/// none. The generated row is what lets the title say it is a task.
#[test]
fn a_page_that_is_a_task_is_a_task_hit_through_its_title() {
    let mut world = World::new();
    let page = world.page(
        "Plan the offsite",
        vec![
            node(
                "root",
                "document",
                "Plan the offsite",
                vec![attr("koru.due", "date", "2026-12-01")],
            ),
            a_node("book a venue"),
        ],
    );
    world.settle();

    // With a page size of one, the first answer is already the task.
    let hits = world.ask(SearchQuery {
        text: "offsite".into(),
        task_results: true,
        page_size: 1,
        ..SearchQuery::default()
    });
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].field, field::TITLE);
    assert_eq!(
        hits[0].node, None,
        "the hit's own node is what it always was"
    );
    assert_eq!(hits[0].task.as_ref().unwrap().node, "root");
    assert_eq!(hits[0].title, "Plan the offsite");

    // Exactly one generated row, however often the page is walked.
    world.project(
        page,
        vec![
            node(
                "root",
                "document",
                "Plan the offsite",
                vec![attr("koru.due", "date", "2026-12-02")],
            ),
            a_node("book a venue"),
        ],
    );
    world.settle();
    assert_eq!(
        world.count("SELECT COUNT(*) FROM attr_values WHERE kind = '@root-task'"),
        1
    );

    // Unmarking the root takes the row away with everything else.
    world.project(
        page,
        vec![
            node("root", "document", "Plan the offsite", Vec::new()),
            a_node("book a venue"),
        ],
    );
    world.settle();
    assert_eq!(
        world.count("SELECT COUNT(*) FROM attr_values WHERE kind = '@root-task'"),
        0
    );
    assert!(world.find("offsite", false, true)[0].task.is_none());
}

/// The root's text differs from the title: today's index has a root fragment
/// too. Both are the same task, whichever page each lands on.
#[test]
fn a_page_task_whose_text_differs_from_its_title_is_one_task_on_two_rows() {
    let mut world = World::new();
    world.page(
        "Offsite",
        vec![node(
            "root",
            "document",
            "Offsite planning",
            vec![attr("koru.task", "bool", "true")],
        )],
    );
    world.settle();
    let hits = world.find("offsite", false, true);
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .all(|hit| hit.task.as_ref().map(|t| t.node.as_str()) == Some("root"))
    );
    assert_eq!(hits.iter().filter(|hit| hit.node.is_none()).count(), 1);

    // Unasked: the same two fragments in the same order, and no task.
    let unasked = world.find("offsite", false, false);
    assert_eq!(
        unasked.iter().map(|h| h.field.clone()).collect::<Vec<_>>(),
        hits.iter().map(|h| h.field.clone()).collect::<Vec<_>>()
    );
    assert!(unasked.iter().all(|hit| hit.task.is_none()));
}

/// **A generated row cannot be mistaken for an authored one, in either
/// direction.** Roles are not reserved: a workspace may define an attribute
/// spelled `@root-task`.
#[test]
fn an_authored_attribute_spelled_like_the_marker_is_neither_read_nor_removed() {
    let mut world = World::new();
    let page = world.page(
        "Glossary",
        vec![
            node("root", "document", "Glossary", Vec::new()),
            node(
                "p1",
                "paragraph",
                "glossary entry",
                vec![attr("@root-task", "text", "yes")],
            ),
        ],
    );
    let task_page = world.page(
        "Glossary review",
        vec![node(
            "root2",
            "document",
            "Glossary review",
            vec![attr("koru.task", "bool", "true")],
        )],
    );
    world.settle();

    let hits = world.find("glossary", false, true);
    let title = |document: DocumentId| {
        hits.iter()
            .find(|hit| hit.field == field::TITLE && hit.document == Some(document.to_storage()))
            .unwrap()
    };
    assert!(title(page).task.is_none(), "the role alone marks nothing");
    assert!(title(task_page).task.is_some());
    assert_eq!(
        hits.iter().filter(|hit| hit.field == field::TITLE).count(),
        2,
        "no title is returned twice"
    );
    assert!(
        !world
            .indexer
            .index()
            .has_root_task_marker(&page.to_storage())
            .unwrap(),
        "nor does it stand in for the marker"
    );
    assert_eq!(
        world.count("SELECT COUNT(*) FROM attr_values WHERE role = '@root-task' AND kind = 'text'"),
        1,
        "and the authored row is still there after the walk"
    );

    // A typed filter naming the role sees the authored row and never the
    // generated one, with either flag.
    let filter = |op: AttrOp, kind: &str| AttrFilter {
        key: "@root-task".to_owned(),
        op,
        values: Vec::new(),
        numeric_values: Vec::new(),
        kinds: vec![kind.to_owned()],
    };
    for tasks in [false, true] {
        let has = |op: AttrOp, kind: &str| {
            world.ask(SearchQuery {
                text: "glossary".into(),
                attributes: vec![filter(op, kind)],
                task_results: tasks,
                ..SearchQuery::default()
            })
        };
        let authored = has(AttrOp::Exists, "text");
        assert_eq!(authored.len(), 1, "tasks={tasks}");
        assert_eq!(authored[0].node.as_deref(), Some("p1"));
        assert!(
            has(AttrOp::Exists, "@root-task").is_empty(),
            "tasks={tasks}: a filter naming the generated kind matches nothing"
        );
    }
}

/// An index caught up before the marker existed has none, and an unchanged
/// page is not re-read. The bootstrap queues exactly the pages that are tasks
/// and are not yet marked.
#[test]
fn only_unmarked_page_tasks_are_re_walked_after_an_upgrade() {
    let mut world = World::new();
    world.lists.set(Some(Vec::new()));
    world.page(
        "Offsite",
        vec![node(
            "root",
            "document",
            "Offsite",
            vec![attr("koru.task", "bool", "true")],
        )],
    );
    world.page("Notes", vec![a_node("offsite notes")]);
    world.page("Sprint", vec![task("t1", "offsite prep")]);
    world.settle();

    // What an older build left: no marker, every cursor current.
    world
        .indexer
        .index()
        .connection
        .execute("DELETE FROM attr_values WHERE kind = '@root-task'", [])
        .unwrap();
    assert!(
        world
            .find("offsite", false, true)
            .iter()
            .filter(|hit| hit.field == field::TITLE)
            .all(|hit| hit.task.is_none())
    );

    // A new process: it has bootstrapped nothing yet.
    world.indexer.bootstrapped.clear();
    let before = world.indexed();
    world.settle();
    assert_eq!(world.indexed(), before + 1, "the one page that is a task");
    let hits = world.find("offsite", false, true);
    let title = hits
        .iter()
        .find(|hit| hit.field == field::TITLE && hit.title == "Offsite")
        .unwrap();
    assert_eq!(title.task.as_ref().unwrap().node, "root");

    // And it is done once: a later reconcile re-walks nothing.
    world.settle();
    assert_eq!(world.indexed(), before + 1);
}

/// **A caller that asked for neither gets what it got before**, order
/// included, in a workspace that has lists, page-tasks and tasks.
#[test]
fn with_neither_flag_the_answer_is_what_it_was() {
    let build = |with_lists: bool| {
        let mut world = World::new();
        world.page(
            "Offsite",
            vec![
                node(
                    "root",
                    "document",
                    "Offsite planning",
                    vec![attr("koru.task", "bool", "true")],
                ),
                task("t1", "book the offsite venue"),
                node("p1", "paragraph", "offsite budget", Vec::new()),
            ],
        );
        world.page(
            "Offsite recap",
            vec![node("r", "document", "Offsite recap", Vec::new())],
        );
        world.settle();
        if with_lists {
            world.lists_changed(vec![
                a_list("l-off", None, "Offsite"),
                a_list("l-only", None, "Quarterly"),
            ]);
        }
        world.indexer.index().warm_fuzzy_vocabulary().unwrap();
        world
    };
    let (plain, listed) = (build(false), build(true));
    // Ids differ between the two worlds; fields, text and scores do not.
    let seen = |world: &World, text: &str| -> Vec<(String, String, i64)> {
        world
            .find(text, false, false)
            .iter()
            .map(|hit| (hit.field.clone(), hit.snippet.clone(), hit.score as i64))
            .collect()
    };
    for text in [
        "offsite",
        "ofsite",
        "offsite budget",
        "quarterly",
        "quarterlly",
    ] {
        assert_eq!(seen(&listed, text), seen(&plain, text), "{text}");
    }
    assert!(
        seen(&listed, "quarterly").is_empty(),
        "a word only a list holds finds nothing"
    );
}
