// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::mainview::harness::container;
use crate::mini_app::artifact::SaveOperation;

#[test]
fn save_action_is_reachable_by_keyboard_and_failure_retains_frozen_operation() {
    let (mut view, mut workspace) = container();
    assert!(view.open_mini_app_fixture());
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.focus_header();
    for _ in 0..2 {
        assert!(View::key(
            app.as_mut(),
            &mut workspace.ws,
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
            None,
            winit::keyboard::ModifiersState::empty()
        ));
    }
    assert!(View::key(
        app.as_mut(),
        &mut workspace.ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        winit::keyboard::ModifiersState::empty()
    ));
    assert!(app.artifact.requested);
    // The ordinary test build disables actual worker startup. Its failure
    // exercises the same exact-operation retention as a full production queue.
    assert!(view.maintain_mini_app_artifacts());
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    assert!(app.artifact.failed);
    assert!(!app.artifact.in_flight);
    assert!(app.artifact.operation.is_some());
    assert!(app.artifact.saved.is_none());
}

#[test]
fn an_accepted_save_blocks_close_until_its_exact_outcome_arrives() {
    let (mut view, _) = container();
    assert!(view.open_mini_app_fixture());
    let (io, commands, answers) = artifact::Io::in_memory();
    view.mini_app_artifacts = io;
    let index = view.tabs().active();
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_artifact_save();
    let count = view.tabs().len();
    assert!(view.close(index));
    assert_eq!(view.tabs().len(), count);
    assert!(!view.prepare_document_creation_close());
    assert!(view.maintain_mini_app_artifacts());
    let Command::Save {
        tab,
        profile,
        operation,
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert!(view.request_close());
    assert_eq!(view.tabs().len(), count);
    answers
        .send(Answer::Saved {
            tab,
            profile,
            workspace: operation.snapshot.bindings.workspace,
            registration: operation.registration.registration_id,
            captured_revision: operation.captured_revision,
            result: Err(artifact::invalid("refused")),
        })
        .unwrap_or_else(|_| panic!("mailbox"));
    view.maintain_mini_app_artifacts();
    assert!(view.prepare_document_creation_close());
    assert!(!view.request_close());
    assert!(view.close(index));
    assert_eq!(view.tabs().len(), count - 1);
}

fn version(op: &SaveOperation) -> koru_artifact_wire::ArtifactVersion {
    koru_artifact_wire::ArtifactVersion {
        id: koru_artifact_wire::ArtifactVersionId::mint(),
        family: op.family,
        ordinal: koru_artifact_wire::Ordinal::FIRST,
        blob: op.registration.blob.clone(),
        size_bytes: op.bytes.len() as u64,
        mime: op.registration.mime.clone(),
        filename: None,
        produced_by: None,
        registration_id: op.registration.registration_id,
        derived_from: op.registration.derived_from,
        created_at_ms: 0,
    }
}
fn opened(op: &SaveOperation, version: koru_artifact_wire::ArtifactVersion) -> Opened {
    let create = op.create.as_ref().unwrap();
    Opened::MiniApp {
        version,
        family: koru_artifact_wire::FamilyResponse {
            family: koru_artifact_wire::ArtifactFamily {
                id: op.family,
                workspace: create.workspace,
                title: create.title.clone(),
                created_by_run: None,
                created_at_ms: 0,
                retired_at_ms: None,
            },
            versions: vec![],
        },
        envelope: op.snapshot.clone(),
    }
}

#[test]
fn immutable_reopen_preserves_a_dirty_or_pending_original_and_uses_fresh_tab_identity() {
    let (mut view, _) = container();
    assert!(view.open_mini_app_fixture());
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    let op = SaveOperation::capture(
        app.artifact_snapshot().unwrap(),
        app.declaration_revision(),
        "Saved",
        None,
    )
    .unwrap();
    let version = version(&op);
    assert!(view.apply_artifact_open(Ok(opened(&op, version.clone()))));
    let first = view.tabs().active_tab().id;
    let count = view.tabs().len();
    assert!(view.apply_artifact_open(Ok(opened(&op, version.clone()))));
    assert_eq!(view.tabs().len(), count);
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.tasks
        .set_target(crate::panes::explore::rail::RailTarget::Bucket(
            koru_core::tasks::Bucket::Later,
        ));
    assert!(view.apply_artifact_open(Ok(opened(&op, version.clone()))));
    assert_eq!(view.tabs().len(), count + 1);
    assert_ne!(view.tabs().active_tab().id, first);
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_document(koru_core::DocumentId::new(), None);
    assert!(view.apply_artifact_open(Ok(opened(&op, version))));
    assert_eq!(view.tabs().len(), count + 2);
}

#[test]
fn exact_save_receipt_settles_the_retained_tab_while_another_workspace_is_current() {
    let (mut view, _) = container();
    assert!(view.open_mini_app_fixture());
    let (io, commands, answers) = artifact::Io::in_memory();
    view.mini_app_artifacts = io;
    let original = view.tabs().active_tab().id;
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_artifact_save();
    assert!(view.maintain_mini_app_artifacts());
    let Command::Save {
        tab,
        profile,
        operation,
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(tab, original);
    let other = koru_core::WorkspaceId::new();
    view.set_pane_context(vec![], profile, vec![], Some(other));
    answers
        .send(Answer::Saved {
            tab,
            profile,
            workspace: operation.snapshot.bindings.workspace,
            registration: operation.registration.registration_id,
            captured_revision: operation.captured_revision,
            result: Ok(version(&operation)),
        })
        .unwrap_or_else(|_| panic!("mailbox"));
    assert!(view.maintain_mini_app_artifacts());
    let app = view
        .panes
        .iter_groups()
        .flat_map(|(_, g)| g.tabs().iter())
        .find_map(|t| match &t.view {
            TabView::MiniApp(app) if t.id == original => Some(app),
            _ => None,
        })
        .unwrap();
    assert!(!app.artifact.in_flight);
    assert!(app.artifact.saved.is_some());
    assert!(app.artifact.operation.is_none());
}

#[test]
fn away_and_back_scope_refuses_an_old_open_answer() {
    let (mut view, _) = container();
    assert!(view.open_mini_app_fixture());
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    let op = SaveOperation::capture(
        app.artifact_snapshot().unwrap(),
        app.declaration_revision(),
        "App",
        None,
    )
    .unwrap();
    let version = version(&op);
    let (io, commands, answers) = artifact::Io::in_memory();
    view.mini_app_artifacts = io;
    assert!(view.request_artifact_open(version.id));
    let Command::Open {
        epoch,
        profile,
        workspace,
        version: id,
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    view.set_pane_context(vec![], profile, vec![], Some(koru_core::WorkspaceId::new()));
    view.set_pane_context(vec![], profile, vec![], Some(workspace));
    let count = view.tabs().len();
    answers
        .send(Answer::Opened {
            epoch,
            profile,
            workspace,
            version: id,
            result: Ok(opened(&op, version)),
        })
        .unwrap_or_else(|_| panic!("mailbox"));
    assert!(!view.maintain_mini_app_artifacts());
    assert_eq!(view.tabs().len(), count);
}

#[test]
fn held_elsewhere_keeps_existing_availability_surface_without_a_gateway_request() {
    let (mut view, _) = container();
    let (io, commands, _) = artifact::Io::in_memory();
    view.mini_app_artifacts = io;
    assert!(view.open_artifact_tab(koru_artifacts_ui::OpenedArtifact {
        version: koru_artifact_wire::ArtifactVersionId::mint(),
        title: "Remote".into(),
        size_bytes: 4,
        produced_by: None,
        held_elsewhere: Some(Some("Other device".into()))
    }));
    assert!(matches!(view.tabs().active_tab().view, TabView::Result(_)));
    assert!(commands.try_recv().is_err());
}

#[test]
fn save_click_freezes_its_original_owner_and_definition_before_dispatch() {
    let (mut view, _) = container();
    assert!(view.open_mini_app_fixture());
    let (io, commands, _answers) = artifact::Io::in_memory();
    view.mini_app_artifacts = io;
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.select_tasks(crate::panes::explore::rail::RailTarget::Bucket(
        koru_core::tasks::Bucket::Completed,
    ));
    let expected = app.artifact_snapshot().unwrap();
    let revision = app.declaration_revision();
    let owner = app.artifact.owner.unwrap();
    app.request_artifact_save();
    let registration = app
        .artifact
        .operation
        .as_ref()
        .unwrap()
        .registration
        .registration_id;
    app.select_tasks(crate::panes::explore::rail::RailTarget::Bucket(
        koru_core::tasks::Bucket::Later,
    ));
    view.set_pane_context(vec![], owner, vec![], Some(koru_core::WorkspaceId::new()));
    assert!(view.maintain_mini_app_artifacts());
    let Command::Save {
        profile, operation, ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(profile, owner);
    assert_eq!(operation.snapshot, expected);
    assert_eq!(operation.captured_revision, revision);
    assert_eq!(operation.registration.registration_id, registration);
}

#[test]
fn missing_local_list_and_foreign_workspace_refuse_without_opening_a_tab() {
    use koru_ai_core::mini_apps::artifact::{ListParameter, Tasks};
    let (mut view, _) = container();
    assert!(view.open_mini_app_fixture());
    let TabView::MiniApp(app) = &view.tabs().active_tab().view else {
        panic!()
    };
    let mut snapshot = app.artifact_snapshot().unwrap();
    snapshot.definition.tasks = Tasks::List {
        parameter: ListParameter::TaskList,
    };
    snapshot.bindings.task_list = Some(uuid::Uuid::new_v4());
    let op = SaveOperation::capture(snapshot, 0, "App", None).unwrap();
    let saved = version(&op);
    let count = view.tabs().len();
    assert!(view.apply_artifact_open(Ok(opened(&op, saved.clone()))));
    assert_eq!(view.tabs().len(), count);
    let mut foreign = op.clone();
    foreign.snapshot.bindings.workspace =
        koru_ai_core::WorkspaceId::from_uuid(uuid::Uuid::new_v4());
    assert!(view.apply_artifact_open(Ok(opened(&foreign, saved))));
    assert_eq!(view.tabs().len(), count);
}
