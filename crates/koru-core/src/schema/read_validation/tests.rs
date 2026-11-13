// SPDX-License-Identifier: MPL-2.0

use super::*;

fn seed() -> (WorkspaceDoc, WorkspaceId, DocumentId) {
    let home = WorkspaceId::new();
    let page = DocumentId::new();
    let mut doc = WorkspaceDoc::page_genesis(home, page).unwrap();
    doc.set_text(genesis_root_id(page), "Memory").unwrap();
    (doc, home, page)
}

fn meta(doc: &WorkspaceDoc, id: NodeId) -> LoroMap {
    doc.nodes().get_meta(doc.tree_id(id).unwrap()).unwrap()
}

fn unchanged<T>(doc: &WorkspaceDoc, read: impl FnOnce() -> T) -> T {
    let before = doc.version();
    let updates = doc.export_all_updates().unwrap();
    let result = read();
    assert_eq!(doc.version(), before);
    assert_eq!(doc.export_all_updates().unwrap(), updates);
    result
}

#[test]
fn duplicate_public_ids_fail_before_index_use() {
    let (doc, _, page) = seed();
    meta(&doc, genesis_seed_id(page))
        .insert(K_ID, genesis_root_id(page).to_storage())
        .unwrap();
    assert_eq!(
        unchanged(&doc, || doc.validate_node_identities(2)),
        Err(E::DuplicateNodeIdentity)
    );
}

#[test]
fn malformed_raw_id_refused() {
    let (doc, _, page) = seed();
    meta(&doc, genesis_seed_id(page))
        .insert(K_ID, "not a uuid")
        .unwrap();
    assert_eq!(
        unchanged(&doc, || doc.validate_node_identities(2)),
        Err(E::InvalidNodeIdentity)
    );
}

#[test]
fn deleted_id_budget_is_counted() {
    let (doc, _, page) = seed();
    doc.nodes()
        .delete(doc.tree_id(genesis_seed_id(page)).unwrap())
        .unwrap();
    assert_eq!(
        unchanged(&doc, || doc.validate_node_identities(1)),
        Err(E::NodeLimit { limit: 1 })
    );
}

#[test]
fn seed_validation_preserves_vv() {
    let (doc, home, page) = seed();
    assert_eq!(
        unchanged(&doc, || doc.validate_page_seed_v1(home, page, "Memory")),
        Ok(())
    );
}

#[test]
fn seed_rejects_extra_root_metadata_attr_mark_child() {
    for variant in 0..5 {
        let (mut doc, home, page) = seed();
        let root = genesis_root_id(page);
        match variant {
            0 => {
                doc.doc.get_map("extra").insert("data", true).unwrap();
            }
            1 => {
                meta(&doc, root).insert("future", true).unwrap();
            }
            2 => {
                meta(&doc, root)
                    .insert_container(K_ATTRS, LoroMap::new())
                    .unwrap();
            }
            3 => {
                existing_text(&meta(&doc, root))
                    .unwrap()
                    .mark(0..1, Mark::Bold.key(), true)
                    .unwrap();
            }
            _ => {
                doc.insert(Parent::Node(root), Node::paragraph("extra").unwrap())
                    .unwrap();
            }
        }
        assert!(
            unchanged(&doc, || doc.validate_page_seed_v1(home, page, "Memory")).is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn evidence_missing_or_wrong_text_never_repairs() {
    for missing in [true, false] {
        let (doc, _, page) = seed();
        let id = genesis_seed_id(page);
        let raw = meta(&doc, id);
        raw.delete(K_TEXT).unwrap();
        if !missing {
            raw.insert(K_TEXT, "wrong container").unwrap();
        }
        assert_eq!(
            unchanged(&doc, || doc.read_node_evidence(id, 10)),
            Err(E::InvalidTree)
        );
        assert!(matches!(
            raw.get(K_TEXT),
            None | Some(ValueOrContainer::Value(_))
        ));
    }
}

#[test]
fn evidence_unknown_marks_count() {
    let (doc, _, page) = seed();
    doc.doc.config_default_text_style(Some(loro::StyleConfig {
        expand: loro::ExpandType::None,
    }));
    let id = genesis_root_id(page);
    let text = existing_text(&meta(&doc, id)).unwrap();
    text.mark(0..1, "future-mark", "future").unwrap();
    assert!(
        unchanged(&doc, || doc.read_node_evidence(id, 64))
            .unwrap()
            .has_marks
    );
}

#[test]
fn seed_allows_currently_empty_extra_roots_without_comparing_history() {
    let (doc, home, page) = seed();
    let extra = doc.doc.get_map("extra");
    extra.insert("removed", true).unwrap();
    extra.delete("removed").unwrap();
    assert_eq!(
        unchanged(&doc, || doc.validate_page_seed_v1(home, page, "Memory")),
        Ok(())
    );
    doc.doc.get_text("other").insert(0, "not empty").unwrap();
    assert_eq!(
        unchanged(&doc, || doc.validate_page_seed_v1(home, page, "Memory")),
        Err(E::SeedMismatch)
    );
}

#[test]
fn evidence_raw_attributes_are_not_dropped() {
    let (doc, _, page) = seed();
    let id = genesis_seed_id(page);
    let attrs = meta(&doc, id)
        .insert_container(K_ATTRS, LoroMap::new())
        .unwrap();
    attrs.insert("unknown", "malformed record").unwrap();
    assert!(
        unchanged(&doc, || doc.read_node_evidence(id, 0))
            .unwrap()
            .has_attribute_data
    );
}

#[test]
fn evidence_blob_with_stale_text_is_nontext() {
    let (doc, _, page) = seed();
    let id = genesis_root_id(page);
    meta(&doc, id).insert(K_BLOB, "blob").unwrap();
    assert_eq!(
        unchanged(&doc, || doc.read_node_evidence(id, 0))
            .unwrap()
            .text,
        None
    );
}

#[test]
fn evidence_heading_uses_model_integer_semantics() {
    let (doc, _, page) = seed();
    let id = genesis_seed_id(page);
    for (value, expected) in [(0, None), (2, Some(Heading::H2)), (99, Some(Heading::H6))] {
        meta(&doc, id).insert(K_HEADING, value).unwrap();
        assert_eq!(
            unchanged(&doc, || doc.read_node_evidence(id, 0))
                .unwrap()
                .heading,
            expected
        );
    }
}

#[test]
fn evidence_text_budget_before_decode() {
    let (doc, _, page) = seed();
    assert_eq!(
        unchanged(&doc, || doc.read_node_evidence(genesis_root_id(page), 5)),
        Err(E::TextLimit { limit: 5 })
    );
}

#[test]
fn depth_chain_has_no_recursive_validator() {
    let (mut doc, _, page) = seed();
    let mut parent = genesis_seed_id(page);
    for _ in 0..300 {
        parent = doc
            .insert(Parent::Node(parent), Node::paragraph("").unwrap())
            .unwrap();
    }
    assert_eq!(
        unchanged(&doc, || doc.validate_node_identities(302))
            .unwrap()
            .live_nodes,
        302
    );
}
