// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_artifact_wire::{ArtifactFamily, Ordinal};
use std::cell::{Cell, RefCell};

fn snapshot() -> Envelope {
    Envelope::capture(
        koru_ai_core::WorkspaceId::from_uuid(uuid::Uuid::new_v4()),
        &koru_ai_core::mini_apps::Declaration::parse(koru_ai_core::mini_apps::FIXTURE.as_bytes())
            .unwrap(),
    )
    .unwrap()
}
struct Fake {
    family: RefCell<Option<ArtifactFamily>>,
    version: RefCell<Option<ArtifactVersion>>,
    bytes: RefCell<Vec<u8>>,
    fail_at: Cell<u8>,
    registrations: RefCell<Vec<RegisterVersionRequest>>,
    reads: Cell<usize>,
    scope_valid: Cell<bool>,
}
impl Fake {
    fn new() -> Self {
        Self {
            family: RefCell::new(None),
            version: RefCell::new(None),
            bytes: RefCell::new(vec![]),
            fail_at: Cell::new(0),
            registrations: RefCell::new(vec![]),
            reads: Cell::new(0),
            scope_valid: Cell::new(true),
        }
    }
    fn fail(&self, stage: u8) -> Result<()> {
        if self.fail_at.get() == stage {
            self.fail_at.set(0);
            Err(Failure::new("uncertain", "lost response", true))
        } else {
            Ok(())
        }
    }
}
impl Port for Fake {
    fn create(&self, r: &CreateFamilyRequest) -> Result<()> {
        if self.family.borrow().is_some() {
            return Err(Failure::new("artifact_family_exists", "exists", false));
        }
        *self.family.borrow_mut() = Some(ArtifactFamily {
            id: r.family_id,
            workspace: r.workspace,
            title: r.title.clone(),
            created_by_run: r.created_by_run,
            created_at_ms: 0,
            retired_at_ms: None,
        });
        self.fail(1)
    }
    fn family(&self, _: ArtifactFamilyId) -> Result<FamilyResponse> {
        Ok(FamilyResponse {
            family: self.family.borrow().as_ref().unwrap().clone(),
            versions: self.version.borrow().iter().cloned().collect(),
        })
    }
    fn upload(&self, op: &SaveOperation) -> Result<BlobHash> {
        *self.bytes.borrow_mut() = op.bytes.clone();
        self.fail(2)?;
        Ok(op.registration.blob.clone())
    }
    fn register(&self, op: &SaveOperation) -> Result<ArtifactVersion> {
        self.registrations
            .borrow_mut()
            .push(op.registration.clone());
        let mut stored = self.version.borrow_mut();
        let version = stored
            .get_or_insert_with(|| ArtifactVersion {
                id: ArtifactVersionId::mint(),
                family: op.family,
                ordinal: Ordinal::FIRST,
                blob: op.registration.blob.clone(),
                size_bytes: op.bytes.len() as u64,
                mime: op.registration.mime.clone(),
                filename: op.registration.filename.clone(),
                produced_by: op.registration.produced_by,
                registration_id: op.registration.registration_id,
                derived_from: op.registration.derived_from,
                created_at_ms: 0,
            })
            .clone();
        drop(stored);
        self.fail(3)?;
        Ok(version)
    }
    fn version(&self, _: ArtifactVersionId) -> Result<ArtifactVersion> {
        Ok(self.version.borrow().as_ref().unwrap().clone())
    }
    fn content(&self, _: ArtifactVersionId) -> Result<Vec<u8>> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.bytes.borrow().clone())
    }
    fn scope(
        &self,
        _: koru_core::ProfileId,
        _: koru_core::WorkspaceId,
        _: &Envelope,
    ) -> Result<()> {
        if self.scope_valid.get() {
            Ok(())
        } else {
            Err(invalid("missing reference"))
        }
    }
}

#[test]
fn every_uncertain_boundary_retries_the_exact_frozen_operation() {
    for stage in 1..=3 {
        let fake = Fake::new();
        fake.fail_at.set(stage);
        let operation = SaveOperation::capture(snapshot(), 42, "My mini app", None).unwrap();
        let mut state = SaveState {
            operation: Some(operation.clone()),
            in_flight: true,
            ..Default::default()
        };
        state.finish(save(&fake, &operation));
        assert!(state.failed);
        assert!(state.saved.is_none());
        let retry = state.operation.as_ref().unwrap();
        assert_eq!(retry.captured_revision, 42);
        assert_eq!(retry.bytes, operation.bytes);
        assert_eq!(retry.registration, operation.registration);
        assert_eq!(retry.create, operation.create);
        let saved = save(&fake, retry).unwrap();
        state.finish(Ok(saved.clone()));
        assert_eq!(state.saved.unwrap().version, saved.id);
        assert!(state.operation.is_none());
        assert!(
            fake.registrations
                .borrow()
                .iter()
                .all(|r| r == &operation.registration)
        );
        assert_eq!(
            fake.version.borrow().as_ref().unwrap().ordinal,
            Ordinal::FIRST
        );
    }
}
#[test]
fn authoritative_metadata_precedes_mime_download_and_scope_is_checked() {
    let fake = Fake::new();
    let op = SaveOperation::capture(snapshot(), 0, "App", None).unwrap();
    let version = save(&fake, &op).unwrap();
    let profile = koru_core::ProfileId::new();
    let workspace = koru_core::WorkspaceId::from_uuid(op.snapshot.workspace().as_uuid());
    assert!(open(&fake, profile, koru_core::WorkspaceId::new(), version.id).is_err());
    assert_eq!(fake.reads.get(), 0);
    fake.version.borrow_mut().as_mut().unwrap().mime = MediaType::new("text/markdown").unwrap();
    assert!(matches!(
        open(&fake, profile, workspace, version.id).unwrap(),
        Opened::Other { .. }
    ));
    assert_eq!(fake.reads.get(), 0);
    fake.version.borrow_mut().as_mut().unwrap().mime = MediaType::new(MIME).unwrap();
    fake.scope_valid.set(false);
    assert!(open(&fake, profile, workspace, version.id).is_err());
    fake.scope_valid.set(true);
    assert!(matches!(
        open(&fake, profile, workspace, version.id).unwrap(),
        Opened::MiniApp { .. }
    ));
    fake.bytes.borrow_mut()[0] = b' ';
    assert!(open(&fake, profile, workspace, version.id).is_err());
}
#[test]
fn save_capture_refuses_pending_binding_and_old_capture_success_does_not_save_new_changes() {
    let mut app = MiniApp::new(
        koru_core::WorkspaceId::new(),
        7,
        RailTarget::Bucket(crate::panes::explore::rail::Bucket::Today),
    );
    // Advance the live declaration before the click so this cannot pass by
    // accidentally always capturing the initial revision.
    app.select_tasks(RailTarget::Bucket(
        crate::panes::explore::rail::Bucket::Later,
    ));
    let captured = app.artifact_snapshot().unwrap();
    let revision = app.declaration_revision();
    app.request_artifact_save();
    let operation = app.artifact.operation.as_ref().unwrap().clone();
    assert_eq!(operation.captured_revision, revision);
    assert_eq!(operation.snapshot, captured);
    assert!(
        !String::from_utf8(operation.bytes.clone())
            .unwrap()
            .contains("revision")
    );
    app.artifact.in_flight = true;
    app.select_tasks(RailTarget::Bucket(
        crate::panes::explore::rail::Bucket::Completed,
    ));
    assert!(app.declaration_revision() > operation.captured_revision);
    let fake = Fake::new();
    app.artifact.finish(save(&fake, &operation));
    assert_ne!(app.artifact_snapshot().unwrap(), captured);
    assert_eq!(
        app.save_label(),
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::MiniAppSave)
    );
    app.select_tasks(RailTarget::Bucket(
        crate::panes::explore::rail::Bucket::Later,
    ));
    assert!(app.declaration_revision() > revision);
    assert!(
        app.saved_arrangement_is_current(),
        "a newer revision with the exact saved definition is saved"
    );
    app.request_document(koru_core::DocumentId::new(), None);
    assert!(app.artifact_snapshot().is_err());
}
