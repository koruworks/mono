// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::mainview::harness::*;
use koru_core::{DocumentId, node::Node, schema::Parent};
use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot, WindowSlot};

#[test]
fn visible_split_task_panels_refresh_without_keyboard_focus() {
    use crate::mainview::panes::{PanePath, SplitDir};
    let (mut view, _workspace, _, _) = fixture();
    let target = view.tabs().active_tab().view.tasks().unwrap().target();
    view.panes
        .split(&PanePath::root(), SplitDir::Right, Tab::tasks(target));
    let left = PanePath::root().child(0);
    let right = PanePath::root().child(1);
    view.size_visible_groups();
    view.panes.set_focus(right.clone());
    let now = Instant::now();
    view.maintain_task_pages(now);
    for path in [&left, &right] {
        let panel = view
            .panes
            .group(path)
            .unwrap()
            .tabs()
            .active_tab()
            .view
            .tasks()
            .unwrap()
            .panel();
        assert!(panel.deadline().is_some(), "both visible splits refresh");
    }
    // Invalidation must schedule the unfocused mini app, even with a
    // non-task tab focused beside it. No client or Store is used here.
    view.tabs_mut().push(Tab::settings());
    view.tasks_changed();
    view.maintain_task_pages(now);
    let deadline = view
        .panes
        .group(&left)
        .unwrap()
        .tabs()
        .active_tab()
        .view
        .tasks()
        .unwrap()
        .panel()
        .deadline()
        .unwrap();
    assert_eq!(view.task_pages_deadline(), Some(deadline));
    let hidden = view
        .panes
        .group(&right)
        .unwrap()
        .tabs()
        .iter()
        .find_map(|tab| tab.view.tasks())
        .unwrap();
    assert_eq!(hidden.panel().deadline(), None, "hidden tabs have no clock");
    // A collapsed or foreign-workspace mini app must not query either.
    let TabView::MiniApp(app) = &mut view
        .panes
        .group_mut(&left)
        .unwrap()
        .tabs_mut()
        .active_mut()
        .view
    else {
        panic!()
    };
    app.regions.tasks.w = 0.0;
    view.maintain_task_pages(now);
    assert_eq!(view.task_pages_deadline(), None);
}

#[test]
fn visible_split_note_edits_refresh_both_hosts_without_activation() {
    use crate::mainview::panes::{PanePath, SplitDir};
    let (mut view, mut workspace, document, task) = fixture();
    let root = {
        let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
            panic!()
        };
        assert!(app.land(&workspace.ws, document));
        app.leaf().unwrap().body().root.unwrap()
    };
    view.panes.split(
        &PanePath::root(),
        SplitDir::Right,
        Tab::document_of(
            Leaf::document(&workspace.ws, root),
            "Source",
            Some(document),
        ),
    );
    let left = PanePath::root().child(0);
    let right = PanePath::root().child(1);
    view.size_visible_groups();
    // Keep a third, hidden editor over the same document to guard against
    // making every local edit lay out every open tab.
    view.panes
        .group_mut(&right)
        .unwrap()
        .tabs_mut()
        .push(Tab::document_of(
            Leaf::document(&workspace.ws, root),
            "Hidden",
            Some(document),
        ));
    view.panes.group_mut(&right).unwrap().tabs_mut().select(0);
    for (source, receiver, text, bold) in [
        (&left, &right, "Edited in mini app", true),
        (&right, &left, "Edited in tab", false),
    ] {
        view.panes.set_focus(source.clone());
        workspace.ws.set_text(task, text).unwrap();
        workspace
            .ws
            .set_mark(task, 0..text.len(), koru_core::Mark::Bold, bold)
            .unwrap();
        // The originating editor has already settled its local gesture.
        let leaf = view
            .panes
            .group_mut(source)
            .unwrap()
            .tabs_mut()
            .active_mut()
            .view
            .retained_leaf_mut()
            .unwrap();
        leaf.body_mut().defer_pending(&workspace.ws);
        leaf.catch_up(&workspace.ws);
        let owner = leaf.native_ime_owner();
        let reflows = leaf.view().reflows;
        // This is the pre-draw path, with no View::adopt or activation event.
        view.catch_up_active(&workspace.ws);
        let leaf = view
            .panes
            .group_mut(receiver)
            .unwrap()
            .tabs_mut()
            .active_mut()
            .view
            .retained_leaf_mut()
            .unwrap();
        let row = leaf.body().rows.iter().find(|row| row.id == task).unwrap();
        assert_eq!(row.text, text);
        assert!(row.runs.iter().all(|run| run.bold == bold));
        let receiver_reflows = leaf.view().reflows;
        view.catch_up_active(&workspace.ws);
        let leaf = view
            .panes
            .group_mut(receiver)
            .unwrap()
            .tabs_mut()
            .active_mut()
            .view
            .retained_leaf_mut()
            .unwrap();
        assert_eq!(
            leaf.view().reflows,
            receiver_reflows,
            "idle draws do not reflow"
        );
        let leaf = view
            .panes
            .group_mut(source)
            .unwrap()
            .tabs_mut()
            .active_mut()
            .view
            .retained_leaf_mut()
            .unwrap();
        assert_eq!(
            leaf.view().reflows,
            reflows,
            "do not reflow the input owner again"
        );
        assert_eq!(leaf.native_ime_owner(), owner);
        let hidden = view
            .panes
            .group_mut(&right)
            .unwrap()
            .tabs_mut()
            .get_mut(1)
            .unwrap()
            .view
            .retained_leaf_mut()
            .unwrap();
        assert_eq!(
            hidden
                .body()
                .rows
                .iter()
                .find(|row| row.id == task)
                .unwrap()
                .text,
            "Fixture task"
        );
    }
}

#[test]
fn cached_mini_app_documents_remain_owned_until_the_tab_closes() {
    let (mut view, workspace, first, _) = fixture();
    let workspace_id = workspace.ws.id().unwrap();
    let second = DocumentId::new();
    let mut other = koru_core::schema::WorkspaceDoc::create(workspace_id, "second").unwrap();
    koru_workspace::document(&mut other, "Second").unwrap();
    other.stamp_document_id(second).unwrap();
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    assert!(app.land(&workspace.ws, first));
    let owner = app.leaf().unwrap().native_ime_owner();
    app.request_document(second, None);
    let request = View::child_document_request(&view).unwrap();
    assert!(view.land_mini_app_document(request, &other));
    assert!(view.shown_documents().contains(&first));
    assert!(view.shown_documents().contains(&second));
    assert!(
        !view.pane_requests.iter().any(
            |r| matches!(r, koru_workspace_host::PaneRequest::CloseDocument(id) if *id == first)
        )
    );
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_document(first, None);
    let request = View::child_document_request(&view).unwrap();
    assert!(view.land_mini_app_document(request, &workspace.ws));
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert_eq!(app.leaf().unwrap().native_ime_owner(), owner);
    let index = view.tabs().active();
    view.close(index);
    for document in [first, second] {
        assert!(view.pane_requests.iter().any(
            |r| matches!(r, koru_workspace_host::PaneRequest::CloseDocument(id) if *id == document)
        ));
    }
}

#[test]
fn mini_app_scripted_refinement_reaches_retained_host_and_refuses_stale_or_closed_target() {
    check_scripted_refinement(false);
}
#[test]
fn mini_app_mcp_refinement_reaches_same_retained_host() {
    check_scripted_refinement(true);
}
fn check_scripted_refinement(mcp: bool) {
    use koru_ai_core::{
        AiToolExecutionContext, ChatId, InferenceAttemptId, ToolInvocationId, mini_apps,
    };
    let (mut view, workspace, document, task) = fixture();
    view.handle_tasks_request_for_test(crate::tasks_view::Request::Open {
        document,
        node: task,
        new_tab: false,
    });
    let request = View::child_document_request(&view).unwrap();
    assert!(view.land_mini_app_document(request, &workspace.ws));
    view.ai_panel.set_immediately(true);
    view.sync_mini_app_refinement_target();
    let (_, profile, _, Some(workspace_id)) = view.pane_context.as_ref().unwrap() else {
        panic!()
    };
    let profile = koru_ai_core::ProfileId::from_uuid(profile.as_uuid());
    let workspace_id = koru_ai_core::WorkspaceId::from_uuid(workspace_id.as_uuid());
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    let host = view
        .ai_chat
        .arm_mini_app_for_test(profile, workspace_id, chat, attempt);
    let mut recorded: serde_json::Value =
        serde_json::from_str(mini_apps::REFINEMENT_FIXTURE).unwrap();
    assert_eq!(
        recorded["prompt"],
        "Put the document on the left and tasks on the right"
    );
    recorded["result"]["updateComponents"]["surfaceId"] =
        host.refinement.unwrap().surface_name().into();
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    let leaf_owner = app.leaf().unwrap().native_ime_owner();
    let snapshot = app.refinement_context();
    assert_eq!(Some(snapshot.target), host.refinement);
    let task_owner = &app.tasks as *const crate::tasks_view::TasksView;
    let count = view.tabs().len();
    let events = (if mcp {
        koru_ai_server::scripted_mcp_refinement
    } else {
        koru_ai_server::scripted_native_refinement
    })(
        AiToolExecutionContext {
            chat_id: chat,
            attempt_id: attempt,
            invocation_id: ToolInvocationId::new(),
            profile_id: Some(profile),
            workspace_ids: vec![workspace_id],
            mini_app: Some(mini_apps::Authority::with_refinement(host, Some(snapshot)).unwrap()),
        },
        recorded["result"].clone(),
    )
    .unwrap();
    for event in events.clone() {
        let wire = serde_json::to_vec(&event).unwrap();
        view.ai_chat
            .receive_mini_app_event_for_test(serde_json::from_slice(&wire).unwrap());
    }
    assert!(view.open_pending_mini_apps());
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert_eq!(app.leaf().unwrap().native_ime_owner(), leaf_owner);
    assert_eq!(
        &app.tasks as *const crate::tasks_view::TasksView,
        task_owner
    );
    assert!(app.regions.document.x < app.regions.tasks.x);
    assert_eq!(view.tabs().len(), count);
    assert!(
        !view
            .pane_requests
            .iter()
            .any(|r| matches!(r, PaneRequest::Navigate { .. }))
    );
    let stale = koru_ai_ui::MiniAppRefinement {
        host,
        profile,
        chat,
        envelope: mini_apps::UpdateEnvelope::from_value(&recorded["result"]).unwrap(),
    };
    assert!(!view.apply_mini_app_refinement(&stale));
    for event in events {
        view.ai_chat.receive_mini_app_event_for_test(event);
    }
    assert!(!view.open_pending_mini_apps());
    // Closing/replacing the selected native host cannot retarget the old result.
    view.tabs_mut().push(Tab::welcome());
    assert!(!view.apply_mini_app_refinement(&stale));
}

#[test]
fn mini_app_scripted_model_dispatches_references_through_stream_and_same_native_host() {
    check_scripted_creation(false);
}
#[test]
fn mini_app_mcp_creation_dispatches_references_through_stream_and_same_native_host() {
    check_scripted_creation(true);
}
fn check_scripted_creation(mcp: bool) {
    use koru_ai_core::mini_apps::{self, Authority};
    use koru_ai_core::{AiToolExecutionContext, ChatId, InferenceAttemptId, ToolInvocationId};
    let (mut view, _workspace) = container();
    let (_, profile, _, Some(workspace)) = view.pane_context.as_ref().unwrap() else {
        panic!()
    };
    let profile = koru_ai_core::ProfileId::from_uuid(profile.as_uuid());
    let workspace = koru_ai_core::WorkspaceId::from_uuid(workspace.as_uuid());
    let mut system = koru_core::system::SystemDoc::create().unwrap();
    let list = system.create_list("Travel", None, 1).unwrap();
    view.list_table = system.lists();
    assert!(view.open_ai_chat_tab());
    let before = view.tabs().len();
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    let document = koru_ai_core::DocumentId::new();
    view.entries = Some(vec![document_entry(DocumentId::from_uuid(
        document.as_uuid(),
    ))]);
    let TabView::AiChat(origin) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    let host = origin
        .dictation_surface_mut()
        .arm_mini_app_for_test(profile, workspace, chat, attempt);
    let mut declaration: serde_json::Value = serde_json::from_str(mini_apps::FIXTURE).unwrap();
    declaration["createSurface"]["components"][0]["arrangement"] = "document-first".into();
    declaration["createSurface"]["components"][2]
        .as_object_mut()
        .unwrap()
        .remove("bucket");
    declaration["createSurface"]["components"][2]["list"] =
        serde_json::json!({"workspace":"current","listId":list.as_uuid()});
    declaration["createSurface"]["components"][4]["document"] =
        serde_json::json!({"workspace":"current","documentId":document});
    let context = AiToolExecutionContext {
        chat_id: chat,
        attempt_id: attempt,
        invocation_id: ToolInvocationId::new(),
        profile_id: Some(profile),
        workspace_ids: vec![workspace],
        mini_app: Some(Authority::new(host)),
    };
    let events = (if mcp {
        koru_ai_server::scripted_mcp_creation
    } else {
        koru_ai_server::scripted_native_creation
    })(
        context,
        declaration,
        vec![(list.as_uuid(), "Travel".into())],
        vec![document],
    )
    .unwrap();
    for event in events.clone() {
        let wire = serde_json::to_vec(&event).unwrap();
        let event = serde_json::from_slice(&wire).unwrap();
        origin
            .dictation_surface_mut()
            .receive_mini_app_event_for_test(event);
    }
    assert!(view.open_pending_mini_apps());
    assert_eq!(view.tabs().len(), before + 1);
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    assert_eq!(
        app.tasks.target(),
        crate::panes::explore::rail::RailTarget::List(list)
    );
    assert_eq!(
        app.pending.unwrap().0.document.as_uuid(),
        document.as_uuid()
    );
    View::set_viewport(app.as_mut(), 1200.0, 700.0);
    assert!(app.regions.document.x < app.regions.tasks.x);
    // Ordinary host operations are local; this fixture has no client/provider
    // reader, and the requesting chat no longer owns a creation delivery.
    app.tasks
        .set_target(crate::panes::explore::rail::RailTarget::Bucket(
            crate::panes::explore::rail::Bucket::Next,
        ));
    let count = view.tabs().len();
    for tab in view.tabs_mut().iter_mut() {
        if let TabView::AiChat(origin) = &mut tab.view {
            for event in events.clone() {
                origin
                    .dictation_surface_mut()
                    .receive_mini_app_event_for_test(event);
            }
        }
    }
    assert!(!view.open_pending_mini_apps());
    assert_eq!(view.tabs().len(), count);
    assert!(
        !view
            .pane_requests
            .iter()
            .any(|r| matches!(r, PaneRequest::Navigate { .. }))
    );
}

fn document_entry(document: DocumentId) -> koru_core::explore::ExploreEntry {
    koru_core::explore::ExploreEntry {
        id: document,
        parent: None,
        ordinal: 0,
        title: "Source".into(),
        kind: koru_core::explore::KIND_DOCUMENT.into(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    }
}

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
    view.entries = Some(vec![document_entry(document)]);
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
