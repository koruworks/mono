// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{add_fact, fixture};
use super::*;
use history::tests::evidence;
use koru_core::{Attr, AttrValue, DocumentReference, Heading, Mark, Node};
use uuid::Uuid;

fn correct(fact: NodeId) -> MemoryRecipeOperation {
    MemoryRecipeOperation::Correct {
        fact: fact.as_uuid(),
        summary: "New summary".into(),
        detail: "New detail".into(),
    }
}
fn verified(
    doc: &koru_core::WorkspaceDoc,
    page: BotPage,
    version: DocumentVersionId,
) -> VerifiedMemory {
    history::validate(page.clone(), version, evidence(doc, &page, version)).unwrap()
}

#[test]
fn recipe_preserves_raw_expected_and_full_fact_uuid_domain() {
    for id in [
        Uuid::nil(),
        Uuid::parse_str("12345678-1234-5678-9234-123456789abc").unwrap(),
    ] {
        let (mut doc, page, version, root, _) = fixture();
        let mut value = Node::paragraph("  exact summary  ").unwrap();
        value.id = NodeId::from_uuid(id);
        value.meta.heading = Some(Heading::H2);
        let fact = doc.insert(Parent::Node(root), value).unwrap();
        let memory = verified(&doc, page, version);
        assert_eq!(memory.source.facts[0].summary, "exact summary");
        let recipe = prepare_recipe(&memory, &correct(fact)).unwrap();
        let MemoryRecipeChange::Correct { fact, expected, .. } = recipe.change else {
            panic!()
        };
        assert_eq!(fact, id);
        assert_eq!(expected, "  exact summary  ");
    }
}

#[test]
fn recipe_placement_membership_and_trash_restore_use_visible_root() {
    let (mut doc, page, version, root, help) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    let nested = add_fact(&mut doc, fact, "nested");
    doc.trash(fact, 10, None).unwrap();
    let memory = verified(&doc, page.clone(), version);
    for bad in [root, help, nested, fact, NodeId::new()] {
        assert!(prepare_recipe(&memory, &correct(bad)).is_err());
    }
    let append = MemoryRecipeOperation::Remember {
        summary: "new".into(),
        detail: "detail".into(),
    };
    let MemoryRecipeChange::Remember { after, .. } =
        prepare_recipe(&memory, &append).unwrap().change
    else {
        panic!()
    };
    assert_eq!(after, Some(help.as_uuid()));
    doc.restore(fact, None).unwrap();
    let memory = verified(&doc, page, version);
    assert!(prepare_recipe(&memory, &correct(fact)).is_ok());
    let MemoryRecipeChange::Remember { after, .. } =
        prepare_recipe(&memory, &append).unwrap().change
    else {
        panic!()
    };
    assert_eq!(after, Some(fact.as_uuid()));
}

#[test]
fn recipe_correct_accepts_supported_marks_and_source_pill() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    let detail = doc
        .insert(Parent::Node(fact), Node::paragraph("detail").unwrap())
        .unwrap();
    doc.set_mark(detail, 0..6, Mark::Bold, true).unwrap();
    let source = doc
        .insert(Parent::Node(fact), Node::paragraph("").unwrap())
        .unwrap();
    doc.insert_pill(
        source,
        0,
        &Attr::labelled(
            "document",
            AttrValue::Document(DocumentReference {
                document: koru_core::DocumentId::new(),
                node: None,
            }),
            "Source Run",
        ),
    )
    .unwrap();
    let memory = verified(&doc, page, version);
    let MemoryRecipeChange::Correct { children, .. } =
        prepare_recipe(&memory, &correct(fact)).unwrap().change
    else {
        panic!()
    };
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].expected, "detail");
    assert_eq!(children[1].expected, koru_core::ANCHOR.to_string());
}

#[test]
fn recipe_lf_refuses_without_mutation_or_allocation() {
    let (doc, page, version, _, _) = fixture();
    let before = doc.version();
    let memory = verified(&doc, page, version);
    for (summary, detail) in [("bad\nsummary", "detail"), ("summary", "bad\ndetail")] {
        assert!(matches!(
            prepare_recipe(
                &memory,
                &MemoryRecipeOperation::Remember {
                    summary: summary.into(),
                    detail: detail.into()
                }
            ),
            Err(MemoryCodecError::InvalidWriteInput)
        ));
    }
    assert_eq!(memory.doc.version(), before);
}

#[test]
fn recipe_child_and_remaining_text_bounds_are_checked() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    for _ in 0..128 {
        doc.insert(Parent::Node(fact), Node::paragraph("").unwrap())
            .unwrap();
    }
    assert!(prepare_recipe(&verified(&doc, page.clone(), version), &correct(fact)).is_ok());
    doc.insert(Parent::Node(fact), Node::paragraph("").unwrap())
        .unwrap();
    assert!(matches!(
        prepare_recipe(&verified(&doc, page, version), &correct(fact)),
        Err(MemoryCodecError::DetailTooLarge)
    ));

    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    let child = doc
        .insert(
            Parent::Node(fact),
            Node::paragraph(&"x".repeat(32 * 1024 - 4)).unwrap(),
        )
        .unwrap();
    assert!(prepare_recipe(&verified(&doc, page.clone(), version), &correct(fact)).is_ok());
    doc.set_text(child, &"x".repeat(32 * 1024)).unwrap();
    let memory = verified(&doc, page, version);
    assert!(matches!(
        prepare_recipe(&memory, &correct(fact)),
        Err(MemoryCodecError::DetailTooLarge)
    ));
    assert!(
        prepare_recipe(
            &memory,
            &MemoryRecipeOperation::Forget {
                fact: fact.as_uuid()
            }
        )
        .is_ok()
    );
}

#[test]
fn recipe_escaped_bytes_refuse_before_returning_a_large_body() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    doc.insert(
        Parent::Node(fact),
        Node::paragraph(&"\u{1}".repeat(12 * 1024)).unwrap(),
    )
    .unwrap();
    let memory = verified(&doc, page, version);
    assert!(matches!(
        prepare_recipe(&memory, &correct(fact)),
        Err(MemoryCodecError::DetailTooLarge)
    ));
}

#[test]
fn recipe_correct_refuses_nontext_and_unknown_marks_but_forget_needs_no_detail_projection() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    doc.insert(
        Parent::Node(fact),
        Node::image(
            koru_core::BlobHash::new("fnv1a", "0000000000000000"),
            "image/png",
            Some(1),
            Some(1),
        )
        .unwrap(),
    )
    .unwrap();
    let memory = verified(&doc, page, version);
    assert!(matches!(
        prepare_recipe(&memory, &correct(fact)),
        Err(MemoryCodecError::UnsupportedDetail)
    ));
    assert!(
        prepare_recipe(
            &memory,
            &MemoryRecipeOperation::Forget {
                fact: fact.as_uuid()
            }
        )
        .is_ok()
    );

    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    let child = doc
        .insert(Parent::Node(fact), Node::paragraph("detail").unwrap())
        .unwrap();
    let mut styles = loro::StyleConfigMap::new();
    styles.insert(
        "future_mark".into(),
        loro::StyleConfig {
            expand: loro::ExpandType::None,
        },
    );
    doc.doc().config_text_style(styles);
    let tree = doc.doc().get_tree("nodes");
    for node in tree.nodes() {
        let meta = tree.get_meta(node).unwrap();
        if matches!(meta.get("id"), Some(loro::ValueOrContainer::Value(loro::LoroValue::String(id))) if id.as_str() == child.to_storage())
        {
            let Some(loro::ValueOrContainer::Container(loro::Container::Text(text))) =
                meta.get("text")
            else {
                panic!()
            };
            text.mark_utf8(0..6, "future_mark", true).unwrap();
        }
    }
    doc.commit();
    let memory = verified(&doc, page, version);
    assert!(matches!(
        prepare_recipe(&memory, &correct(fact)),
        Err(MemoryCodecError::UnsupportedDetail)
    ));
    assert!(
        prepare_recipe(
            &memory,
            &MemoryRecipeOperation::Forget {
                fact: fact.as_uuid()
            }
        )
        .is_ok()
    );
}
