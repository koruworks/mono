// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{add_fact, fixture};
use super::*;
use history::tests::evidence;

#[test]
fn detail_fact_membership_excludes_trash_and_unrelated_nodes() {
    let (mut doc, binding, version, root, seed) = fixture();
    let fact = add_fact(&mut doc, root, "fact");
    let removed = add_fact(&mut doc, root, "removed");
    doc.trash(removed, 100, None).unwrap();
    let memory =
        history::validate(binding.clone(), version, evidence(&doc, &binding, version)).unwrap();
    for invalid in [root, seed, removed, NodeId::new()] {
        assert!(matches!(
            page(
                &memory,
                invalid,
                MemoryDetailPosition::default(),
                DetailBudget::default()
            ),
            Err(MemoryCodecError::InvalidDetailPosition)
        ));
    }
    assert!(
        page(
            &memory,
            fact,
            MemoryDetailPosition::default(),
            DetailBudget::default()
        )
        .unwrap()
        .complete
    );
}

#[test]
fn detail_unicode_runs_paginate_with_positive_progress() {
    let (mut doc, binding, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "summary");
    let text = "é🦀\"\\\n".repeat(600);
    let child = doc
        .insert(Parent::Node(fact), koru_core::Node::code(&text).unwrap())
        .unwrap();
    doc.set_mark(child, 0..text.len(), koru_core::Mark::Bold, true)
        .unwrap();
    let memory =
        history::validate(binding.clone(), version, evidence(&doc, &binding, version)).unwrap();
    let mut position = MemoryDetailPosition::default();
    let mut rebuilt = String::new();
    let mut pages = 0;
    loop {
        let result = page(&memory, fact, position, DetailBudget::default()).unwrap();
        assert!(fits(
            &result.chunks,
            result.complete,
            DetailBudget::default()
        ));
        for chunk in &result.chunks {
            if chunk["node_id"] == child.to_storage() {
                let a = chunk["start_utf8"].as_u64().unwrap() as usize;
                let b = chunk["end_utf8"].as_u64().unwrap() as usize;
                assert_eq!(chunk["content"]["text"].as_str().unwrap(), &text[a..b]);
                assert_eq!(a, rebuilt.len());
                rebuilt.push_str(&text[a..b]);
                let runs = chunk["content"]["runs"].as_array().unwrap();
                assert!(runs.iter().all(|run| run["bold"] == true));
                assert_eq!(runs[0]["start_utf8"], 0);
                assert_eq!(runs.last().unwrap()["end_utf8"], b - a);
            }
        }
        pages += 1;
        assert!(pages < 100);
        match result.next {
            None => break,
            Some(next) => {
                assert!(
                    next.node_ordinal > position.node_ordinal
                        || next.start_utf8 > position.start_utf8
                );
                position = next;
            }
        }
    }
    assert!(pages > 1);
    assert_eq!(rebuilt, text);
    for start in [1, text.len() + 1] {
        assert!(matches!(
            page(
                &memory,
                fact,
                MemoryDetailPosition {
                    node_ordinal: 1,
                    start_utf8: start
                },
                DetailBudget::default()
            ),
            Err(MemoryCodecError::InvalidDetailPosition)
        ));
    }
}

#[test]
fn detail_empty_text_advances_and_budget_counts_continuation_framing() {
    let (mut doc, binding, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "summary");
    let empty = doc
        .insert(Parent::Node(fact), koru_core::Node::paragraph("").unwrap())
        .unwrap();
    let memory =
        history::validate(binding.clone(), version, evidence(&doc, &binding, version)).unwrap();
    let result = page(
        &memory,
        fact,
        MemoryDetailPosition {
            node_ordinal: 1,
            start_utf8: 0,
        },
        DetailBudget::default(),
    )
    .unwrap();
    assert!(result.complete);
    assert!(result.next.is_none());
    assert_eq!(result.chunks[0]["node_id"], empty.to_storage());
    assert_eq!(result.chunks[0]["start_utf8"], 0);
    assert_eq!(result.chunks[0]["end_utf8"], 0);
    assert!(
        !fits(
            &[],
            false,
            DetailBudget {
                max_page_bytes: 2048,
                ..Default::default()
            }
        ),
        "reserve itself is not the complete envelope"
    );
    assert!(matches!(
        page(
            &memory,
            fact,
            MemoryDetailPosition::default(),
            DetailBudget {
                max_page_bytes: 1,
                ..Default::default()
            }
        ),
        Err(MemoryCodecError::DetailTooLarge)
    ));
}

#[test]
fn detail_oversize_attributes_are_not_dropped() {
    let (mut doc, binding, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "summary");
    doc.set_attr(
        fact,
        &koru_core::Attr::new("detail", koru_core::AttrValue::Text("x".repeat(5000))),
    )
    .unwrap();
    let memory =
        history::validate(binding.clone(), version, evidence(&doc, &binding, version)).unwrap();
    assert!(matches!(
        page(
            &memory,
            fact,
            MemoryDetailPosition::default(),
            DetailBudget::default()
        ),
        Err(MemoryCodecError::DetailTooLarge)
    ));
    let result = page(
        &memory,
        fact,
        MemoryDetailPosition::default(),
        DetailBudget {
            max_page_bytes: 16384,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        result.chunks[0]["content"]["attributes"][0]["value"]["value"]
            .as_str()
            .unwrap()
            .len(),
        5000
    );
}
