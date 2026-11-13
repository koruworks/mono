// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::mainview::harness::*;
use koru_core::{DocumentId, node::Node, schema::Parent};
use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot, WindowSlot};

fn fixture() -> (MainView, Workspace, DocumentId, NodeId) {
    let (mut view, mut workspace) = container();
    let root = a_document(&mut workspace.ws, "Source");
    let task = workspace
        .ws
        .insert(Parent::Node(root), Node::task("Fixture task").unwrap())
        .unwrap();
    koru_core::tasks::set_due(
        &mut workspace.ws,
        task,
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::parse("2026-11-12").unwrap()),
    )
    .unwrap();
    let document = DocumentId::new();
    workspace.ws.stamp_document_id(document).unwrap();
    assert!(view.open_mini_app_fixture());
    if let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        let row = koru_store_wire::tasks::TaskRow {
            workspace: koru_store_core::ids::WorkspaceId::from_uuid(
                workspace.ws.id().unwrap().as_uuid(),
            ),
            document: koru_store_core::ids::DocumentId::from_uuid(document.as_uuid()),
            node: task.to_storage(),
            task: format!(
                "task:{}:{}:{}",
                workspace.ws.id().unwrap().to_storage(),
                document.to_storage(),
                task.to_storage()
            ),
            node_kind: "paragraph".into(),
            lifecycle: "open".into(),
            document_trashed: false,
            in_trash: false,
            breadcrumb: "Source".into(),
            title: "Fixture task".into(),
            attributes: vec![],
        };
        app.tasks.seed_for_test(
            vec![crate::tasks_view::query::TaskEntry::of(&row, today, 0).unwrap()],
            today,
        );
    }
    (view, workspace, document, task)
}

fn collect(nodes: &[SemanticNode], ids: &mut std::collections::HashSet<accesskit::NodeId>) {
    for node in nodes {
        assert!(
            ids.insert(node.occurrence.node_id()),
            "duplicate occurrence {:?}",
            node.occurrence
        );
        collect(&node.children, ids);
    }
}

#[test]
fn task_source_lands_in_child_without_a_shell_reservation() {
    let (mut view, workspace, document, task) = fixture();
    let count = view.tabs().len();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: task,
        new_tab: true,
    });
    let request = View::child_document_request(&view).unwrap();
    assert!(view.accept_mini_app_document(request, &workspace.ws));
    assert!(view.land_mini_app_document(request, &workspace.ws));
    assert_eq!(view.tabs().len(), count);
    assert_eq!(view.tabs().active_tab().document, None);
    assert_eq!(view.tabs().active_tab().view.root(), None);
    assert_eq!(view.tabs().active_tab().edits(), Some(document));
    assert!(View::child_document_request(&view).is_none());
}

#[test]
fn newer_intent_and_missing_task_refuse_old_land() {
    let (mut view, workspace, document, _) = fixture();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: NodeId::new(),
        new_tab: false,
    });
    let request = View::child_document_request(&view).unwrap();
    assert!(!view.accept_mini_app_document(request, &workspace.ws));
    assert!(View::child_document_request(&view).is_none());
    assert_eq!(view.tabs().active_tab().edits(), None);
}

#[test]
fn full_and_delta_semantics_keep_document_and_tasks_under_one_content_parent() {
    let (mut view, workspace, document, task) = fixture();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: task,
        new_tab: false,
    });
    let request = View::child_document_request(&view).unwrap();
    assert!(view.land_mini_app_document(request, &workspace.ws));
    view.seat_accessibility(WindowSlot::from_raw(1));
    let slot = ViewSlot::from_raw(1);
    let first = view.semantics(slot, Scope::Whole);
    let mut ids = std::collections::HashSet::new();
    collect(&first, &mut ids);
    let mut second_ids = std::collections::HashSet::new();
    collect(&view.semantics(slot, Scope::Delta), &mut second_ids);
    assert!(second_ids.is_subset(&ids));
    let mut whole_again = std::collections::HashSet::new();
    collect(&view.semantics(slot, Scope::Whole), &mut whole_again);
    assert_eq!(ids, whole_again);
}

fn find_part(
    nodes: &[SemanticNode],
    part: &'static str,
) -> Option<koru_workspace_host::accessibility::Occurrence> {
    nodes.iter().find_map(|node| {
        (node.occurrence.part == koru_workspace_host::accessibility::ControlPart::new(part))
            .then_some(node.occurrence)
            .or_else(|| find_part(&node.children, part))
    })
}

#[test]
fn accessible_task_focus_transfers_from_document_and_completion_uses_canonical_command() {
    let (mut view, mut workspace, document, task) = fixture();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: task,
        new_tab: false,
    });
    let request = View::child_document_request(&view).unwrap();
    assert!(view.land_mini_app_document(request, &workspace.ws));
    view.seat_accessibility(WindowSlot::from_raw(1));
    let nodes = view.semantics(ViewSlot::from_raw(1), Scope::Whole);
    let row = find_part(&nodes, "task-row").expect("task collection row beside document row");
    let check = find_part(&nodes, "task-check").unwrap();
    assert!(View::accessibility_action(
        &mut view,
        &mut workspace.ws,
        row,
        accesskit::Action::Focus,
        None
    ));
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert!(app.tasks_focused());
    assert!(!app.document_focused());
    assert!(View::accessibility_action(
        &mut view,
        &mut workspace.ws,
        check,
        accesskit::Action::Click,
        None
    ));
    let commands = View::take_wants_task_commands(&mut view);
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(commands[0],koru_workspace_host::tasks::TaskCommand::Complete { document: id,node,completed:true } if id == document && node == task)
    );
    let mut owned = Workspace::holding_for_tests(workspace.ws, document);
    let landed = koru_workspace_host::tasks::serve(&mut owned, &commands[0]).unwrap();
    assert_eq!(landed.document, document);
    assert_eq!(
        koru_core::tasks::TaskFields::of(&owned.ws.read(task).unwrap()).lifecycle,
        koru_core::attributes::Lifecycle::Completed
    );
}

#[test]
fn retained_child_reactivation_preserves_region_focus_and_never_opens_shell_tab() {
    let (mut view, workspace, document, task) = fixture();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: task,
        new_tab: false,
    });
    let request = View::child_document_request(&view).unwrap();
    assert!(view.land_mini_app_document(request, &workspace.ws));
    let count = view.tabs().len();
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.focus_region(crate::mini_app::Region::Tasks);
    let owner = app.leaf().unwrap().native_ime_owner();
    app.reactivate();
    let request = View::child_document_request(&view).unwrap();
    assert_eq!(
        View::editing_document(&view),
        Some((workspace.ws.id().unwrap(), document))
    );
    assert!(view.land_mini_app_document(request, &workspace.ws));
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert!(app.tasks_focused());
    assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
    assert_eq!(view.tabs().len(), count);
    assert!(!view.tabs().active_tab().is_reusable());
}

#[test]
fn newest_admission_refusal_preserves_earlier_echo_and_other_windows_intent() {
    use koru_workspace_host::tasks::TaskCommand;
    let (mut view, workspace, document, task) = fixture();
    let first = TaskCommand::Complete {
        document,
        node: task,
        completed: true,
    };
    let rejected = TaskCommand::Complete {
        document,
        node: task,
        completed: false,
    };
    view.queue_task_command(first.clone(), true);
    view.queue_task_command(rejected.clone(), true);
    assert_eq!(
        view.tabs()
            .active_tab()
            .view
            .tasks()
            .unwrap()
            .echoes_for_test(),
        2
    );
    View::task_command_rejected(&mut view, &rejected, "full");
    assert_eq!(view.pending_task_receipts.len(), 1);
    assert_eq!(view.pending_task_receipts.front().unwrap().1, first);
    assert_eq!(view.pending_landings.len(), 1);
    assert_eq!(view.pending_count_deltas.len(), 1);
    assert_eq!(
        view.tabs()
            .active_tab()
            .view
            .tasks()
            .unwrap()
            .echoes_for_test(),
        1
    );
    let (mut sibling, _, other, other_task) = fixture();
    sibling.queue_task_command(
        TaskCommand::Complete {
            document: other,
            node: other_task,
            completed: true,
        },
        true,
    );
    let mut owned = Workspace::holding_for_tests(workspace.ws, document);
    let landed = koru_workspace_host::tasks::serve(&mut owned, &first).unwrap();
    View::task_command_observed(&mut sibling, &landed);
    assert_eq!(sibling.pending_task_receipts.len(), 1);
    assert_eq!(
        sibling
            .tabs()
            .active_tab()
            .view
            .tasks()
            .unwrap()
            .echoes_for_test(),
        1
    );
    View::task_command_landed(&mut view, &landed);
    assert!(view.pending_task_receipts.is_empty());
}

#[test]
fn ordinary_tasks_note_native_owner_can_be_canceled_after_body_focus_moves() {
    use crate::panes::explore::rail::RailTarget;
    use koru_core::ids::{ListId, PageKey, page_document_id};
    use koru_editor::Editing;
    use koru_workspace::pages::{PagePhase, PageState};
    let (mut view, workspace, _, _) = fixture();
    let current = workspace.ws.id().unwrap();
    let list = ListId::new();
    let page = PageKey::List(list);
    let document = page_document_id(current, page, 0);
    let mut doc = WorkspaceDoc::page_genesis(current, document).unwrap();
    doc.stamp_document_id(document).unwrap();
    let mut tasks = crate::tasks_view::TasksView::new(RailTarget::List(list));
    View::set_viewport(&mut tasks, 900.0, 600.0);
    tasks.set_page_state(&PageState {
        page,
        document,
        generation: 0,
        phase: PagePhase::Empty,
        conflict: false,
        note_shown: true,
        attachments: vec![],
    });
    tasks.catch_up(&doc);
    tasks.layout_note_for_test();
    for y in (0..400).step_by(5) {
        View::mouse_down(&mut tasks, &mut doc, 250.0, y as f32, false);
        if tasks.body_is_focused_in(&doc) {
            break;
        }
    }
    assert!(tasks.body_is_focused_in(&doc));
    tasks.body_leaf_mut().unwrap().view_mut().active = true;
    view.tabs_mut().active_mut().view = TabView::Tasks(tasks);
    view.explore.clear_focus();
    let target = View::ime_target(&view)
        .expect("ordinary Tasks note opts in")
        .id;
    let before = doc.version();
    assert!(View::ime(
        &mut view,
        &mut doc,
        target,
        &winit::event::Ime::Preedit("compose".into(), Some((7, 7)))
    ));
    if let TabView::Tasks(tasks) = &mut view.tabs_mut().active_mut().view {
        tasks.begin_capture();
    }
    assert!(View::ime(
        &mut view,
        &mut doc,
        target,
        &winit::event::Ime::Disabled
    ));
    assert_eq!(doc.version(), before);
}

#[test]
fn child_image_import_ticket_is_ended_on_the_retained_leaf() {
    let (mut view, workspace, document, task) = fixture();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: task,
        new_tab: false,
    });
    assert!(
        view.land_mini_app_document(View::child_document_request(&view).unwrap(), &workspace.ws)
    );
    let ticket = koru_workspace_host::ImageTicket(53);
    View::begin_image_import(
        &mut view,
        &workspace.ws,
        ticket,
        &koru_workspace_host::ImagePlace::Append,
    );
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert!(app.leaf().unwrap().holds_image_import(ticket));
    assert!(View::end_image_import(&mut view, ticket));
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert!(!app.leaf().unwrap().holds_image_import(ticket));
}
