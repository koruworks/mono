// SPDX-License-Identifier: MPL-2.0
use super::*;

fn fixture(text: &str) -> (WorkspaceDoc, NodeId) {
    let page = DocumentId::new();
    let mut doc = WorkspaceDoc::page_genesis(WorkspaceId::new(), page).unwrap();
    let node = genesis_seed_id(page);
    doc.set_text(node, text).unwrap();
    (doc, node)
}
fn meta(doc: &WorkspaceDoc, node: NodeId) -> LoroMap {
    doc.nodes().get_meta(doc.tree_id(node).unwrap()).unwrap()
}
fn read(doc: &WorkspaceDoc, node: NodeId) -> ContentResult<NodeContentRead> {
    let before = doc.version();
    let updates = doc.export_all_updates().unwrap();
    let result = doc.read_node_content_strict(node, 8 * 1024 * 1024);
    assert_eq!(doc.version(), before);
    assert_eq!(doc.export_all_updates().unwrap(), updates);
    result
}
fn record(doc: &WorkspaceDoc, node: NodeId, attr: AttrId) -> LoroMap {
    let ValueOrContainer::Container(Container::Map(attrs)) = meta(doc, node).get(K_ATTRS).unwrap()
    else {
        panic!()
    };
    let ValueOrContainer::Container(Container::Map(record)) =
        attrs.get(&attr.to_storage()).unwrap()
    else {
        panic!()
    };
    record
}

#[test]
fn detail_preserves_url_ref_document_targets_and_marks() {
    let (mut doc, node) = fixture("héllo");
    let target = NodeId::new();
    let page = DocumentId::new();
    let attrs = [
        Attr::new("link", AttrValue::Url("https://example.test/a?b=1".into())),
        Attr::new(
            "reference",
            AttrValue::Ref(Ref {
                node: target,
                home: Some(DocRef::Document(page)),
            }),
        ),
        Attr::new(
            "document",
            AttrValue::Document(crate::ids::DocumentReference {
                document: page,
                node: Some(target),
            }),
        ),
    ];
    for attr in &attrs {
        doc.set_attr(node, attr).unwrap();
    }
    doc.set_mark(node, 0..6, Mark::Bold, true).unwrap();
    doc.set_script(node, 0..6, Some(Script::Super)).unwrap();
    doc.set_size(node, 0..6, Some(14.0)).unwrap();
    doc.set_highlight(node, 0..6, Some("yellow")).unwrap();
    let value = read(&doc, node).unwrap();
    let Content::Text {
        text,
        attrs: actual,
    } = value.content
    else {
        panic!()
    };
    assert_eq!(text, "héllo");
    for attr in attrs {
        assert!(actual.contains(&attr));
    }
    assert!(value.runs[0].bold);
    assert_eq!(value.runs[0].style.script, Some(Script::Super));
    assert_eq!(value.runs[0].style.size, Some(14.0));
    assert_eq!(value.runs[0].style.highlight.as_deref(), Some("yellow"));
}

#[test]
fn detail_unknown_attr_or_mark_refuses_without_repair() {
    let (mut doc, node) = fixture("hello");
    let attr = Attr::new("fact", AttrValue::Text("known".into()));
    doc.set_attr(node, &attr).unwrap();
    record(&doc, node, attr.id)
        .insert("future_field", "unknown")
        .unwrap();
    assert_eq!(read(&doc, node), Err(C::UnsupportedDetail));
    record(&doc, node, attr.id).delete("future_field").unwrap();
    record(&doc, node, attr.id)
        .insert(K_ATTR_TYPE, "future_type")
        .unwrap();
    assert_eq!(read(&doc, node), Err(C::UnsupportedDetail));
    record(&doc, node, attr.id)
        .insert(K_ATTR_TYPE, "text")
        .unwrap();
    // Model bytes produced by a newer writer that knows this mark. Loro
    // requires write-time configuration even though readers retain unknowns.
    let mut styles = loro::StyleConfigMap::new();
    styles.insert(
        "future_mark".into(),
        loro::StyleConfig {
            expand: loro::ExpandType::None,
        },
    );
    doc.doc.config_text_style(styles);
    doc.text_handle(node)
        .unwrap()
        .mark_utf8(0..5, "future_mark", true)
        .unwrap();
    assert_eq!(read(&doc, node), Err(C::UnsupportedDetail));
    let reloaded = WorkspaceDoc::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
    assert_eq!(read(&reloaded, node), Err(C::UnsupportedDetail));
}

#[test]
fn detail_stale_known_attribute_fields_follow_model() {
    let (mut doc, node) = fixture("body");
    let attr = Attr::new(
        "document",
        AttrValue::Document(crate::ids::DocumentReference {
            document: DocumentId::new(),
            node: Some(NodeId::new()),
        }),
    );
    doc.set_attr(node, &attr).unwrap();
    let changed = Attr {
        value: AttrValue::Text("plain now".into()),
        ..attr
    };
    doc.set_attr(node, &changed).unwrap();
    assert!(record(&doc, node, changed.id).get(K_ATTR_HOME).is_some());
    let Content::Text { attrs, .. } = read(&doc, node).unwrap().content else {
        panic!()
    };
    assert_eq!(attrs, [changed]);
}

#[test]
fn detail_missing_text_and_pill_target_refuse() {
    let (doc, node) = fixture("body");
    meta(&doc, node).delete(K_TEXT).unwrap();
    assert_eq!(read(&doc, node), Err(C::InvalidContent));
    let (mut doc, node) = fixture("");
    let attr = Attr::new("link", AttrValue::Url("https://example.test".into()));
    doc.insert_pill(node, 0, &attr).unwrap();
    let ValueOrContainer::Container(Container::Map(attrs)) = meta(&doc, node).get(K_ATTRS).unwrap()
    else {
        panic!()
    };
    attrs.delete(&attr.id.to_storage()).unwrap();
    assert_eq!(read(&doc, node), Err(C::InvalidContent));
}

#[test]
fn detail_blob_and_embed_are_metadata_only() {
    let (doc, node) = fixture("stale text");
    let map = meta(&doc, node);
    map.insert(K_BLOB, "opaque-hash").unwrap();
    map.insert(K_MIME, "image/png").unwrap();
    map.insert(K_WIDTH, 20_i64).unwrap();
    map.insert(K_HEIGHT, 30_i64).unwrap();
    assert!(matches!(
        read(&doc, node).unwrap().content,
        Content::Blob {
            width: Some(20),
            height: Some(30),
            ..
        }
    ));
    map.insert(K_WIDTH, -1_i64).unwrap();
    assert_eq!(read(&doc, node), Err(C::InvalidContent));
    let target = NodeId::new();
    map.insert(K_EMBED, target.to_storage()).unwrap();
    assert_eq!(
        read(&doc, node).unwrap().content,
        Content::Embed {
            target: Ref::new(target)
        }
    );
}

#[test]
fn detail_metadata_or_run_budget_refuses() {
    let (mut doc, node) = fixture("text");
    doc.set_attr(
        node,
        &Attr::new("big", AttrValue::Text("x".repeat(METADATA_LIMIT))),
    )
    .unwrap();
    assert_eq!(read(&doc, node), Err(C::TooLarge));
    let (mut doc, node) = fixture("");
    let attr = Attr::new("link", AttrValue::Text("target".into()));
    doc.set_attr(node, &attr).unwrap();
    let text = doc.text_handle(node).unwrap();
    text.insert_utf8(0, &ANCHOR_STR.repeat(RUN_LIMIT + 1))
        .unwrap();
    text.mark_utf8(0..text.len_utf8(), PILL_KEY, attr.id.to_storage())
        .unwrap();
    assert_eq!(read(&doc, node), Err(C::TooLarge));
    assert_eq!(doc.read_node_content_strict(node, 0), Err(C::TooLarge));
}

#[test]
fn detail_adjacent_same_target_pills_preserve_anchors() {
    let (mut doc, node) = fixture("");
    let attr = Attr::new("link", AttrValue::Url("https://example.test".into()));
    doc.insert_pill(node, 0, &attr).unwrap();
    doc.insert_pill(node, ANCHOR_LEN, &attr).unwrap();
    let value = read(&doc, node).unwrap();
    assert_eq!(value.runs.len(), 2);
    for (index, run) in value.runs.iter().enumerate() {
        assert_eq!(run.text, ANCHOR_STR);
        assert_eq!(run.pill, Some(attr.id));
        assert_eq!(run.range(), index * ANCHOR_LEN..(index + 1) * ANCHOR_LEN);
    }
}

#[test]
fn detail_mark_metadata_and_f32_overflow_refuse() {
    let (doc, node) = fixture("text");
    let text = doc.text_handle(node).unwrap();
    text.mark_utf8(0..4, SIZE_KEY, f64::MAX).unwrap();
    assert_eq!(read(&doc, node), Err(C::InvalidContent));
    text.mark_utf8(0..4, SIZE_KEY, 12.0).unwrap();
    text.mark_utf8(0..4, HIGHLIGHT_KEY, "x".repeat(METADATA_LIMIT))
        .unwrap();
    assert_eq!(read(&doc, node), Err(C::TooLarge));
}

#[test]
fn detail_ordinary_apply_and_clear_marks_remains_readable() {
    let (mut doc, node) = fixture("ordinary");
    doc.set_mark(node, 0..8, Mark::Bold, true).unwrap();
    doc.set_script(node, 0..8, Some(Script::Super)).unwrap();
    doc.set_size(node, 0..8, Some(14.0)).unwrap();
    doc.set_highlight(node, 0..8, Some("yellow")).unwrap();
    read(&doc, node).unwrap();
    doc.set_mark(node, 0..8, Mark::Bold, false).unwrap();
    doc.set_script(node, 0..8, None).unwrap();
    doc.set_size(node, 0..8, None).unwrap();
    doc.set_highlight(node, 0..8, None).unwrap();
    let value = read(&doc, node).unwrap();
    assert!(
        value
            .runs
            .iter()
            .all(|run| !run.bold && run.style == TextStyle::default())
    );
    let reloaded = WorkspaceDoc::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
    assert_eq!(read(&reloaded, node).unwrap(), value);
}
