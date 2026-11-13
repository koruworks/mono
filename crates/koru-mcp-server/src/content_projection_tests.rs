// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn mcp_content_projection_is_byte_shape_compatible() {
    let mut node = Node::paragraph("exact").unwrap();
    let attr = koru_core::Attr::new(
        "link",
        koru_core::AttrValue::Url("https://example.test".into()),
    );
    if let Content::Text { attrs, .. } = &mut node.content {
        attrs.push(attr.clone());
    }
    let runs = vec![koru_core::TextRun {
        start: koru_core::Utf8Pos(0),
        end: koru_core::Utf8Pos(5),
        text: "exact".into(),
        bold: true,
        italic: false,
        underline: false,
        strike: false,
        code: false,
        style: Default::default(),
        pill: None,
    }];
    let expected = json!({"type":"text", "text":"exact", "attributes":[{
        "attribute_id":attr.id.to_storage(),"role":"link","label":null,
        "value":{"type":"url","value":"https://example.test"}
    }],"runs":[{"start_utf8":0,"end_utf8":5,"text":"exact","bold":true,"italic":false,
        "underline":false,"strike":false,"code":false,"script":null,"size":null,"highlight":null,"pill_attribute_id":null}]});
    assert_eq!(
        serde_json::to_vec(&project_node_content(&node.content, &runs)).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    let ordinary = node_json(&node, Parent::Root, 0, 0, &runs);
    assert_eq!(ordinary["content"], expected);
    assert!(ordinary.get("metadata").is_some());
    for content in [
        Content::Embed {
            target: koru_core::Ref::new(NodeId::new()),
        },
        Content::Blob {
            hash: koru_core::BlobHash("opaque".into()),
            mime: "image/png".into(),
            width: Some(1),
            height: Some(2),
            attrs: vec![attr],
        },
    ] {
        node.content = content;
        assert_eq!(
            node_json(&node, Parent::Root, 0, 0, &[])["content"],
            project_node_content(&node.content, &[])
        );
    }
}
