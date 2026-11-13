// SPDX-License-Identifier: MPL-2.0

use super::index::{encode_index, verify_capture};
use super::seed::{seed_hash, seed_update, validate_seed_update};
use super::*;
use koru_ai_core::bots::{BotId, BotOwner, BotRequestId, DeviceId, UserScopeId};
use koru_core::schema::{genesis_root_id, genesis_seed_id};
use koru_core::{DocumentId, Heading, Mark, Node, Parent, WorkspaceDoc};
use uuid::Uuid;

fn identity() -> BotMemoryIdentity {
    let mut value = BotMemoryIdentity {
        owner: BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::from_uuid(Uuid::new_v4()),
        },
        bot: BotId::from_uuid(Uuid::new_v4()),
        workspace: koru_ai_core::WorkspaceId::new(),
        creation_request: BotRequestId::from_uuid(Uuid::new_v4()),
        candidate: koru_ai_core::DocumentId::new(),
        store_request: BotRequestId::from_uuid(Uuid::new_v4()),
        seed_format: 1,
        seed_title: "Memory".into(),
        seed_hash: [0; 32],
    };
    value.seed_hash = seed_hash(value.workspace, &value.seed_title).unwrap();
    value
}

fn fixture() -> (WorkspaceDoc, BotPage, DocumentVersionId, NodeId, NodeId) {
    let identity = identity();
    let page = BotPage {
        workspace: identity.workspace,
        document: identity.candidate,
    };
    let doc = WorkspaceDoc::from_snapshot(&seed_update(&identity).unwrap()).unwrap();
    let document = DocumentId::from_uuid(page.document.as_uuid());
    (
        doc,
        page,
        DocumentVersionId::new(),
        genesis_root_id(document),
        genesis_seed_id(document),
    )
}

fn source(doc: &WorkspaceDoc, page: BotPage, version: DocumentVersionId) -> MemoryIndexSource {
    observe(page, version, &doc.export_snapshot().unwrap()).unwrap()
}

fn add_fact(doc: &mut WorkspaceDoc, root: NodeId, text: &str) -> NodeId {
    let mut node = Node::paragraph(text).unwrap();
    node.meta.heading = Some(Heading::H2);
    doc.insert(Parent::Node(root), node).unwrap()
}

#[test]
fn format1_hash_vector_and_locale_independent_title() {
    let home =
        koru_ai_core::WorkspaceId::from_uuid(Uuid::from_u128(0x0123456789ab4def8123456789abcdef));
    let title = "Mémoire";
    let mut vector = b"koru-bot-memory-seed/1\0".to_vec();
    vector.extend_from_slice(&[
        1, 35, 69, 103, 137, 171, 77, 239, 129, 35, 69, 103, 137, 171, 205, 239,
    ]);
    vector.extend_from_slice(&[0, 0, 0, 8]);
    vector.extend_from_slice("Mémoire".as_bytes());
    assert_eq!(
        seed_hash(home, title).unwrap(),
        *blake3::hash(&vector).as_bytes()
    );
    for bad in ["", " \t ", "one\ntwo", "one\rtwo", &"x".repeat(257)] {
        assert_eq!(seed_hash(home, bad), Err(MemoryCodecError::InvalidSeed));
    }
}

#[test]
fn random_title_edit_peers_same_semantic_hash() {
    let identity = identity();
    let first = seed_update(&identity).unwrap();
    let second = seed_update(&identity).unwrap();
    assert_ne!(first, second);
    assert_ne!(
        validate_seed_update(&identity, &first).unwrap(),
        validate_seed_update(&identity, &second).unwrap()
    );
    assert_eq!(
        seed_hash(identity.workspace, &identity.seed_title).unwrap(),
        identity.seed_hash
    );
}

#[test]
fn seed_roundtrip_original_vv() {
    let identity = identity();
    let bytes = seed_update(&identity).unwrap();
    let imported = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    assert_eq!(
        validate_seed_update(&identity, &bytes).unwrap(),
        imported.version().encode()
    );
}

#[test]
fn wrong_identity_hash_or_schema_refused() {
    let identity = identity();
    let bytes = seed_update(&identity).unwrap();
    let mut wrong = identity.clone();
    wrong.candidate = koru_ai_core::DocumentId::new();
    assert!(validate_seed_update(&wrong, &bytes).is_err());
    wrong = identity.clone();
    wrong.seed_hash[0] ^= 1;
    assert_eq!(seed_update(&wrong), Err(MemoryCodecError::InvalidSeed));
    wrong = identity.clone();
    wrong.owner.device = DeviceId::from_uuid(Uuid::nil());
    assert_eq!(seed_update(&wrong), Err(MemoryCodecError::InvalidSeed));
    let raw = loro::LoroDoc::new();
    raw.import(&bytes).unwrap();
    raw.get_map("meta")
        .insert("schema_version", 999i64)
        .unwrap();
    let changed = raw.export(loro::ExportMode::snapshot()).unwrap();
    assert!(validate_seed_update(&identity, &changed).is_err());
    assert_eq!(
        observe(
            BotPage {
                workspace: identity.workspace,
                document: identity.candidate
            },
            DocumentVersionId::new(),
            &changed
        ),
        Err(MemoryCodecError::UnsupportedSchema)
    );
}

#[test]
fn root_h2_only_and_descendant_details() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "  Likes tea  ");
    add_fact(&mut doc, fact, "nested detail is not another fact");
    let prose = doc
        .insert(
            Parent::Node(root),
            Node::paragraph("ordinary prose").unwrap(),
        )
        .unwrap();
    add_fact(&mut doc, prose, "nested heading");
    let observed = source(&doc, page, version);
    assert_eq!(
        observed.facts,
        vec![MemorySummary {
            node: fact,
            summary: "Likes tea".into()
        }]
    );
    assert_eq!(observed.not_indexed_count, 1);
    assert_eq!(observed.invalid_count, 0);
}

#[test]
fn genesis_v5_paragraph_fact_is_retained() {
    let (mut doc, page, version, _, seed) = fixture();
    assert_eq!(seed.as_uuid().get_version_num(), 5);
    doc.set_text(seed, "Human-authored fact").unwrap();
    doc.set_heading(seed, Some(Heading::H2)).unwrap();
    assert_eq!(
        encode_index(&source(&doc, page, version)).unwrap().nodes,
        [seed]
    );
}

#[test]
fn seed_blank_is_not_invalid() {
    let (doc, page, version, _, _) = fixture();
    let observed = source(&doc, page, version);
    assert!(observed.facts.is_empty());
    assert!(observed.diagnostics.is_empty());
    assert_eq!((observed.invalid_count, observed.not_indexed_count), (0, 0));
}

#[test]
fn unindexed_prose_is_visible() {
    let (mut doc, page, version, root, seed) = fixture();
    doc.set_text(seed, "ordinary prose").unwrap();
    let container = doc
        .insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    doc.insert(
        Parent::Node(container),
        Node::paragraph("nested prose").unwrap(),
    )
    .unwrap();
    assert_eq!(source(&doc, page, version).not_indexed_count, 2);
}

#[test]
fn blank_marked_or_linked_prose_is_not_empty() {
    let (mut doc, page, version, root, seed) = fixture();
    doc.set_text(seed, " ").unwrap();
    doc.set_mark(seed, 0..1, Mark::Bold, true).unwrap();
    add_fact(&mut doc, root, "\u{2060}");
    let observed = source(&doc, page, version);
    assert_eq!((observed.invalid_count, observed.not_indexed_count), (1, 1));
}

#[test]
fn encoded_node_depth_text_budgets() {
    let (mut doc, page, version, _, seed) = fixture();
    assert_eq!(
        observe(page.clone(), version, &vec![0; SNAPSHOT_LIMIT + 1]),
        Err(MemoryCodecError::TooLarge(MemoryBudget::EncodedSnapshot))
    );
    let mut parent = seed;
    for _ in 0..DEPTH_LIMIT {
        parent = doc
            .insert(Parent::Node(parent), Node::paragraph("").unwrap())
            .unwrap();
    }
    assert_eq!(
        observe(page, version, &doc.export_snapshot().unwrap()),
        Err(MemoryCodecError::TooLarge(MemoryBudget::Depth))
    );
    let (mut doc, page, version, _, seed) = fixture();
    doc.set_text(seed, &"x".repeat(TEXT_LIMIT)).unwrap();
    assert_eq!(
        observe(page, version, &doc.export_snapshot().unwrap()),
        Err(MemoryCodecError::TooLarge(MemoryBudget::Text))
    );
    // Raw-node limit behavior, including deleted IDs, is exercised directly in
    // core's deleted_id_budget_is_counted; the observer maps that same error.
}

#[test]
fn escaped_unicode_complete_index_fits8k() {
    let (mut doc, page, version, root, _) = fixture();
    for _ in 0..30 {
        add_fact(&mut doc, root, &"\"\\\t茶".repeat(100));
    }
    let encoded = encode_index(&source(&doc, page, version)).unwrap();
    assert!(encoded.bytes.len() <= INDEX_LIMIT);
    assert!(encoded.omitted_count > 0);
    let block = std::str::from_utf8(&encoded.bytes).unwrap();
    let body = block
        .strip_prefix("\n<koru-memory-index-v1>\n")
        .unwrap()
        .strip_suffix("\n</koru-memory-index-v1>\n")
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        parsed["facts"].as_array().unwrap().len(),
        encoded.nodes.len()
    );
}

#[test]
fn prefix_selection_and256_limit() {
    let (mut doc, page, version, root, _) = fixture();
    let ids: Vec<_> = (0..300).map(|_| add_fact(&mut doc, root, "x")).collect();
    let encoded = encode_index(&source(&doc, page, version)).unwrap();
    assert!(encoded.nodes.len() <= FACT_LIMIT);
    assert_eq!(encoded.nodes, ids[..encoded.nodes.len()]);
    assert_eq!(encoded.nodes.len() as u64 + encoded.omitted_count, 300);
}

#[test]
fn digest_vector_and_exact_reencoding() {
    let page = BotPage {
        workspace: koru_ai_core::WorkspaceId::from_uuid(Uuid::from_u128(1)),
        document: koru_ai_core::DocumentId::from_uuid(Uuid::from_u128(2)),
    };
    let version = DocumentVersionId::from_uuid(Uuid::from_u128(3));
    let source = MemoryIndexSource {
        page,
        version,
        facts: vec![],
        diagnostics: vec![],
        invalid_count: 2,
        not_indexed_count: 3,
    };
    let actual = encode_index(&source).unwrap();
    let expected = concat!(
        "\n<koru-memory-index-v1>\n",
        "{\"version\":1,\"page\":{\"workspace\":\"00000000-0000-0000-0000-000000000001\",\"document\":\"00000000-0000-0000-0000-000000000002\"},\"history\":\"00000000-0000-0000-0000-000000000003\",\"facts\":[],\"omitted_count\":0,\"invalid_count\":2,\"not_indexed_count\":3}",
        "\n</koru-memory-index-v1>\n"
    );
    assert_eq!(actual.bytes, expected.as_bytes());
    assert_eq!(actual.digest, *blake3::hash(expected.as_bytes()).as_bytes());
    assert_eq!(actual, encode_index(&source).unwrap());
}

#[test]
fn captured_nodes_counts_digest_mismatch_refused() {
    let (mut doc, page, version, root, _) = fixture();
    add_fact(&mut doc, root, "remembered");
    let snapshot = doc.export_snapshot().unwrap();
    let index = encode_index(&source(&doc, page.clone(), version)).unwrap();
    let expected = MemoryIndexExpectation::Captured {
        page,
        version,
        nodes: index.nodes.clone(),
        omitted: index.omitted_count,
        invalid: index.invalid_count,
        digest: index.digest,
    };
    assert_eq!(verify_capture(&expected, Some(&snapshot)).unwrap(), index);
    for variant in 0..4 {
        let mut changed = expected.clone();
        if let MemoryIndexExpectation::Captured {
            nodes,
            omitted,
            invalid,
            digest,
            ..
        } = &mut changed
        {
            match variant {
                0 => nodes.clear(),
                1 => *omitted += 1,
                2 => *invalid += 1,
                _ => digest[0] ^= 1,
            }
        }
        assert_eq!(
            verify_capture(&changed, Some(&snapshot)),
            Err(MemoryCodecError::InvalidCapture)
        );
    }
}

#[test]
fn absent_requires_empty_digest() {
    let empty = *blake3::hash(&[]).as_bytes();
    let expected = MemoryIndexExpectation::Absent { digest: empty };
    assert!(verify_capture(&expected, None).unwrap().bytes.is_empty());
    assert_eq!(
        verify_capture(&expected, Some(&[])),
        Err(MemoryCodecError::InvalidCapture)
    );
    assert_eq!(
        verify_capture(&MemoryIndexExpectation::Absent { digest: [0; 32] }, None),
        Err(MemoryCodecError::InvalidCapture)
    );
}

#[test]
fn ordinary_trash_and_restore_remove_and_reinstate_same_fact() {
    let (mut doc, page, version, root, _) = fixture();
    let removed = add_fact(&mut doc, root, "private discarded fact");
    let retained = add_fact(&mut doc, root, "retained fact");
    doc.insert(
        Parent::Node(removed),
        Node::paragraph("private discarded detail").unwrap(),
    )
    .unwrap();
    doc.trash(removed, 1, None).unwrap();
    let observed = source(&doc, page.clone(), version);
    assert_eq!(
        observed
            .facts
            .iter()
            .map(|fact| fact.node)
            .collect::<Vec<_>>(),
        [retained]
    );
    assert!(observed.diagnostics.is_empty());
    let encoded = encode_index(&observed).unwrap();
    assert!(
        !std::str::from_utf8(&encoded.bytes)
            .unwrap()
            .contains("private discarded")
    );
    doc.restore(removed, None).unwrap();
    let restored = source(&doc, page, version);
    assert_eq!(
        restored
            .facts
            .iter()
            .map(|fact| fact.node)
            .collect::<Vec<_>>(),
        [retained, removed]
    );
}

#[test]
fn concurrent_trash_roots_emit_no_private_summaries_or_diagnostics() {
    let (mut first, page, version, root, _) = fixture();
    let one = add_fact(&mut first, root, "private one");
    let two = add_fact(&mut first, root, "private two");
    let keep = add_fact(&mut first, root, "keep");
    let mut second = WorkspaceDoc::from_snapshot(&first.export_snapshot().unwrap()).unwrap();
    first.trash(one, 1, None).unwrap();
    second.trash(two, 2, None).unwrap();
    first.import(&second.export_all_updates().unwrap()).unwrap();
    assert_eq!(
        first
            .validate_node_identities(NODE_LIMIT)
            .unwrap()
            .root_nodes
            .len(),
        3
    );
    let observed = source(&first, page, version);
    assert_eq!(
        observed
            .facts
            .iter()
            .map(|fact| fact.node)
            .collect::<Vec<_>>(),
        [keep]
    );
    assert_eq!((observed.invalid_count, observed.not_indexed_count), (0, 0));
    assert!(observed.diagnostics.is_empty());
    assert!(
        !std::str::from_utf8(&encode_index(&observed).unwrap().bytes)
            .unwrap()
            .contains("private")
    );
}

#[test]
fn trash_subtrees_still_consume_validation_budgets() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "removed");
    let mut parent = fact;
    for _ in 0..DEPTH_LIMIT {
        parent = doc
            .insert(Parent::Node(parent), Node::paragraph("").unwrap())
            .unwrap();
    }
    doc.trash(fact, 1, None).unwrap();
    assert_eq!(
        observe(page, version, &doc.export_snapshot().unwrap()),
        Err(MemoryCodecError::TooLarge(MemoryBudget::Depth))
    );
    let (mut doc, page, version, _, seed) = fixture();
    doc.set_text(seed, &"x".repeat(TEXT_LIMIT)).unwrap();
    doc.trash(seed, 1, None).unwrap();
    assert_eq!(
        observe(page, version, &doc.export_snapshot().unwrap()),
        Err(MemoryCodecError::TooLarge(MemoryBudget::Text))
    );
}

#[test]
fn visible_extra_root_and_nested_trash_refuse() {
    let (mut doc, page, version, _, _) = fixture();
    doc.insert(Parent::Root, Node::paragraph("extra visible root").unwrap())
        .unwrap();
    assert_eq!(
        observe(page, version, &doc.export_snapshot().unwrap()),
        Err(MemoryCodecError::InvalidSnapshot)
    );
    let (mut doc, page, version, root, _) = fixture();
    let nested = doc
        .insert(Parent::Node(root), Node::trash().unwrap())
        .unwrap();
    add_fact(&mut doc, nested, "must not be disclosed");
    assert_eq!(
        observe(page, version, &doc.export_snapshot().unwrap()),
        Err(MemoryCodecError::InvalidSnapshot)
    );
}

#[test]
fn nontext_h2_is_invalid_instead_of_empty_success() {
    let (mut doc, page, version, root, _) = fixture();
    let heading = add_fact(&mut doc, root, "placeholder");
    let raw = loro::LoroDoc::new();
    raw.import(&doc.export_snapshot().unwrap()).unwrap();
    let tree = raw.get_tree("nodes");
    for node in tree.nodes() {
        let meta = tree.get_meta(node).unwrap();
        if matches!(meta.get("id"), Some(loro::ValueOrContainer::Value(loro::LoroValue::String(id))) if id.as_str() == heading.to_storage())
        {
            meta.insert("blob", "example").unwrap();
        }
    }
    let observed = observe(
        page,
        version,
        &raw.export(loro::ExportMode::snapshot()).unwrap(),
    )
    .unwrap();
    assert!(observed.facts.is_empty());
    assert_eq!(observed.invalid_count, 1);
}
