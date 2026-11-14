// SPDX-License-Identifier: MPL-2.0

//! Frozen saves and bounded background I/O. A failed call retains its entire
//! operation: a later click retries the same bytes and registration identity.
use super::*;
use koru_ai_core::mini_apps::artifact::{
    MAX_FILE_BYTES as MAX_BYTES, MIME, SavedEnvelope as Envelope,
};
use koru_artifact_wire::{
    ArtifactFamilyId, ArtifactTitle, ArtifactVersion, ArtifactVersionId, BlobHash,
    CreateFamilyRequest, Failure, FamilyResponse, MediaType, RegisterVersionRequest,
    RegistrationId,
};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, SyncSender},
};

pub type Waker = Arc<dyn Fn() + Send + Sync>;
pub type Result<T> = std::result::Result<T, Failure>;

#[derive(Clone)]
pub struct SaveOperation {
    /// Live declaration evidence only; never part of the artifact bytes.
    pub captured_revision: u64,
    pub snapshot: Envelope,
    pub bytes: Vec<u8>,
    pub family: ArtifactFamilyId,
    pub create: Option<CreateFamilyRequest>,
    pub registration: RegisterVersionRequest,
}
impl SaveOperation {
    pub fn capture(
        snapshot: Envelope,
        captured_revision: u64,
        title: &str,
        prior: Option<&Saved>,
    ) -> Result<Self> {
        let bytes = snapshot.encode().map_err(invalid)?;
        let family = prior.map_or_else(ArtifactFamilyId::mint, |p| p.family);
        let create = if prior.is_none() {
            Some(CreateFamilyRequest {
                family_id: family,
                workspace: snapshot.workspace(),
                title: ArtifactTitle::new(title).map_err(|_| invalid("invalid artifact title"))?,
                created_by_run: None,
            })
        } else {
            None
        };
        let blob = BlobHash::from_digest("b3", blake3::hash(&bytes).to_hex().as_str())
            .map_err(|_| invalid("invalid blob"))?;
        Ok(Self {
            captured_revision,
            snapshot,
            bytes,
            family,
            create,
            registration: RegisterVersionRequest {
                registration_id: RegistrationId::mint(),
                blob,
                mime: MediaType::new(MIME).map_err(|_| invalid("invalid media type"))?,
                filename: None,
                produced_by: None,
                derived_from: prior.map(|p| p.version),
            },
        })
    }
}

#[derive(Clone)]
pub struct Saved {
    pub family: ArtifactFamilyId,
    pub version: ArtifactVersionId,
    pub snapshot: Envelope,
}
#[derive(Default)]
pub struct SaveState {
    pub owner: Option<koru_core::ProfileId>,
    pub title: String,
    pub notice: Option<koru_i18n::Key>,
    pub saved: Option<Saved>,
    pub operation: Option<SaveOperation>,
    pub requested: bool,
    pub in_flight: bool,
    pub failed: bool,
}
impl SaveState {
    pub fn request(&mut self) {
        if !self.in_flight {
            self.requested = true;
        }
    }
    pub fn finish(&mut self, result: Result<ArtifactVersion>) {
        self.in_flight = false;
        match result {
            Ok(version) => {
                if let Some(op) = self.operation.take() {
                    self.saved = Some(Saved {
                        family: op.family,
                        version: version.id,
                        snapshot: op.snapshot,
                    });
                }
                self.failed = false;
            }
            Err(_) => self.failed = true,
        }
    }
}

impl MiniApp {
    pub fn request_artifact_save(&mut self) {
        if self.artifact.in_flight {
            return;
        }
        if self.artifact.operation.is_some() {
            self.artifact.request();
            return;
        }
        if self.saved_arrangement_is_current() {
            return;
        }
        let title = if self.artifact.title.is_empty() {
            self.default_title()
        } else {
            &self.artifact.title
        };
        match self.artifact_snapshot().and_then(|snapshot| {
            SaveOperation::capture(
                snapshot,
                self.declaration_revision(),
                title,
                self.artifact.saved.as_ref(),
            )
        }) {
            Ok(operation) => {
                self.artifact.operation = Some(operation);
                self.artifact.failed = false;
                self.artifact.request();
            }
            Err(_) => self.artifact.notice = Some(koru_i18n::Key::MiniAppSavePending),
        }
    }
    pub fn artifact_snapshot(&self) -> Result<Envelope> {
        if self.pending.is_some() {
            return Err(invalid("document binding is unsettled"));
        }
        let target = self.refinement_target();
        if let Some((cached, snapshot)) = self.snapshot_cache.borrow().as_ref()
            && *cached == target
        {
            return Ok(snapshot.clone());
        }
        let snapshot = Envelope::capture(
            koru_ai_core::WorkspaceId::from_uuid(self.workspace.as_uuid()),
            &self.effective_definition(),
        )
        .map_err(invalid)?;
        *self.snapshot_cache.borrow_mut() = Some((target, snapshot.clone()));
        Ok(snapshot)
    }
    pub fn save_label(&self) -> &'static str {
        use koru_i18n::Key::*;
        let key = if self.artifact.in_flight {
            MiniAppSaving
        } else if self.artifact.failed {
            MiniAppSaveRetry
        } else if self.saved_arrangement_is_current() {
            MiniAppSaved
        } else {
            MiniAppSave
        };
        koru_i18n::t(crate::format::lang(), key)
    }
    pub fn saved_arrangement_is_current(&self) -> bool {
        let Some(saved) = &self.artifact.saved else {
            return false;
        };
        self.artifact_snapshot()
            .is_ok_and(|current| current == saved.snapshot)
    }
}

pub enum Command {
    Save {
        tab: u64,
        profile: koru_core::ProfileId,
        operation: SaveOperation,
    },
    Open {
        epoch: u64,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        version: ArtifactVersionId,
    },
}
pub enum Opened {
    MiniApp {
        version: ArtifactVersion,
        family: FamilyResponse,
        envelope: Envelope,
    },
    Other {
        version: ArtifactVersion,
        family: FamilyResponse,
    },
}
impl Opened {
    pub fn version(&self) -> ArtifactVersionId {
        match self {
            Self::MiniApp { version, .. } | Self::Other { version, .. } => version.id,
        }
    }
}
pub enum Answer {
    Saved {
        tab: u64,
        profile: koru_core::ProfileId,
        workspace: koru_ai_core::WorkspaceId,
        registration: RegistrationId,
        captured_revision: u64,
        result: Result<ArtifactVersion>,
    },
    Opened {
        epoch: u64,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        version: ArtifactVersionId,
        result: Result<Opened>,
    },
}

/// One bounded queue per window; no worker or gateway exists until an action.
#[derive(Default)]
pub struct Io {
    sender: Option<SyncSender<Command>>,
    answers: Option<Receiver<Answer>>,
    wake: Option<Waker>,
    epoch: u64,
}
impl Io {
    #[cfg(test)]
    pub fn in_memory() -> (Self, Receiver<Command>, SyncSender<Answer>) {
        let (sender, commands) = mpsc::sync_channel(16);
        let (answers, receiver) = mpsc::sync_channel(16);
        (
            Self {
                sender: Some(sender),
                answers: Some(receiver),
                ..Self::default()
            },
            commands,
            answers,
        )
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn invalidate_scope(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }
    pub fn set_waker(&mut self, wake: Waker) {
        self.wake = Some(wake);
    }
    pub fn send(&mut self, command: Command) -> Result<()> {
        if self.sender.is_none() {
            if cfg!(test) {
                return Err(invalid("artifact I/O is disabled in tests"));
            }
            let (send, receive) = mpsc::sync_channel(16);
            let (answers, results) = mpsc::sync_channel(16);
            let root = koru_workspace_host::cli::root_from_args();
            let wake = self.wake.clone();
            std::thread::Builder::new()
                .name("koru-mini-app-artifacts".into())
                .spawn(move || {
                    let mut delivery_alive = true;
                    for command in receive {
                        if !delivery_alive && matches!(command, Command::Open { .. }) {
                            continue;
                        }
                        let client = match &root {
                            Some(root) => {
                                koru_api_client::Client::open_at(koru_channel::segment(), root)
                            }
                            None => koru_api_client::Client::open(koru_channel::segment()),
                        }
                        .map_err(|_| {
                            Failure::new(
                                "gateway_unavailable",
                                "The artifact gateway is unavailable.",
                                true,
                            )
                        });
                        let answer = match command {
                            Command::Save {
                                tab,
                                profile,
                                operation,
                            } => {
                                let workspace = operation.snapshot.workspace();
                                let registration = operation.registration.registration_id;
                                let captured_revision = operation.captured_revision;
                                let result = client.and_then(|client| {
                                    let port = ClientPort(&client);
                                    port.scope(
                                        profile,
                                        koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
                                        &operation.snapshot,
                                    )?;
                                    save(&port, &operation)
                                });
                                Answer::Saved {
                                    tab,
                                    profile,
                                    workspace,
                                    registration,
                                    captured_revision,
                                    result,
                                }
                            }
                            Command::Open {
                                epoch,
                                profile,
                                workspace,
                                version,
                            } => {
                                let result = client.and_then(|client| {
                                    open(&ClientPort(&client), profile, workspace, version)
                                });
                                Answer::Opened {
                                    epoch,
                                    profile,
                                    workspace,
                                    version,
                                    result,
                                }
                            }
                        };
                        // Dropping the view never joins a blocked worker. If its
                        // result mailbox is full, pending UI work must drain first.
                        // Even when the owner has closed, finish accepted queued
                        // saves. Dropping the UI receiver cannot erase an action
                        // that already reached this bounded worker.
                        delivery_alive &= answers.send(answer).is_ok();
                        if let Some(wake) = &wake {
                            wake();
                        }
                    }
                })
                .map_err(|_| {
                    Failure::new(
                        "worker_unavailable",
                        "The artifact worker could not start.",
                        true,
                    )
                })?;
            self.sender = Some(send);
            self.answers = Some(results);
        }
        self.sender
            .as_ref()
            .unwrap()
            .try_send(command)
            .map_err(|_| {
                Failure::new(
                    "worker_busy",
                    "The artifact worker is busy. Try again.",
                    true,
                )
            })
    }
    pub fn drain(&self) -> Vec<Answer> {
        self.answers
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }
}

pub trait Port {
    fn create(&self, request: &CreateFamilyRequest) -> Result<()>;
    fn family(&self, family: ArtifactFamilyId) -> Result<FamilyResponse>;
    fn upload(&self, operation: &SaveOperation) -> Result<BlobHash>;
    fn register(&self, operation: &SaveOperation) -> Result<ArtifactVersion>;
    fn version(&self, version: ArtifactVersionId) -> Result<ArtifactVersion>;
    fn content(&self, version: ArtifactVersionId) -> Result<Vec<u8>>;
    fn scope(
        &self,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        envelope: &Envelope,
    ) -> Result<()>;
}
pub fn save(port: &impl Port, operation: &SaveOperation) -> Result<ArtifactVersion> {
    if operation.bytes != operation.snapshot.encode().map_err(invalid)? {
        return Err(invalid("save snapshot changed"));
    }
    if let Some(request) = &operation.create {
        match port.create(request) {
            Ok(()) => {}
            Err(f) if f.code == "artifact_family_exists" => {}
            Err(f) => return Err(f),
        }
    }
    let family = port.family(operation.family)?.family;
    if family.id != operation.family
        || family.workspace != operation.snapshot.workspace()
        || family.is_retired()
        || operation.create.as_ref().is_some_and(|c| {
            c.family_id != family.id
                || c.title != family.title
                || c.created_by_run != family.created_by_run
        })
    {
        return Err(invalid("artifact family does not match save"));
    }
    if port.upload(operation)? != operation.registration.blob {
        return Err(invalid("uploaded bytes do not match save"));
    }
    let version = port.register(operation)?;
    if version.family != operation.family
        || version.registration_id != operation.registration.registration_id
        || version.blob != operation.registration.blob
        || version.mime != operation.registration.mime
        || version.filename != operation.registration.filename
        || version.produced_by != operation.registration.produced_by
        || version.derived_from != operation.registration.derived_from
        || version.size_bytes != operation.bytes.len() as u64
    {
        return Err(invalid("registered version does not match save"));
    }
    Ok(version)
}
pub fn open(
    port: &impl Port,
    profile: koru_core::ProfileId,
    workspace: koru_core::WorkspaceId,
    id: ArtifactVersionId,
) -> Result<Opened> {
    let version = port.version(id)?;
    if version.id != id {
        return Err(invalid("artifact identity mismatch"));
    }
    let family = port.family(version.family)?;
    if family.family.id != version.family
        || family.family.workspace.as_uuid() != workspace.as_uuid()
    {
        return Err(invalid("artifact workspace mismatch"));
    }
    if version.mime.as_str() != MIME {
        return Ok(Opened::Other { version, family });
    }
    if version.size_bytes > MAX_BYTES as u64 {
        return Err(invalid("mini app exceeds byte limit"));
    }
    let bytes = port.content(id)?;
    if bytes.len() != version.size_bytes as usize {
        return Err(invalid("artifact length mismatch"));
    }
    let hash = BlobHash::from_digest("b3", blake3::hash(&bytes).to_hex().as_str())
        .map_err(|_| invalid("invalid artifact hash"))?;
    if version.blob != hash {
        return Err(invalid("artifact bytes do not match immutable version"));
    }
    let envelope = Envelope::decode(&bytes).map_err(invalid)?;
    if envelope.workspace().as_uuid() != workspace.as_uuid() {
        return Err(invalid("mini app workspace mismatch"));
    }
    port.scope(profile, workspace, &envelope)?;
    Ok(Opened::MiniApp {
        version,
        family,
        envelope,
    })
}
struct ClientPort<'a>(&'a koru_api_client::Client);
impl Port for ClientPort<'_> {
    fn create(&self, request: &CreateFamilyRequest) -> Result<()> {
        self.0.artifacts().create_family(request).map(|_| ())
    }
    fn family(&self, family: ArtifactFamilyId) -> Result<FamilyResponse> {
        self.0.artifacts().family(family)
    }
    fn upload(&self, op: &SaveOperation) -> Result<BlobHash> {
        self.0
            .store()
            .put_blob(op.snapshot.workspace(), op.bytes.clone(), MIME, None)
            .map(|b| b.hash)
            .map_err(store_failure)
    }
    fn register(&self, op: &SaveOperation) -> Result<ArtifactVersion> {
        self.0
            .artifacts()
            .register_version(op.family, &op.registration)
            .map(|r| r.version)
    }
    fn version(&self, version: ArtifactVersionId) -> Result<ArtifactVersion> {
        self.0.artifacts().version(version)
    }
    fn content(&self, version: ArtifactVersionId) -> Result<Vec<u8>> {
        self.0.artifacts().content_bounded(version, MAX_BYTES)
    }
    fn scope(
        &self,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        envelope: &Envelope,
    ) -> Result<()> {
        let scope = self
            .0
            .store()
            .workspaces()
            .map_err(store_failure)?
            .into_iter()
            .find(|w| {
                w.id.as_uuid() == workspace.as_uuid()
                    && w.owner.as_uuid() == profile.as_uuid()
                    && w.deleted_at.is_none()
            })
            .ok_or_else(|| invalid("workspace is unavailable"))?;
        let documents = envelope.document_references();
        if !documents.is_empty() {
            let entries = self.0.store().documents(scope.id).map_err(store_failure)?;
            if !documents.iter().all(|document| {
                entries.iter().any(|e| {
                    e.id == *document
                        && e.kind == koru_core::explore::KIND_DOCUMENT
                        && e.trashed_at.is_none()
                })
            }) {
                return Err(invalid("document binding is unavailable"));
            }
        }
        // List membership is rechecked against the host's live SystemDoc table
        // before tab allocation. No source contents travel through the codec.
        Ok(())
    }
}
pub fn invalid(message: &str) -> Failure {
    Failure::new("invalid_mini_app", message, false)
}
fn store_failure(error: koru_store_core::errors::Error) -> Failure {
    Failure::new("store_unavailable", error.to_string(), true)
}

#[cfg(test)]
mod tests;
