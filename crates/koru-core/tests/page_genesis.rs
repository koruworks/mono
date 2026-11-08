// SPDX-License-Identifier: MPL-2.0

//! The identity proof gate for page documents (page-document plan, P0).
//!
//! A page document's id is derived from its page, and its genesis is written
//! identically on every device under a peer derived from that id. These tests
//! are what lets the rest of the feature assume *one document from the first
//! keystroke*: two independent geneses import into each other as no-ops, the
//! genesis peer's counter never moves afterwards, and a golden file pins the
//! changes and op ids the genesis writes so a code change or a Loro upgrade
//! that would alter them fails here rather than forking op ids in the field.

use koru_core::attributes::TaskReason;
use koru_core::ids::ListId;
use koru_core::ids::{PageKey, genesis_peer, page_document_id};
use koru_core::node::Node;
use koru_core::pages::{covers, join};
use koru_core::schema::{
    PAGE_GENESIS_SCHEMA_VERSION_V1, Parent, WorkspaceDoc, genesis_root_id, genesis_seed_id,
};
use koru_core::tasks::{Bucket, TaskFields, ensure_task, is_task, set_due};
use koru_core::time::{DateOnly, Temporal};
use koru_core::{DocumentId, Kind, NodeId, WorkspaceId};
use loro::VersionVector;

/// The pinned inputs the golden file was generated from. A known vector, so
/// the derived id and peer are asserted as values, not only as "equal on two
/// calls".
const WORKSPACE: &str = "0f6d5c4b-3a29-4f18-8e07-16d5c4b3a291";
const LIST: &str = "7b1c2d3e-4f50-4a61-9b72-83c4d5e6f708";

fn pinned() -> (WorkspaceId, PageKey, DocumentId) {
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let page = PageKey::List(ListId::parse(LIST).unwrap());
    (workspace, page, page_document_id(workspace, page, 0))
}

/// Two devices, each running the genesis on its own, for one derived id.
fn two_geneses() -> (WorkspaceDoc, WorkspaceDoc, DocumentId) {
    let (workspace, _, document) = pinned();
    let a = WorkspaceDoc::page_genesis(workspace, document).unwrap();
    let b = WorkspaceDoc::page_genesis(workspace, document).unwrap();
    (a, b, document)
}

fn sync(a: &mut WorkspaceDoc, b: &mut WorkspaceDoc) {
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();
}

/// Everything readable in document order: `(id, depth, kind, text)`.
fn shape(ws: &WorkspaceDoc) -> Vec<(NodeId, usize, Kind, String)> {
    ws.walk()
        .unwrap()
        .into_iter()
        .map(|(id, depth)| {
            let node = ws.read(id).unwrap();
            (
                id,
                depth,
                node.kind,
                node.content.as_text().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn document_roots(ws: &WorkspaceDoc) -> Vec<NodeId> {
    ws.children(Parent::Root)
        .unwrap()
        .into_iter()
        .filter(|&id| ws.kind_of(id) == Some(Kind::Document))
        .collect()
}

/// Where the genesis peer's counter stands — the number that must never move.
fn genesis_counter(ws: &WorkspaceDoc, document: DocumentId) -> i32 {
    ws.version()
        .get(&genesis_peer(document))
        .copied()
        .unwrap_or(0)
}

fn restart(ws: &WorkspaceDoc) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap()
}

fn on(s: &str) -> Temporal {
    Temporal::Date(DateOnly::parse(s).unwrap())
}

// --------------------------------------------------------------- identity

#[test]
fn the_derived_id_is_a_known_vector() {
    let (workspace, page, document) = pinned();
    assert_eq!(
        document.to_storage(),
        "4bed194b-070d-53be-a817-8f0c596ad3c0",
        "the V1 derivation changed — that is a new genesis version, not a fix"
    );
    assert_eq!(genesis_peer(document), 0xff16_8f1b_1679_5afd);
    assert_eq!(
        genesis_root_id(document).to_storage(),
        "dded49b5-8b14-5026-addf-cef2e4d0bcc3"
    );
    assert_eq!(
        genesis_seed_id(document).to_storage(),
        "b9b75ca5-d0db-50f0-90e4-016c6ef6da17"
    );
    // Every input is part of the name.
    assert_ne!(page_document_id(workspace, page, 1), document, "generation");
    assert_ne!(
        page_document_id(WorkspaceId::new(), page, 0),
        document,
        "workspace"
    );
    assert_ne!(
        page_document_id(workspace, PageKey::Bucket(Bucket::Today), 0),
        document,
        "page"
    );
}

#[test]
fn a_page_key_round_trips_through_storage() {
    let list = PageKey::List(ListId::new());
    assert_eq!(PageKey::parse(&list.to_storage()), Some(list));
    for bucket in Bucket::ALL {
        let key = PageKey::Bucket(bucket);
        assert_eq!(key.to_storage(), format!("bucket:{}", bucket.as_str()));
        assert_eq!(PageKey::parse(&key.to_storage()), Some(key));
    }
    assert_eq!(PageKey::parse("bucket:someday"), None);
    assert_eq!(PageKey::parse("list:not-a-uuid"), None);
    assert_eq!(PageKey::parse("today"), None);
}

#[test]
fn the_genesis_peer_is_never_loros_reserved_peer() {
    for _ in 0..64 {
        assert_ne!(genesis_peer(DocumentId::new()), u64::MAX);
    }
}

// ---------------------------------------------------------------- genesis

#[test]
fn a_genesis_is_one_root_one_seed_and_the_derived_ids() {
    let (workspace, _, document) = pinned();
    let ws = WorkspaceDoc::page_genesis(workspace, document).unwrap();

    assert_eq!(ws.id(), Some(workspace));
    assert_eq!(ws.name().as_deref(), Some(""));
    assert_eq!(ws.schema_version(), Some(PAGE_GENESIS_SCHEMA_VERSION_V1));
    let root = genesis_root_id(document);
    let seed = genesis_seed_id(document);
    assert_eq!(document_roots(&ws), vec![root]);
    assert_eq!(ws.children(Parent::Node(root)).unwrap(), vec![seed]);
    assert_eq!(ws.document_id(root), document, "the root carries the id");
    assert_eq!(
        shape(&ws),
        vec![
            (root, 0, Kind::Document, String::new()),
            (seed, 1, Kind::Paragraph, String::new())
        ]
    );
    assert!(
        !ws.on_genesis_peer(document),
        "the document leaves its genesis peer before anyone can edit it"
    );
    assert!(genesis_counter(&ws, document) > 0);
}

#[test]
fn two_independent_geneses_are_the_same_operations() {
    let (a, b, document) = two_geneses();
    assert_eq!(a.version(), b.version(), "the same op ids");
    assert_eq!(genesis_counter(&a, document), genesis_counter(&b, document));
    assert_eq!(decoded_changes(&a), decoded_changes(&b), "the same content");
    assert_ne!(a.peer_id(), b.peer_id(), "and two random peers afterwards");
}

#[test]
fn importing_the_other_devices_genesis_is_a_no_op() {
    for order in ["a then b", "b then a"] {
        let (mut a, mut b, document) = two_geneses();
        let (first, second) = if order == "a then b" {
            (&mut a, &mut b)
        } else {
            (&mut b, &mut a)
        };
        let before = second.version();
        let counter = genesis_counter(second, document);
        second.import(&first.export_all_updates().unwrap()).unwrap();
        assert_eq!(second.version(), before, "{order}: nothing new arrived");
        assert_eq!(genesis_counter(second, document), counter);
        assert_eq!(
            document_roots(second).len(),
            1,
            "{order}: one root, no fold"
        );
        assert_eq!(shape(second).len(), 2, "{order}: one root, one seed");
    }
}

#[test]
fn a_restarted_replica_still_meets_the_other_as_a_no_op() {
    let (a, b, document) = two_geneses();
    let mut a = restart(&a);
    let mut b = restart(&b);
    let counter = genesis_counter(&a, document);
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(shape(&a).len(), 2);
    assert_eq!(genesis_counter(&a, document), counter);
    assert_eq!(genesis_counter(&b, document), counter);
}

// --------------------------------------------------------- after genesis

#[test]
fn late_and_nested_insertions_on_both_devices_all_survive() {
    let (mut a, mut b, document) = two_geneses();
    let root = genesis_root_id(document);
    let counter = genesis_counter(&a, document);

    // A writes two lines; B, never having seen A, writes one and nests under it.
    a.insert(Parent::Node(root), Node::paragraph("A's first").unwrap())
        .unwrap();
    a.insert(Parent::Node(root), Node::paragraph("A's second").unwrap())
        .unwrap();
    let b_line = b
        .insert(Parent::Node(root), Node::paragraph("B's line").unwrap())
        .unwrap();
    b.insert(Parent::Node(b_line), Node::paragraph("B's nested").unwrap())
        .unwrap();

    for order in ["a first", "b first"] {
        let (mut x, mut y) = (restart(&a), restart(&b));
        if order == "b first" {
            std::mem::swap(&mut x, &mut y);
        }
        sync(&mut x, &mut y);
        assert_eq!(shape(&x), shape(&y), "{order}: converged");
        assert_eq!(document_roots(&x), vec![root], "{order}: one shared root");
        let texts: Vec<String> = shape(&x).into_iter().map(|(_, _, _, t)| t).collect();
        for text in ["A's first", "A's second", "B's line", "B's nested"] {
            assert!(
                texts.iter().any(|t| t == text),
                "{order}: {text} in {texts:?}"
            );
        }
        assert_eq!(
            x.children(Parent::Node(b_line)).unwrap().len(),
            1,
            "{order}: nested kept"
        );
        assert_eq!(
            x.walk().unwrap().len(),
            6,
            "{order}: root, seed and four lines"
        );
        for id in x.walk().unwrap().into_iter().map(|(id, _)| id) {
            x.read(id).unwrap();
        }
        assert_eq!(genesis_counter(&x, document), counter, "{order}");
        assert_eq!(genesis_counter(&y, document), counter, "{order}");
    }
}

#[test]
fn concurrent_edits_to_the_seed_paragraph_merge() {
    let (mut a, mut b, document) = two_geneses();
    let seed = genesis_seed_id(document);
    let counter = genesis_counter(&a, document);

    a.set_text(seed, "bike ").unwrap();
    b.set_text(seed, " project").unwrap();
    sync(&mut a, &mut b);

    let merged = a.read(seed).unwrap().content.as_text().unwrap().to_string();
    assert_eq!(merged, b.read(seed).unwrap().content.as_text().unwrap());
    assert!(
        merged.contains("bike") && merged.contains("project"),
        "{merged:?}"
    );
    assert_eq!(genesis_counter(&a, document), counter);
}

#[test]
fn a_deletion_on_one_device_and_an_edit_on_the_other_converge() {
    let (mut a, mut b, document) = two_geneses();
    let root = genesis_root_id(document);
    let seed = genesis_seed_id(document);
    let counter = genesis_counter(&a, document);

    let kept = a
        .insert(Parent::Node(root), Node::paragraph("kept").unwrap())
        .unwrap();
    a.delete(seed).unwrap();
    b.set_text(seed, "typed into the seed").unwrap();

    for order in ["a first", "b first"] {
        let (mut x, mut y) = (restart(&a), restart(&b));
        if order == "b first" {
            std::mem::swap(&mut x, &mut y);
        }
        sync(&mut x, &mut y);
        assert_eq!(shape(&x), shape(&y), "{order}");
        assert_eq!(document_roots(&x), vec![root], "{order}");
        assert!(x.read(kept).is_ok(), "{order}: the insertion survives");
        assert_eq!(genesis_counter(&x, document), counter, "{order}");
    }
}

#[test]
fn root_task_attributes_land_on_the_one_shared_root() {
    let (mut a, mut b, document) = two_geneses();
    let root = genesis_root_id(document);
    let counter = genesis_counter(&a, document);

    ensure_task(&mut a, root, TaskReason::Explicit).unwrap();
    set_due(&mut b, root, on("2026-09-10")).unwrap();

    for order in ["a first", "b first"] {
        let (mut x, mut y) = (restart(&a), restart(&b));
        if order == "b first" {
            std::mem::swap(&mut x, &mut y);
        }
        sync(&mut x, &mut y);
        assert_eq!(document_roots(&x), vec![root], "{order}: one root");
        let from_x = TaskFields::of(&x.read(root).unwrap());
        assert_eq!(from_x, TaskFields::of(&y.read(root).unwrap()), "{order}");
        assert!(is_task(&from_x), "{order}");
        assert_eq!(
            from_x.due,
            Some(on("2026-09-10")),
            "{order}: the due is on the root"
        );
        assert_eq!(genesis_counter(&x, document), counter, "{order}");
    }
}

#[test]
fn the_genesis_peers_counter_never_moves_after_genesis() {
    let (mut a, mut b, document) = two_geneses();
    let root = genesis_root_id(document);
    let seed = genesis_seed_id(document);
    let counter = genesis_counter(&a, document);
    let check = |ws: &WorkspaceDoc, step: &str| {
        assert_eq!(genesis_counter(ws, document), counter, "{step}");
        assert!(!ws.on_genesis_peer(document), "{step}");
    };

    a.set_text(seed, "edit").unwrap();
    check(&a, "edit");
    a.insert(Parent::Node(root), Node::paragraph("line").unwrap())
        .unwrap();
    check(&a, "insert");
    b.import(&a.export_all_updates().unwrap()).unwrap();
    check(&b, "import");
    let mut again = restart(&b);
    check(&again, "restart");
    again.set_text(seed, "after restart").unwrap();
    check(&again, "edit after restart");
    a.import(&again.export_all_updates().unwrap()).unwrap();
    check(&a, "import after restart");
}

// ------------------------------------------------------------ the golden

/// The decoded changes of everything written under the genesis peer:
/// change ids, deps, lamport, and every op's container, counter and content.
/// Peer compression is off so the peer ids are literal.
fn decoded_changes(ws: &WorkspaceDoc) -> serde_json::Value {
    let schema = ws
        .doc()
        .export_json_updates_without_peer_compression(&VersionVector::new(), &ws.version());
    serde_json::to_value(&schema.changes).unwrap()
}

const GOLDEN: &str = include_str!("golden/page_genesis_v1.json");

/// **The frozen prefix.** Changing what the genesis writes forks op ids
/// between builds; this fails first. If the change is intended it is a new
/// genesis version — a new namespace and new ids — never a regenerated file
/// under `V1`. A Loro upgrade that changes only the *JSON schema* of this
/// export, with every change id, op id and content the same, may regenerate
/// the file after that has been checked by hand; the failure message prints
/// what was produced.
#[test]
fn the_genesis_matches_the_golden_file() {
    let (workspace, _, document) = pinned();
    let ws = WorkspaceDoc::page_genesis(workspace, document).unwrap();
    let actual = decoded_changes(&ws);
    let expected: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    assert!(
        actual == expected,
        "the V1 genesis changed.\n--- produced ---\n{}\n--- golden ---\n{}",
        serde_json::to_string_pretty(&actual).unwrap(),
        serde_json::to_string_pretty(&expected).unwrap()
    );
    // And the whole prefix is the genesis peer's: nothing else wrote.
    let changes = actual.as_array().unwrap();
    assert!(!changes.is_empty());
    let peer = genesis_peer(document).to_string();
    for change in changes {
        let id = change["id"].as_str().unwrap();
        assert!(
            id.ends_with(&format!("@{peer}")),
            "{id} is not the genesis peer's"
        );
    }
}

// ------------------------------------------------------------- coverage

/// `join` and `covers` over vectors from real replicas, in the four shapes
/// the deletion protocol meets: contained, equal, concurrent and ahead — and
/// through the encoded form a deletion event stores.
#[test]
fn coverage_over_real_replica_versions() {
    let (mut a, mut b, document) = two_geneses();
    let seed = genesis_seed_id(document);
    let genesis = a.version();

    a.set_text(seed, "from a").unwrap();
    let a_only = a.version();
    b.set_text(seed, "from b").unwrap();
    let b_only = b.version();

    assert!(covers(&a_only, &genesis), "contained");
    assert!(covers(&a_only, &a_only), "equal");
    assert!(
        !covers(&a_only, &b_only) && !covers(&b_only, &a_only),
        "concurrent"
    );

    sync(&mut a, &mut b);
    let merged = a.version();
    assert_eq!(merged, b.version());
    assert!(
        covers(&merged, &a_only) && covers(&merged, &b_only),
        "ahead"
    );
    assert!(!covers(&a_only, &merged));

    let joined = join(&a_only, &b_only);
    assert_eq!(
        joined, merged,
        "the join of two concurrent versions is their merge"
    );
    assert_eq!(join(&b_only, &a_only), joined);
    assert_eq!(join(&joined, &a_only), joined, "monotone");

    // Through the encoded form, as a deletion event stores its coverage.
    let decoded = VersionVector::decode(&joined.encode()).unwrap();
    assert_eq!(decoded, joined);
    assert!(covers(&decoded, &b_only));
}
