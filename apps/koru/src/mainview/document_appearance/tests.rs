// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_wire::attributes::AttributeRowValue;

fn row() -> AttributeRow {
    AttributeRow {
        workspace: koru_store_core::ids::WorkspaceId::new(),
        document: koru_store_core::ids::DocumentId::new(),
        node: NodeId::new().to_storage(),
        node_kind: "document".into(),
        excerpt: "A document".into(),
        document_title: "A document".into(),
        trashed: false,
        attributes: [(KEY_COLOR, "purple"), (KEY_PRIORITY, "urgent")]
            .into_iter()
            .map(|(role, value)| AttributeRowValue {
                id: role.into(),
                role: role.into(),
                kind: "text".into(),
                value: value.into(),
                zone: None,
                label: None,
            })
            .collect(),
    }
}

#[test]
fn decorations_belong_to_document_roots_not_tasks_inside_them() {
    let mut row = row();
    let id = NodeId::from_uuid(row.document.as_uuid());
    let mut rows = HashMap::new();
    collect(&mut rows, &row);
    assert_eq!(rows[&id].color.as_deref(), Some("purple"));
    assert_eq!(rows[&id].priority, Some(Priority::Urgent));
    row.node_kind = "task".into();
    row.attributes[0].value = "red".into();
    collect(&mut rows, &row);
    assert_eq!(rows[&id].color.as_deref(), Some("purple"));
    rows.clear();
    row.node_kind = "document".into();
    row.trashed = true;
    collect(&mut rows, &row);
    assert!(rows.is_empty());
}

#[test]
fn delayed_projection_cannot_undo_a_local_edit_or_reappear_in_another_workspace() {
    let workspace = WorkspaceId::new();
    let id = NodeId::new();
    let mut reader = DocumentAppearances::default();
    reader.set_workspace(Some(workspace));
    let local = DocumentAppearance {
        color: Some("purple".into()),
        priority: Some(Priority::High),
    };
    reader.observe(id, local.clone());
    let (send, receive) = mpsc::channel();
    reader.receiver = Some(receive);
    send.send(Answer {
        workspace,
        generation: reader.generation,
        rows: Ok(HashMap::new()),
    })
    .unwrap();
    reader.maintain(true, Instant::now());
    assert_eq!(reader.rows.get(&id), Some(&local));

    let (send, receive) = mpsc::channel();
    reader.receiver = Some(receive);
    send.send(Answer {
        workspace,
        generation: reader.generation,
        rows: Ok(reader.rows.clone()),
    })
    .unwrap();
    reader.maintain(true, Instant::now());
    assert!(reader.pending.is_empty());

    let (send, receive) = mpsc::channel();
    reader.receiver = Some(receive);
    send.send(Answer {
        workspace,
        generation: reader.generation,
        rows: Ok(reader.rows.clone()),
    })
    .unwrap();
    reader.set_workspace(Some(WorkspaceId::new()));
    reader.maintain(true, Instant::now());
    assert!(reader.rows.is_empty());
    assert!(reader.deadline().is_none(), "no client means no polling");
}
