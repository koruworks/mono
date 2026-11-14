// SPDX-License-Identifier: MPL-2.0
use super::*;

fn swap(app: &mut MiniApp) {
    let mut root = app
        .effective_definition()
        .envelope
        .surface
        .components
        .remove(0);
    root.arrangement = Some(
        if app.swapped {
            "tasks-first"
        } else {
            "document-first"
        }
        .into(),
    );
    let target = app.refinement_target();
    app.refine(
        target,
        &koru_ai_core::mini_apps::UpdateEnvelope {
            version: "1.0".into(),
            update: koru_ai_core::mini_apps::ComponentUpdate {
                surface_id: target.surface_name(),
                components: vec![root],
            },
        },
    )
    .unwrap();
}
#[test]
fn a_new_pin_refuses_an_older_save_but_an_accepted_pin_keeps_its_capture() {
    let mut app = MiniApp::fixture(koru_core::WorkspaceId::new(), 1).unwrap();
    app.request_artifact_save();
    let snapshot = app.artifact.operation.as_ref().unwrap().snapshot.clone();
    swap(&mut app);
    app.request_pin();
    assert!(app.pin.saving.is_none());
    assert_eq!(app.artifact.notice, Some(koru_i18n::Key::MiniAppPinWait));
    swap(&mut app);
    app.request_pin();
    let registration = app.pin.saving.unwrap();
    swap(&mut app);
    app.request_pin();
    assert_eq!(app.pin.saving, Some(registration));
    assert_eq!(app.artifact.operation.as_ref().unwrap().snapshot, snapshot);
}
#[test]
fn failed_pin_retry_uses_saved_reference_without_new_save() {
    let mut app = MiniApp::fixture(koru_core::WorkspaceId::new(), 1).unwrap();
    let saved = artifact::Saved {
        family: koru_artifact_wire::ArtifactFamilyId::mint(),
        version: koru_artifact_wire::ArtifactVersionId::mint(),
        snapshot: app.artifact_snapshot().unwrap(),
    };
    app.pin.saved(&saved, "Saved app");
    let frozen = app.pin.target.clone();
    app.pin.requested = false;
    app.pin.failed = true;
    swap(&mut app);
    app.request_pin();
    assert_eq!(app.pin.target, frozen);
    assert!(app.pin.requested);
    assert!(app.artifact.operation.is_none());
}
#[test]
fn pin_wait_and_write_have_continuous_close_protection() {
    let mut state = State::default();
    state.saving = Some(koru_artifact_wire::RegistrationId::mint());
    assert!(state.pending());
    state.failed = true;
    assert!(!state.pending());
    state.failed = false;
    state.saving = None;
    state.requested = true;
    assert!(state.pending());
    state.requested = false;
    state.in_flight = true;
    assert!(state.pending());
}
