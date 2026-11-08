// SPDX-License-Identifier: MPL-2.0

//! The workspace layer: what it supplies that `koru-core` deliberately lacks.

use super::*;
use koru_core::WorkspaceId;

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap()
}

/// The resolution order, and the reason for each step: a name if the person gave
/// one, otherwise the part of their email that reads like a name, otherwise the
/// replica handle they typed on the command line.
#[test]
fn actor_label_prefers_the_profile_name_then_email_then_the_peer_slot() {
    assert_eq!(
        actor_label(Some("Sam Okafor"), Some("sam@example.com"), Some("A")),
        Some("Sam Okafor".into())
    );
    assert_eq!(
        actor_label(None, Some("sam@example.com"), Some("A")),
        Some("sam".into()),
        "the local part, not the whole address"
    );
    assert_eq!(actor_label(None, None, Some("A")), Some("A".into()));
}

/// Whitespace and empty strings are not names. A profile with a blank name field
/// must fall through rather than labelling everything `""`.
#[test]
fn blank_is_not_a_name() {
    assert_eq!(
        actor_label(Some("   "), Some("sam@example.com"), None),
        Some("sam".into())
    );
    assert_eq!(actor_label(Some(""), Some(""), Some("A")), Some("A".into()));
    assert_eq!(
        actor_label(Some("@nope.com"), None, None),
        Some("@nope.com".into())
    );
}

/// **`None` is a supported answer**, not a failure. An unnamed profile with no
/// `--peer` has nothing to say, and every message built from this has a form
/// with no "by" clause — inventing one would be worse than saying nothing.
#[test]
fn an_unnamed_profile_with_no_peer_flag_has_no_label() {
    assert_eq!(actor_label(None, None, None), None);
    assert_eq!(actor_label(Some(""), None, Some("  ")), None);
}

/// An in-memory workspace — a test, or one opened with no storage — has no
/// session and therefore no peer to fall back on.
#[test]
fn an_in_memory_workspace_has_no_actor_until_one_is_set() {
    let mut w = Workspace::new(doc());
    assert_eq!(w.actor_label(), None);

    w.set_actor(Some("Sam".into()));
    assert_eq!(w.actor_label().as_deref(), Some("Sam"));

    w.set_actor(Some("   ".into()));
    assert_eq!(w.actor_label(), None, "blank clears rather than labels");
}

/// A tab can remain selected while Explore shows another workspace. Its live
/// Store session is the routing authority for blobs; the document stamp may
/// still name the workspace the document occupied before a move.
#[test]
fn the_active_document_uses_its_store_sessions_workspace() {
    let stale = WorkspaceId::new();
    let home = WorkspaceId::new();
    let document = koru_core::DocumentId::new();
    let mut workspace =
        Workspace::holding_for_tests(WorkspaceDoc::create(stale, "moved").unwrap(), document);
    let (session, _link) = store::StoreSession::for_system_test(
        explore::store_workspace(home),
        explore::store_document(document),
    );
    workspace.doc_sessions.insert(document, session);

    assert_eq!(workspace.active_workspace(), Some(home));
}

/// The wrappers exist to supply the clock and the label; this is the proof that
/// they actually reach the document.
#[test]
fn trashing_through_the_workspace_records_the_actor() {
    let mut w = Workspace::new(doc());
    w.set_actor(Some("Sam".into()));
    let d =
        w.ws.insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();

    w.trash(d).unwrap();
    assert!(w.ws.in_trash(d));
    assert_eq!(w.ws.trashed_by(d).as_deref(), Some("Sam"));
    assert!(
        w.ws.trashed().unwrap()[0].trashed_at.is_some(),
        "and the clock reached it too"
    );

    w.restore(d).unwrap();
    assert!(!w.ws.in_trash(d));
    assert_eq!(w.ws.restored_by(d).as_deref(), Some("Sam"));
}

/// Find-or-create must not resurrect something a person threw away: asking for
/// "Spec" when the only Spec is in the bin makes a **new** one.
#[test]
fn document_does_not_resurrect_a_trashed_document() {
    let mut ws = doc();
    let first = document(&mut ws, "Spec").unwrap();
    ws.trash(first, 1, None).unwrap();

    let second = document(&mut ws, "Spec").unwrap();
    assert_ne!(second, first, "a new document, not the one in the bin");
    assert!(ws.in_trash(first));
    assert!(!ws.in_trash(second));
}

#[test]
fn create_document_after_lands_as_next_library_sibling() {
    let mut ws = doc();
    let a = create_document(&mut ws, "A").unwrap();
    let b = create_document(&mut ws, "B").unwrap();
    let mid = create_document_after(&mut ws, "Mid", Some(a)).unwrap();
    let kids = ws.children(Parent::Root).unwrap();
    let ia = kids.iter().position(|&id| id == a).unwrap();
    let imid = kids.iter().position(|&id| id == mid).unwrap();
    let ib = kids.iter().position(|&id| id == b).unwrap();
    assert_eq!(imid, ia + 1, "Mid sits directly under A");
    assert!(ib > imid, "B stays after Mid");
}

// ------------------------------------------------- several documents

/// `entries()` answers from the tree while there is no manifest, and the answer
/// is the one Explore already draws — so the readers can be written against it
/// before the manifest exists.
#[test]
fn entries_come_from_the_tree_until_there_is_a_manifest() {
    let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    let a = ws
        .insert(Parent::Root, Node::document("A").unwrap())
        .unwrap();
    ws.insert(Parent::Node(a), Node::document("A1").unwrap())
        .unwrap();
    let w = Workspace::new(ws);

    let titles: Vec<String> = w.entries().into_iter().map(|e| e.title).collect();
    assert_eq!(titles, ["A", "A1"], "tree order, parents included");
    assert_eq!(w.entries()[1].parent, Some(w.entries()[0].id));
    let (entries, trashed) = w.entries_and_trashed();
    assert_eq!(entries, w.entries());
    assert!(trashed.is_empty());
}

/// One document open means `documents()` yields exactly it — the shape every
/// workspace has today, and the one the walk has to keep answering correctly
/// when there are more.
#[test]
fn documents_yields_the_active_one() {
    let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    ws.insert(Parent::Root, Node::document("Only").unwrap())
        .unwrap();
    let w = Workspace::new(ws);
    assert_eq!(w.documents().count(), 1);
    assert_eq!(w.active(), None, "nothing to tell apart yet");
}

/// **Blobs are gathered across every open document, and deduplicated.**
///
/// The same picture pasted into two documents is one blob to decode, not two —
/// and once documents are separate `LoroDoc`s this is the only way the shell can
/// still ask "what still needs decoding" without walking one giant tree.
#[test]
fn blob_hashes_span_every_open_document_without_repeating() {
    use koru_core::node::BlobHash;
    let mut ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("Pictures").unwrap())
        .unwrap();
    for _ in 0..2 {
        ws.insert(
            Parent::Node(doc),
            Node::image(BlobHash("b3:abc".into()), "image/png", None, None).unwrap(),
        )
        .unwrap();
    }
    ws.insert(
        Parent::Node(doc),
        Node::image(BlobHash("b3:def".into()), "image/png", None, None).unwrap(),
    )
    .unwrap();

    let w = Workspace::new(ws);
    assert_eq!(
        w.blob_hashes(),
        vec!["b3:abc".to_string(), "b3:def".to_string()],
        "twice referenced is once decoded"
    );
}

/// The workspace's **tasks document** (`"task-list"`, SPEC §8 and ADR-272's
/// 2026-09-01 amendment) is hidden by the same predicate as the system
/// document: the manifest still carries it — `task_list_documents` finds it
/// there, which is how the next capture knows where to go — and the listing a
/// person sees does not, because those tasks are seen through the Tasks view.
#[test]
fn the_tasks_document_is_filtered_out_of_the_listing() {
    let ws = koru_core::WorkspaceId::new();
    let mut manifest = koru_store_core::manifest::Manifest::create(
        explore::store_workspace(ws),
        "t",
        koru_store_core::model::Timestamp::from_millis(1),
    )
    .unwrap();
    let page = koru_core::DocumentId::new();
    let tasks = koru_core::DocumentId::new();
    manifest
        .add_document(
            explore::store_document(page),
            "A page",
            koru_core::explore::KIND_DOCUMENT,
            None,
        )
        .unwrap();
    manifest
        .add_document(
            explore::store_document(tasks),
            crate::tasks::TASK_LIST_TITLE,
            crate::tasks::KIND_TASK_LIST,
            None,
        )
        .unwrap();

    let mut w = Workspace::new(doc());
    w.manifest = Some(manifest);

    let (entries, trashed) = w.entries_and_trashed();
    assert_eq!(
        entries.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![page],
        "no `Tasks` row in Explore"
    );
    assert_eq!(entries[0].ordinal, 0, "and no hole where it was");
    assert!(trashed.is_empty());
    assert_eq!(
        w.task_list_documents(),
        vec![tasks],
        "still found where it is looked for: the manifest, by kind"
    );
}

/// The workspace system document (`"koru-system"`, ADR-176) is store-listed
/// but never person-listed: the manifest carries it like any entry, and the
/// listing a person sees filters it out — with the surviving siblings
/// renumbered, because `ordinal` promises positions among what the caller can
/// see.
#[test]
fn the_system_document_is_filtered_out_of_the_listing() {
    let ws = koru_core::WorkspaceId::new();
    let mut manifest = koru_store_core::manifest::Manifest::create(
        explore::store_workspace(ws),
        "t",
        koru_store_core::model::Timestamp::from_millis(1),
    )
    .unwrap();
    let a = koru_core::DocumentId::new();
    let system = koru_core::DocumentId::new();
    let b = koru_core::DocumentId::new();
    manifest
        .add_document(
            explore::store_document(a),
            "A",
            koru_core::explore::KIND_DOCUMENT,
            None,
        )
        .unwrap();
    manifest
        .add_document(
            explore::store_document(system),
            "",
            koru_core::explore::KIND_SYSTEM,
            None,
        )
        .unwrap();
    manifest
        .add_document(
            explore::store_document(b),
            "B",
            koru_core::explore::KIND_DOCUMENT,
            None,
        )
        .unwrap();

    let mut w = Workspace::new(doc());
    w.manifest = Some(manifest);

    let (entries, trashed) = w.entries_and_trashed();
    let ids: Vec<_> = entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![a, b], "the system document earns no row");
    assert_eq!(
        entries.iter().map(|e| e.ordinal).collect::<Vec<_>>(),
        vec![0, 1],
        "and leaves no hole in the ordinals"
    );
    assert!(trashed.is_empty());
}

/// A selection selects list *items* and never their invisible container, so a
/// moved bullet arrived at the destination as a plain paragraph — the first
/// field report against moving a selection. The style travels beside each
/// cutting now, and consecutive items sharing one are planted under a fresh
/// `ListBlock` wearing it, first in the document, in their original order.
#[test]
fn planting_list_items_rebuilds_their_list_at_the_top() {
    use koru_core::schema::Parent;

    let mut source = doc();
    let list = source
        .insert(Parent::Root, koru_core::node::Node::list_block().unwrap())
        .unwrap();
    source
        .set_list_style(list, Some(koru_core::ListStyle::Bullet))
        .unwrap();
    let mut cuttings = Vec::new();
    for text in ["one", "two"] {
        let item = source
            .insert(
                Parent::Node(list),
                koru_core::node::Node::paragraph(text).unwrap(),
            )
            .unwrap();
        cuttings.push((
            source.cutting(item).unwrap(),
            Some(koru_core::ListStyle::Bullet),
        ));
    }
    let loose = source
        .insert(Parent::Root, koru_core::node::Node::task("loose").unwrap())
        .unwrap();
    cuttings.push((source.cutting(loose).unwrap(), None));

    let mut target = doc();
    let root = target
        .insert(Parent::Root, koru_core::node::Node::document("T").unwrap())
        .unwrap();
    target
        .insert(
            Parent::Node(root),
            koru_core::node::Node::paragraph("existing").unwrap(),
        )
        .unwrap();

    plant_at_top(&mut target, None, &cuttings, None).unwrap();

    let children = target.children(Parent::Node(root)).unwrap();
    assert_eq!(children.len(), 3, "list, loose row, existing paragraph");
    let planted_list = target.read(children[0]).unwrap();
    assert_eq!(planted_list.kind, koru_core::Kind::ListBlock);
    assert_eq!(
        planted_list.meta.list_style,
        Some(koru_core::ListStyle::Bullet),
        "the items' bullet style survived the move"
    );
    let items: Vec<String> = target
        .children(Parent::Node(children[0]))
        .unwrap()
        .into_iter()
        .map(|id| {
            target
                .read(id)
                .unwrap()
                .content
                .as_text()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(items, ["one", "two"], "in their original order");
    assert_eq!(
        target.read(children[1]).unwrap().content.as_text().unwrap(),
        "loose",
        "a root outside any list is planted bare, after the list"
    );
}

/// **A store-made document is seeded by its first open, not by its creation.**
/// The store registers a note — like any document — with nothing inside it
/// (ADR-80), so the open path is where the root node and its paragraph appear,
/// titled as the manifest already knows it.
#[test]
fn an_empty_registered_document_is_seeded_from_the_manifest() {
    use koru_store_core::manifest::Manifest;
    use koru_store_core::model::Timestamp;

    let at = Timestamp::from_millis(1_700_000_000_000);
    let mut w = Workspace::new(doc());
    let mut manifest = Manifest::create(
        explore::store_workspace(koru_core::WorkspaceId::new()),
        "Acme",
        at,
    )
    .unwrap();
    let note = koru_core::DocumentId::new();
    manifest
        .add_document(explore::store_document(note), "Report.pdf", "note", None)
        .unwrap();
    let file = koru_core::DocumentId::new();
    manifest
        .add_document(explore::store_document(file), "Report.pdf", "file", None)
        .unwrap();
    w.manifest = Some(manifest);

    // The note: an empty document gains the root and its one paragraph, titled
    // as the manifest says, and a second pass seeds nothing.
    let mut opened = doc();
    assert!(w.seed_empty_document(note, &mut opened));
    let root = document_root(&opened).expect("a seeded document has a root");
    assert_eq!(
        opened.read(root).unwrap().content.as_text(),
        Some("Report.pdf"),
        "the manifest's title, not a guessed one"
    );
    assert_eq!(opened.children(Parent::Node(root)).unwrap().len(), 1);
    assert!(!w.seed_empty_document(note, &mut opened), "never twice");

    // The file: an empty one is damaged, not unseeded — inventing a page root
    // for it would hide that.
    let mut broken = doc();
    assert!(!w.seed_empty_document(file, &mut broken));
    assert!(document_root(&broken).is_none());

    // And a document the manifest does not name is left alone rather than
    // titled from nothing.
    let mut stranger = doc();
    assert!(!w.seed_empty_document(koru_core::DocumentId::new(), &mut stranger));
    assert!(document_root(&stranger).is_none());
}

/// Two devices can each seed the same note before either's seed has
/// replicated — the manifest entry and the document sync separately, and
/// there is no mergeable `LoroTree::create`. Whoever next opens the merged
/// note must fold it back to one root (the first in order, the one every
/// reader already shows) rather than leave a second root's content invisible.
#[test]
fn a_doubly_seeded_note_is_folded_back_to_one_root_on_open() {
    use koru_store_core::manifest::Manifest;
    use koru_store_core::model::Timestamp;

    let mut w = Workspace::new(doc());
    let mut manifest = Manifest::create(
        explore::store_workspace(koru_core::WorkspaceId::new()),
        "Acme",
        Timestamp::from_millis(1_700_000_000_000),
    )
    .unwrap();
    let note = koru_core::DocumentId::new();
    manifest
        .add_document(explore::store_document(note), "Report.pdf", "note", None)
        .unwrap();
    w.manifest = Some(manifest);

    // The store's empty registration, forked onto two devices; each seeds it.
    let empty = loro::LoroDoc::new()
        .export(loro::ExportMode::snapshot())
        .unwrap();
    let mut a = WorkspaceDoc::from_snapshot(&empty).unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&empty).unwrap();
    assert!(w.seed_empty_document(note, &mut a));
    assert!(w.seed_empty_document(note, &mut b));

    // The seeds meet: Loro keeps both, because it cannot do anything else.
    a.import(&b.export_all_updates().unwrap()).unwrap();
    assert_eq!(
        a.children(Parent::Root).unwrap().len(),
        2,
        "the race duplicated the document root"
    );

    // Opening that state folds it back to the one root readers already show,
    // keeps both seeds' content, and reports the write so it is published.
    assert!(w.seed_empty_document(note, &mut a));
    let roots = a.children(Parent::Root).unwrap();
    assert_eq!(roots.len(), 1, "one root again");
    assert_eq!(
        a.read(roots[0]).unwrap().content.as_text(),
        Some("Report.pdf")
    );
    assert_eq!(
        a.children(Parent::Node(roots[0])).unwrap().len(),
        2,
        "both seed paragraphs survived the fold"
    );
    assert!(
        !w.seed_empty_document(note, &mut a),
        "a healthy note is left alone"
    );
}

// ------------------------------------------------------------------ canvases

/// A manifest with one page and one canvas, and the workspace holding it.
fn with_a_page_and_a_canvas() -> (Workspace, koru_core::DocumentId, koru_core::DocumentId) {
    use koru_store_core::manifest::Manifest;
    use koru_store_core::model::Timestamp;

    let mut manifest = Manifest::create(
        explore::store_workspace(koru_core::WorkspaceId::new()),
        "Acme",
        Timestamp::from_millis(1_700_000_000_000),
    )
    .unwrap();
    let page = koru_core::DocumentId::new();
    let canvas = koru_core::DocumentId::new();
    manifest
        .add_document(
            explore::store_document(page),
            "Brief",
            koru_core::explore::KIND_DOCUMENT,
            None,
        )
        .unwrap();
    manifest
        .add_document(
            explore::store_document(canvas),
            "Board",
            koru_core::explore::KIND_CANVAS,
            None,
        )
        .unwrap();
    let mut w = Workspace::new(doc());
    w.manifest = Some(manifest);
    (w, page, canvas)
}

/// **A canvas is seeded with its root and nothing under it.** Its body is
/// what is put on the surface; an empty paragraph would be a card nobody made.
/// A page keeps the paragraph its caret lands in.
#[test]
fn a_canvas_seed_is_its_root_alone_and_a_page_keeps_its_line() {
    let mut canvas = doc();
    let root =
        seed_document(&mut canvas, "", koru_core::explore::KIND_CANVAS).expect("a canvas seeds");
    assert_eq!(document_root(&canvas), Some(root));
    assert_eq!(canvas.kind_of(root), Some(Kind::Document));
    assert!(
        canvas.children(Parent::Node(root)).unwrap().is_empty(),
        "no paragraph under a canvas's root"
    );

    let mut page = doc();
    let root =
        seed_document(&mut page, "", koru_core::explore::KIND_DOCUMENT).expect("a page seeds");
    assert_eq!(page.children(Parent::Node(root)).unwrap().len(), 1);
}

/// A canvas the store registered empty is seeded on its first open the way
/// `create_canvas` seeds one — titled from the manifest, root only.
#[test]
fn an_empty_registered_canvas_is_seeded_with_its_root_only() {
    let (mut w, _, canvas) = with_a_page_and_a_canvas();
    let mut opened = doc();
    assert!(w.seed_empty_document(canvas, &mut opened));
    let root = document_root(&opened).expect("a seeded canvas has a root");
    assert_eq!(opened.read(root).unwrap().content.as_text(), Some("Board"));
    assert!(opened.children(Parent::Node(root)).unwrap().is_empty());
    assert!(!w.seed_empty_document(canvas, &mut opened), "never twice");
}

/// **The provisional row is the kind the real one will be.** A canvas made a
/// moment ago, before the manifest's echo, is listed as a canvas — at the
/// workspace root, where `create_canvas` asks the store to put it — and never
/// as a page, even briefly.
#[test]
fn a_provisional_canvas_is_listed_as_a_canvas_at_the_root() {
    let (mut w, _, _) = with_a_page_and_a_canvas();
    let fresh = koru_core::DocumentId::new();
    w.pending_documents.push(Pending {
        id: fresh,
        title: String::new(),
        after: None,
        parent: None,
        kind: koru_core::explore::KIND_CANVAS.into(),
    });
    let entries = w.entries();
    let row = entries
        .iter()
        .find(|entry| entry.id == fresh)
        .expect("listed before the echo");
    assert_eq!(row.kind, koru_core::explore::KIND_CANVAS);
    assert_eq!(row.parent, None);
    assert_eq!(
        entries.last().map(|entry| entry.id),
        Some(fresh),
        "appended, the store's own placement for (None, None)"
    );
    assert_eq!(
        w.document_kind(fresh).as_deref(),
        Some(koru_core::explore::KIND_CANVAS)
    );
}

/// `document_kind` reads the manifest — what `copy_document` registers the
/// copy under, so a duplicated canvas is a canvas — and says nothing for a
/// document it does not name rather than guessing a page.
#[test]
fn a_documents_kind_is_read_from_the_manifest() {
    let (w, page, canvas) = with_a_page_and_a_canvas();
    assert_eq!(
        w.document_kind(canvas).as_deref(),
        Some(koru_core::explore::KIND_CANVAS)
    );
    assert_eq!(
        w.document_kind(page).as_deref(),
        Some(koru_core::explore::KIND_DOCUMENT)
    );
    assert_eq!(w.document_kind(koru_core::DocumentId::new()), None);
    assert_eq!(
        Workspace::new(doc()).document_kind(canvas),
        None,
        "no manifest, no kind"
    );
}

/// A canvas in the bin still has its kind — the Trash notice's *Keep a copy*
/// copies from there, and the copy has to come out a canvas.
#[test]
fn a_binned_canvas_keeps_its_kind() {
    let (mut w, _, canvas) = with_a_page_and_a_canvas();
    let at = koru_store_core::model::Timestamp::from_millis(1_700_000_000_001);
    w.manifest
        .as_mut()
        .unwrap()
        .trash_document(explore::store_document(canvas), at, None)
        .unwrap();
    assert!(w.trashed_entries().iter().any(|entry| entry.id == canvas));
    assert_eq!(
        w.document_kind(canvas).as_deref(),
        Some(koru_core::explore::KIND_CANVAS)
    );
}
