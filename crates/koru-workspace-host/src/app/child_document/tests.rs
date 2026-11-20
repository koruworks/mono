// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::{DocumentId, NodeId, ProfileId, WorkspaceDoc, WorkspaceId};

#[test]
fn focused_window_restores_only_its_resident_document_in_current_scope() {
    let current = WorkspaceId::new();
    let a = DocumentId::new();
    let b = DocumentId::new();
    let resident = |id| Some((Some(current), Some(id)));
    // A's delayed completion may be accepted only after A becomes focused;
    // returning to the ordinary sibling B restores B without shell navigation.
    assert_eq!(
        resident_restore_target(current, Some(b), Some((current, a)), resident),
        Some((current, a))
    );
    assert_eq!(
        resident_restore_target(current, Some(a), Some((current, b)), resident),
        Some((current, b))
    );
    assert_eq!(
        resident_restore_target(current, Some(b), Some((current, b)), resident),
        None
    );
    assert_eq!(
        resident_restore_target(current, Some(b), Some((current, a)), |_| None),
        None
    );
    assert_eq!(
        resident_restore_target(current, Some(b), Some((WorkspaceId::new(), a)), |_| panic!(
            "scope must be checked before resident lookup"
        )),
        None
    );
    assert_eq!(
        resident_restore_target(current, Some(b), Some((current, a)), |_| Some((
            Some(current),
            Some(b)
        ))),
        None
    );
}

#[test]
fn accepted_cold_task_blocks_scope_replacement_and_shutdown_until_returned() {
    let profile = ProfileId::new();
    let current = WorkspaceId::new();
    let command = complete(DocumentId::new());
    let mut queue = TaskQueue::default();
    assert!(!queue.has_pending());
    queue.push(profile, current, command.clone()).unwrap();
    // No preparation starts here. The same guard is checked before close,
    // update close, workspace switch and either profile adoption path.
    assert!(queue.has_pending());
    assert!(queue.in_scope(profile, current));
    let mut workspace = koru_workspace::Workspace::new(WorkspaceDoc::create(current, "W").unwrap());
    // A terminal refusal is returned explicitly, never silently coalesced.
    let (returned, outcome) = queue
        .poll(ProfileId::new(), current, &mut workspace)
        .unwrap();
    assert_eq!(returned, command);
    assert!(outcome.is_err());
    assert!(!queue.has_pending());
}

#[test]
fn accepted_command_keeps_source_identity_but_does_not_need_a_live_window_to_run() {
    let current = WorkspaceId::new();
    let profile = ProfileId::new();
    let document = DocumentId::new();
    let mut workspace = koru_workspace::Workspace::holding_for_tests(
        WorkspaceDoc::create(current, "W").unwrap(),
        document,
    );
    let mut queue = TaskQueue::default();
    let source = WindowId::from(17);
    queue
        .push_from(Some(source), profile, current, complete(document))
        .unwrap();
    // No pane exists for this id. Preparation/command ownership is independent
    // of its optional UI return destination, so closing it loses no command.
    assert_eq!(queue.source(), Some(source));
    let (_, ready) = queue.poll(profile, current, &mut workspace).unwrap();
    assert!(ready.is_ok());
    assert!(!queue.has_pending());
}

fn complete(document: DocumentId) -> crate::tasks::TaskCommand {
    crate::tasks::TaskCommand::Complete {
        document,
        node: NodeId::new(),
        completed: true,
    }
}

#[test]
fn supported_source_kinds_reject_canvas_and_unknown_even_when_warm() {
    for kind in ["page", "document", "note", "task-list"] {
        assert!(supported_kind(Some(kind)));
    }
    for kind in [None, Some("canvas"), Some("file"), Some("future-shape")] {
        assert!(!supported_kind(kind));
    }
}

#[test]
fn queue_preserves_every_accepted_intent_and_refuses_overflow() {
    let profile = ProfileId::new();
    let current = WorkspaceId::new();
    let mut workspace = koru_workspace::Workspace::new(WorkspaceDoc::create(current, "W").unwrap());
    let mut queue = TaskQueue::default();
    let commands: Vec<_> = (0..64).map(|_| complete(DocumentId::new())).collect();
    for command in &commands {
        queue.push(profile, current, command.clone()).unwrap();
    }
    assert!(
        queue
            .push(profile, current, complete(DocumentId::new()))
            .is_err()
    );
    // A scope change cancels before loading. No Store or reader is involved.
    let next = ProfileId::new();
    for command in commands {
        let (returned, result) = queue.poll(next, current, &mut workspace).unwrap();
        assert_eq!(returned, command);
        assert!(result.is_err());
    }
    assert!(queue.poll(next, current, &mut workspace).is_none());
}

#[test]
fn warm_task_preparation_does_not_switch_the_active_document() {
    let profile = ProfileId::new();
    let current = WorkspaceId::new();
    let id = DocumentId::new();
    let mut workspace = koru_workspace::Workspace::holding_for_tests(
        WorkspaceDoc::create(current, "W").unwrap(),
        id,
    );
    let before = workspace.ws.doc().oplog_vv();
    let mut queue = TaskQueue::default();
    let command = complete(id);
    queue.push(profile, current, command.clone()).unwrap();
    let (returned, ready) = queue.poll(profile, current, &mut workspace).unwrap();
    assert_eq!(returned, command);
    ready.unwrap();
    assert_eq!(workspace.active(), Some(id));
    assert_eq!(workspace.ws.doc().oplog_vv(), before);
}
