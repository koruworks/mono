// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn copied_files_plant_as_references_without_replacing_existing_text() {
    let workspace = WorkspaceId::new();
    let file = DocumentId::new();
    let mut target = WorkspaceDoc::create(workspace, "target").unwrap();
    let root = target
        .insert(Parent::Root, Node::document("Page").unwrap())
        .unwrap();
    let existing = target
        .insert(
            Parent::Node(root),
            Node::paragraph("Keep this text").unwrap(),
        )
        .unwrap();
    let cuttings = attachment_cuttings(workspace, &[(file, "picture.png".into())]).unwrap();
    let row = target
        .plant(&cuttings[0].0, Parent::Node(root), "")
        .unwrap();
    assert_eq!(
        target.read(existing).unwrap().content.as_text(),
        Some("Keep this text")
    );
    let node = target.read(row).unwrap();
    let attrs = node.content.attrs();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].role, "attachment");
    assert_eq!(attrs[0].label.as_deref(), Some("picture.png"));
    assert_eq!(
        attrs[0].value,
        AttrValue::Ref(Ref::in_doc(
            NodeId::from_uuid(file.as_uuid()),
            DocRef::Document(file)
        ))
    );
}
