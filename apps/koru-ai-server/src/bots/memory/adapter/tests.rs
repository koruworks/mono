// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::memory::history::tests::evidence;
use crate::bots::memory::tests::{add_fact, fixture};

mod store;

#[test]
fn control_heavy_whole_summary_refuses_without_skip_or_truncation() {
    let (mut doc, page, version, root, _) = fixture();
    add_fact(&mut doc, root, &"\u{0001}".repeat(1024));
    add_fact(&mut doc, root, "Later readable fact");
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    assert_eq!(verified.source.facts.len(), 2);
    assert_eq!(
        projection::page(
            &verified,
            MemoryReadPosition::List {
                after: 0,
                limit: 20
            }
        ),
        Err(BotError::ContextTooLarge.into())
    );
}

#[test]
fn maximum_escaped_summary_progresses_without_skipping() {
    let (mut doc, page, version, root, _) = fixture();
    let summary = "\"\\".repeat(512);
    add_fact(&mut doc, root, &summary);
    add_fact(&mut doc, root, "Next fact");
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    let MemoryPageResult::List { mut page, next } =
        projection::page(&verified, MemoryReadPosition::List { after: 0, limit: 1 }).unwrap()
    else {
        panic!()
    };
    let BotMemoryListPage::Observed {
        items,
        continuation,
        ..
    } = &mut page
    else {
        panic!()
    };
    assert_eq!(items[0].summary, summary);
    assert_eq!(next, Some(MemoryReadPosition::List { after: 1, limit: 1 }));
    *continuation = Some("a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES));
    assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
}

#[test]
fn memory_list_pages_bound_escaped_envelope_and_advance() {
    let (mut doc, page, version, root, _) = fixture();
    for i in 0..45 {
        add_fact(&mut doc, root, &format!("{i}: {}", "\"\\".repeat(200)));
    }
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    let mut position = MemoryReadPosition::List {
        after: 0,
        limit: 20,
    };
    let mut found = Vec::new();
    loop {
        let MemoryPageResult::List { mut page, next } =
            projection::page(&verified, position).unwrap()
        else {
            panic!()
        };
        let BotMemoryListPage::Observed {
            items,
            continuation,
            complete,
            ..
        } = &mut page
        else {
            panic!()
        };
        assert!(!items.is_empty());
        found.extend(items.iter().map(|item| item.fact_id));
        assert_eq!(*complete, next.is_none());
        *continuation = next
            .as_ref()
            .map(|_| "a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES));
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        let Some(next) = next else { break };
        position = next;
    }
    assert_eq!(found.len(), 45);
    assert_eq!(
        found.iter().collect::<std::collections::HashSet<_>>().len(),
        45
    );
}

#[test]
fn memory_list_includes_prompt_omitted_facts_and_validates_positions() {
    let (mut doc, page, version, root, _) = fixture();
    for i in 0..260 {
        add_fact(&mut doc, root, &format!("Fact {i}"));
    }
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    let MemoryPageResult::List {
        page:
            BotMemoryListPage::Observed {
                counts,
                items,
                complete,
                ..
            },
        ..
    } = projection::page(
        &verified,
        MemoryReadPosition::List {
            after: 259,
            limit: 20,
        },
    )
    .unwrap()
    else {
        panic!()
    };
    assert_eq!(counts.valid, 260);
    assert!(counts.omitted_from_prompt > 0);
    assert_eq!(items.len(), 1);
    assert!(complete);
    for position in [
        MemoryReadPosition::List {
            after: 261,
            limit: 1,
        },
        MemoryReadPosition::List { after: 0, limit: 0 },
        MemoryReadPosition::List {
            after: 0,
            limit: 21,
        },
    ] {
        assert_eq!(
            projection::page(&verified, position),
            Err(BotError::Invalid.into())
        );
    }
}

#[test]
fn memory_detail_pages_preserve_original_ranges_and_have_full_wire_bound() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "Fact");
    let body = "雪\"\\".repeat(1500);
    let detail = doc
        .insert(
            koru_core::Parent::Node(fact),
            koru_core::Node::paragraph(&body).unwrap(),
        )
        .unwrap();
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    let mut position = MemoryReadPosition::Detail {
        fact_id: BotMemoryNodeId::from_uuid(fact.as_uuid()),
        node_ordinal: 0,
        start_utf8: 0,
    };
    let mut end = 0;
    loop {
        let MemoryPageResult::Detail { mut page, next } =
            projection::page(&verified, position).unwrap()
        else {
            panic!()
        };
        let BotMemoryDetailPage::Observed {
            chunks,
            continuation,
            ..
        } = &mut page
        else {
            panic!()
        };
        assert!(!chunks.is_empty());
        for chunk in chunks
            .iter()
            .filter(|chunk| chunk.node_id.as_uuid() == detail.as_uuid())
        {
            let range = chunk.range.as_ref().unwrap();
            assert_eq!(range.start_utf8, end);
            assert!(range.end_utf8 > end);
            end = range.end_utf8;
            assert!(body.is_char_boundary(end as usize));
        }
        *continuation = next
            .as_ref()
            .map(|_| "a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES));
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        let Some(next) = next else { break };
        position = next;
    }
    assert_eq!(end as usize, body.len());
}

#[test]
fn memory_replay_checks_every_durable_index_claim() {
    let (mut doc, page, version, root, _) = fixture();
    add_fact(&mut doc, root, "Fact");
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    let index = index::encode_index(&verified.source).unwrap();
    let nodes: Vec<_> = index
        .nodes
        .iter()
        .map(|node| BotMemoryNodeId::from_uuid(node.as_uuid()))
        .collect();
    assert!(
        projection::verify_index(
            &index,
            &nodes,
            index.omitted_count,
            index.invalid_count,
            index.digest
        )
        .is_ok()
    );
    for (nodes, omitted, invalid, digest) in [
        (
            Vec::new(),
            index.omitted_count,
            index.invalid_count,
            index.digest,
        ),
        (
            nodes.clone(),
            index.omitted_count + 1,
            index.invalid_count,
            index.digest,
        ),
        (
            nodes.clone(),
            index.omitted_count,
            index.invalid_count + 1,
            index.digest,
        ),
        (nodes, index.omitted_count, index.invalid_count, [0; 32]),
    ] {
        assert_eq!(
            projection::verify_index(&index, &nodes, omitted, invalid, digest),
            Err(MemoryAuthorityError::CaptureUnavailable)
        );
    }
}

#[test]
fn memory_detail_missing_fact_is_not_an_empty_page() {
    let (doc, page, version, root, _) = fixture();
    let verified =
        history::validate(page.clone(), version, evidence(&doc, &page, version)).unwrap();
    assert_eq!(
        projection::page(
            &verified,
            MemoryReadPosition::Detail {
                fact_id: BotMemoryNodeId::from_uuid(root.as_uuid()),
                node_ordinal: 0,
                start_utf8: 0,
            }
        ),
        Err(MemoryAuthorityError::FactNotFound)
    );
}
