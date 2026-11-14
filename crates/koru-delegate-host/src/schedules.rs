// SPDX-License-Identifier: MPL-2.0

//! `koru-schedule` documents through the gateway.
//!
//! The durable link is the same private mechanism as Run documents. The
//! change watch joins only each active workspace manifest and each known
//! schedule document's Store sync room. Updates set a sticky content-free
//! scheduler wake; the dedicated evaluator then re-reads catalog and authority.

use std::{
    collections::BTreeSet,
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use koru_api_client::Client;
use koru_delegate_core::{DocumentId, WorkspaceId};
use koru_delegate_document::{
    SCHEDULE_KIND, ScheduleClaimAuthority, ScheduleClaimPlan, ScheduleDocument,
    ScheduleDocumentError,
};
use koru_delegate_runtime::{
    CaptureFailure, ListedSchedule, Port, PortError, PortResult, ScheduleCatalog,
    ScheduleChangeWatch, ScheduleDocumentHandle, ScheduleDocuments, SchedulePreparation,
    ScheduleWake, ScheduleWatchTargets, Sessions,
};
use koru_delegate_schedule::{ScheduleHeader, ScheduleName};
use koru_delegate_scope::ResolvedScope;
use koru_store_client::sync::{Connection, Event};
use loro::{LoroDoc, VersionVector};

use crate::documents::{Link, SyncedDocument};

const WATCH_JOIN_DEADLINE: Duration = Duration::from_secs(10);
const WATCH_JOIN_READ_SLICE: Duration = Duration::from_millis(200);
const WATCH_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);

impl SyncedDocument for ScheduleDocument {
    fn version(&self) -> VersionVector {
        ScheduleDocument::version(self)
    }

    fn import(&self, bytes: &[u8]) -> Result<(), impl std::fmt::Display> {
        ScheduleDocument::import(self, bytes)
    }

    fn export_updates_since(
        &self,
        since: &VersionVector,
    ) -> Result<Vec<u8>, impl std::fmt::Display> {
        ScheduleDocument::export_updates_since(self, since)
    }
}

pub struct StoreSchedules {
    client: Client,
}

pub struct HostSchedulePreparation {
    sessions: crate::sessions::HostSessions,
    roots: crate::roots::HostRoots,
    capture: crate::context::ScheduleCapture,
}

impl HostSchedulePreparation {
    pub fn new(
        sessions: crate::sessions::HostSessions,
        roots: crate::roots::HostRoots,
        capture: crate::context::ScheduleCapture,
    ) -> Self {
        Self {
            sessions,
            roots,
            capture,
        }
    }
}

impl SchedulePreparation for HostSchedulePreparation {
    fn profile_delegatable(
        &mut self,
        profile: &koru_session_protocol::SessionProfileKey,
    ) -> PortResult<bool> {
        self.sessions.profile_delegatable(profile)
    }

    fn resolve_scope(
        &mut self,
        run: koru_delegate_core::RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<ResolvedScope, koru_delegate_scope::ScopeRefusal> {
        match scope {
            koru_delegate_core::WorkspaceScope::Temporary => self.roots.planned_temporary(run),
            _ => ResolvedScope::resolve(scope, run, &self.roots),
        }
    }

    fn resolve_and_pin(
        &mut self,
        run: koru_delegate_core::RunId,
        pointers: &[koru_delegate_schedule::ScheduleContextPointer],
        workspace: WorkspaceId,
    ) -> Result<Vec<koru_delegate_core::RunContextItem>, CaptureFailure> {
        self.capture.resolve_and_pin(run, pointers, workspace)
    }

    fn discard_candidate(
        &mut self,
        run: koru_delegate_core::RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> PortResult<()> {
        if !scope.is_koru_owned() {
            return Ok(());
        }
        let _ = run;
        Ok(())
    }
}

impl StoreSchedules {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

fn schedule_error(operation: &'static str, error: impl std::fmt::Display) -> PortError {
    PortError::new(Port::Schedules, operation, error.to_string())
}

fn schedule_seed_error(error: koru_store_core::Error) -> PortError {
    if error.code() == koru_store_core::Code::IdempotencyConflict {
        return PortError::new(
            Port::Schedules,
            "schedule_conflict",
            "the schedule identity is already owned elsewhere",
        );
    }
    schedule_error("schedule_seed", error)
}

fn authored_request_hash(name: &ScheduleName, header: &ScheduleHeader) -> PortResult<String> {
    // Hash the canonical typed value, not Loro update bytes: a fresh Loro
    // document may encode a different actor identity on an exact retry.
    let authored = serde_json::to_vec(&(
        header.schedule,
        name,
        &header.definition.template,
        &header.definition.recurrence,
    ))
    .map_err(|error| schedule_error("schedule_encode", error))?;
    Ok(blake3::hash(&authored).to_hex().to_string())
}

fn schedule_document_id(schedule: koru_delegate_core::ScheduleId) -> DocumentId {
    DocumentId::from_uuid(schedule.as_uuid())
}

fn schedule_link_error(error: PortError) -> PortError {
    PortError::new(Port::Schedules, error.code, error.detail)
}

struct StoreScheduleHandle {
    client: Client,
    workspace: WorkspaceId,
    document: DocumentId,
    doc: ScheduleDocument,
    link: Link,
}

impl ScheduleDocumentHandle for StoreScheduleHandle {
    fn document(&self) -> &ScheduleDocument {
        &self.doc
    }

    fn expired_routine_authority(
        &self,
        device: koru_delegate_core::DeviceId,
        plan: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
        expired_routine_authority_at(self.document, self.workspace, &self.doc, device, plan)
    }

    fn claim_authority(
        &self,
        device: koru_delegate_core::DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<ScheduleClaimAuthority>> {
        routine_claim_location(
            self.document,
            match &plan {
                ScheduleClaimPlan::BotRoutine { prepared, .. } => Some(prepared.slot.schedule),
                _ => None,
            },
        )?;
        self.doc
            .claim_authority(self.workspace, device, plan)
            .map_err(Into::into)
    }

    fn absorb(&mut self) -> PortResult<bool> {
        self.link.absorb(&self.doc).map_err(schedule_link_error)
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(&ScheduleDocument) -> Result<(), ScheduleDocumentError>,
    ) -> PortResult<()> {
        write(&self.doc)?;
        match self.link.commit(&self.doc) {
            Ok(()) => Ok(()),
            Err(_) => {
                let acked = self.link.acked.clone();
                self.link = Link::join(&self.client, self.document, &self.doc, acked)
                    .map_err(schedule_link_error)?;
                self.link.commit(&self.doc).map_err(schedule_link_error)
            }
        }
    }
}

/// Shared production binding, separated from the network-owning handle so its
/// exact physical-location guard can be tested without opening a Store.
fn expired_routine_authority_at(
    physical: DocumentId,
    workspace: WorkspaceId,
    doc: &ScheduleDocument,
    device: koru_delegate_core::DeviceId,
    plan: koru_delegate_document::RoutineExpiryPlan,
) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
    routine_claim_location(physical, Some(doc.header()?.schedule))?;
    doc.expired_routine_authority(workspace, device, plan)
        .map_err(Into::into)
}

fn routine_claim_location(
    document: DocumentId,
    schedule: Option<koru_delegate_core::ScheduleId>,
) -> Result<(), ScheduleDocumentError> {
    if schedule.is_some_and(|schedule| schedule.as_uuid() != document.as_uuid()) {
        return Err(ScheduleDocumentError::ClaimBindingMismatch);
    }
    Ok(())
}

struct Watch {
    control: Arc<WatchControl>,
    thread: Option<JoinHandle<()>>,
}

struct WatchControl {
    stop: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
    retry: Condvar,
}

impl WatchControl {
    fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            socket: Mutex::new(None),
            retry: Condvar::new(),
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn install(&self, socket: TcpStream) -> bool {
        let mut current = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.stopped() {
            let _ = socket.shutdown(Shutdown::Both);
            false
        } else {
            *current = Some(socket);
            true
        }
    }

    fn clear(&self) {
        self.socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn wait_to_retry(&self) {
        let current = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = self
            .retry
            .wait_timeout_while(current, WATCH_RECONNECT_BACKOFF, |_| !self.stopped());
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(stream) = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.retry.notify_all();
    }
}

impl Watch {
    fn stop(&mut self) {
        self.control.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ScheduleChangeWatch for Watch {
    fn close(mut self: Box<Self>) {
        self.stop();
    }
}

fn document_room(document: DocumentId) -> String {
    format!("document:{}", document.to_storage())
}

fn manifest_room(workspace: WorkspaceId) -> String {
    format!("workspace:{}:manifest", workspace.to_storage())
}

fn snapshot_version(
    client: &Client,
    workspace: WorkspaceId,
    document: DocumentId,
) -> PortResult<VersionVector> {
    let snapshot = client
        .store()
        .snapshot(workspace, document)
        .map_err(|error| schedule_error("watch_snapshot", error))?;
    let doc = LoroDoc::new();
    doc.import(&snapshot)
        .map_err(|error| schedule_error("watch_snapshot_import", error))?;
    Ok(doc.oplog_vv())
}

fn watch_rooms(
    client: &Client,
    targets: &ScheduleWatchTargets,
) -> PortResult<Vec<(String, VersionVector)>> {
    let mut rooms = Vec::with_capacity(targets.workspaces.len() + targets.documents.len());
    for workspace in &targets.workspaces {
        let manifest = client
            .store()
            .manifest(*workspace)
            .map_err(|error| schedule_error("watch_manifest", error))?;
        rooms.push((
            manifest_room(*workspace),
            snapshot_version(client, *workspace, manifest)?,
        ));
    }
    for (workspace, document) in &targets.documents {
        rooms.push((
            document_room(*document),
            snapshot_version(client, *workspace, *document)?,
        ));
    }
    rooms.sort_by(|left, right| left.0.cmp(&right.0));
    rooms.dedup_by(|left, right| left.0 == right.0);
    Ok(rooms)
}

fn discover_catalog(client: &Client) -> PortResult<ScheduleCatalog> {
    // The public/evaluation catalog excludes deleted workspaces. Keep archived
    // workspaces and trashed schedule rows: both are presentation state, not
    // execution authority. A trashed-but-unretired row is the recoverable
    // half of retire-before-trash and must remain visible to the evaluator.
    let mut workspaces = client
        .store()
        .workspaces()
        .map_err(|error| schedule_error("schedule_workspaces", error))?
        .into_iter()
        .filter(|workspace| workspace.deleted_at.is_none())
        .map(|workspace| workspace.id)
        .collect::<Vec<_>>();
    workspaces.sort_unstable();
    workspaces.dedup();

    let mut schedules = Vec::new();
    let mut failed_workspaces = Vec::new();
    let mut unavailable_documents = Vec::new();
    let mut failures = 0_u32;
    for workspace in &workspaces {
        let entries = match client.store().documents(*workspace) {
            Ok(entries) => entries,
            Err(_) => {
                failed_workspaces.push(*workspace);
                failures = failures.saturating_add(1);
                continue;
            }
        };
        for entry in entries
            .into_iter()
            .filter(|entry| entry.kind == SCHEDULE_KIND)
        {
            let snapshot = match client.store().snapshot(*workspace, entry.id) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    unavailable_documents.push((*workspace, entry.id));
                    failures = failures.saturating_add(1);
                    continue;
                }
            };
            let document = match ScheduleDocument::from_snapshot(&snapshot) {
                Ok(document) => document,
                Err(_) => {
                    unavailable_documents.push((*workspace, entry.id));
                    failures = failures.saturating_add(1);
                    continue;
                }
            };
            let header = match document.header() {
                Ok(header) => header,
                Err(_) => {
                    unavailable_documents.push((*workspace, entry.id));
                    failures = failures.saturating_add(1);
                    continue;
                }
            };
            schedules.push(ListedSchedule {
                workspace: *workspace,
                document: entry.id,
                header,
                trashed_at_ms: entry.trashed_at.map(|at| at.millis()),
            });
        }
    }
    schedules.sort_by_key(|listed| (listed.header.created_at_ms, listed.header.schedule));
    unavailable_documents.sort_unstable();
    unavailable_documents.dedup();
    failed_workspaces.sort_unstable();
    failed_workspaces.dedup();
    Ok(ScheduleCatalog {
        workspaces,
        schedules,
        failed_workspaces,
        unavailable_documents,
        failures,
    })
}

fn connect_exact_rooms(
    client: &Client,
    rooms: &[(String, VersionVector)],
    control: &WatchControl,
    wake: &ScheduleWake,
) -> PortResult<Connection> {
    let answer = (|| {
        let mut connection = Connection::open(&client.store().discovery())
            .map_err(|error| schedule_error("watch_sync_open", error))?;
        connection
            .set_read_timeout(Some(WATCH_JOIN_READ_SLICE))
            .map_err(|error| schedule_error("watch_sync_timeout", error))?;
        let abort = connection.abort_handle().ok_or_else(|| {
            schedule_error(
                "watch_sync_abort",
                "the Store sync stream cannot be interrupted",
            )
        })?;
        if !control.install(abort) {
            return Err(schedule_error("watch_sync_stopped", "watch was closed"));
        }
        for (room, version) in rooms {
            connection
                .join(room, version)
                .map_err(|error| schedule_error("watch_sync_join", error))?;
        }

        let expected = rooms
            .iter()
            .map(|(room, _)| room.clone())
            .collect::<BTreeSet<_>>();
        let mut joined = BTreeSet::new();
        let deadline = Instant::now() + WATCH_JOIN_DEADLINE;
        while joined != expected {
            match connection.next_event() {
                Ok(Some(Event::Joined { room, .. })) if expected.contains(&room) => {
                    joined.insert(room);
                }
                Ok(Some(Event::Updates { room, .. })) if expected.contains(&room) => wake.signal(),
                Ok(Some(Event::JoinRefused { room, code, .. })) if expected.contains(&room) => {
                    return Err(schedule_error(
                        "watch_sync_join_refused",
                        format!("{room}: {code:?}"),
                    ));
                }
                Ok(Some(Event::RoomProblem { room, code, .. })) if expected.contains(&room) => {
                    return Err(schedule_error(
                        "watch_sync_room",
                        format!("{room}: {code:?}"),
                    ));
                }
                Ok(_) => {}
                Err(error) => return Err(schedule_error("watch_sync_read", error)),
            }
            if control.stopped() {
                return Err(schedule_error("watch_sync_stopped", "watch was closed"));
            }
            if Instant::now() >= deadline {
                return Err(schedule_error(
                    "watch_sync_join",
                    "the Store did not answer every exact room join",
                ));
            }
        }
        // Healthy idle is one interruptible blocking read. Only registration
        // uses a timeout to enforce its deadline.
        connection
            .set_read_timeout(None)
            .map_err(|error| schedule_error("watch_sync_blocking", error))?;
        Ok(connection)
    })();
    if answer.is_err() {
        control.clear();
    }
    answer
}

fn current_rooms(client: &Client) -> PortResult<Vec<(String, VersionVector)>> {
    let catalog = discover_catalog(client)?;
    watch_rooms(client, &ScheduleWatchTargets::from_catalog(&catalog))
}

fn drive_watch(
    client: Client,
    mut connection: Connection,
    control: Arc<WatchControl>,
    wake: ScheduleWake,
) {
    loop {
        let ended = match connection.next_event() {
            Ok(Some(Event::Updates { .. })) => {
                wake.signal();
                false
            }
            Ok(Some(Event::RoomProblem { .. } | Event::JoinRefused { .. })) | Err(_) => true,
            Ok(Some(Event::Joined { .. } | Event::Acked { .. })) => false,
            Ok(None) => true,
        };
        if !ended {
            continue;
        }

        control.clear();
        if control.stopped() {
            return;
        }
        // Terminal watch failure is content-free but never silent. The
        // scheduler re-reads authority while this registration repairs itself.
        wake.signal();
        loop {
            if control.stopped() {
                return;
            }
            let reconnected = current_rooms(&client).and_then(|rooms| {
                if rooms.is_empty() {
                    Err(schedule_error("watch_sync_rooms", "no schedule rooms"))
                } else {
                    connect_exact_rooms(&client, &rooms, &control, &wake)
                }
            });
            match reconnected {
                Ok(joined) => {
                    connection = joined;
                    wake.signal();
                    break;
                }
                Err(_) if control.stopped() => return,
                Err(_) => control.wait_to_retry(),
            }
        }
    }
}

impl ScheduleDocuments for StoreSchedules {
    fn create(
        &mut self,
        workspace: WorkspaceId,
        header: &ScheduleHeader,
    ) -> PortResult<DocumentId> {
        let name = ScheduleName::from_objective(&header.definition.template.objective);
        self.create_named(workspace, &name, header)
    }

    fn create_named(
        &mut self,
        workspace: WorkspaceId,
        name: &ScheduleName,
        header: &ScheduleHeader,
    ) -> PortResult<DocumentId> {
        // The ScheduleId-derived candidate is durable create identity. The
        // request hash only protects the Store's short-lived transport replay
        // receipt; candidate recovery deliberately wins after content exists.
        let request_hash = authored_request_hash(name, header)?;
        let document = ScheduleDocument::create_named(header, name)?;
        let initial = document.export_updates_since(&VersionVector::default())?;
        let title = format!("schedule-{}", &header.schedule.to_storage()[..8]);
        self.client
            .store()
            .seed_document_with_candidate(
                workspace,
                &title,
                SCHEDULE_KIND,
                None,
                None,
                schedule_document_id(header.schedule),
                &header.schedule.to_storage(),
                &request_hash,
                initial,
            )
            .map(|seeded| seeded.id)
            .map_err(schedule_seed_error)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        let snapshot = self
            .client
            .store()
            .snapshot(workspace, document)
            .map_err(|error| schedule_error("schedule_snapshot", error))?;
        let doc = ScheduleDocument::from_snapshot(&snapshot)?;
        let acked = doc.version();
        let link = Link::join(&self.client, document, &doc, acked).map_err(schedule_link_error)?;
        Ok(Box::new(StoreScheduleHandle {
            client: self.client.clone(),
            workspace,
            document,
            doc,
            link,
        }))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        discover_catalog(&self.client)
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        // The version-fenced deterministic seed operation performs the
        // all-workspace (including deleted and hidden rows) identity check
        // atomically under Store's mutation lock. This active catalog still
        // gives the runtime its canonical replay projection without exposing
        // deleted workspace content through HTTP.
        discover_catalog(&self.client)
    }

    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        let rooms = watch_rooms(&self.client, targets)?;
        let control = Arc::new(WatchControl::new());
        if rooms.is_empty() {
            return Ok(Box::new(Watch {
                control,
                thread: None,
            }));
        }
        let connection = connect_exact_rooms(&self.client, &rooms, &control, &wake)?;
        let thread_control = Arc::clone(&control);
        let client = self.client.clone();
        let thread = thread::Builder::new()
            .name("delegate-schedule-watch".to_owned())
            .spawn(move || drive_watch(client, connection, thread_control, wake))
            .map_err(|error| schedule_error("watch_thread", error))?;
        Ok(Box::new(Watch {
            control,
            thread: Some(thread),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::mpsc;

    #[test]
    fn routine_expiry_production_binding_rejects_matching_header_at_wrong_physical_document() {
        let mut header = header();
        let workspace = WorkspaceId::from_uuid(header.initial_owner.as_uuid());
        let association = koru_delegate_schedule::RoutineAssociation::new(
            koru_delegate_core::BotOwner {
                user_scope: koru_store_core::UserScopeId::from_uuid(header.initial_owner.as_uuid()),
                device: header.initial_owner,
            },
            koru_delegate_core::BotId::mint(),
            workspace,
            koru_delegate_core::BotRequestId::mint(),
        )
        .unwrap();
        header.definition.routine = Some(association.clone());
        header.definition.lifecycle = koru_delegate_schedule::ScheduleLifecycle::Paused {
            reason: koru_delegate_schedule::SchedulePauseReason::Manual,
        };
        let doc = ScheduleDocument::create(&header).unwrap();
        header.definition.revision = 2;
        header.definition.lifecycle = koru_delegate_schedule::ScheduleLifecycle::Active;
        doc.update_definition(header.initial_owner, &header.definition)
            .unwrap();
        let plan = koru_delegate_document::RoutineExpiryPlan {
            occurrence_ms: 60_000,
            planned_run: koru_delegate_core::RunId::mint(),
            representative_miss: None,
        };
        assert!(
            expired_routine_authority_at(
                DocumentId::new(),
                workspace,
                &doc,
                header.initial_owner,
                plan
            )
            .is_err()
        );
        let canonical = DocumentId::from_uuid(header.schedule.as_uuid());
        assert!(
            expired_routine_authority_at(
                canonical,
                WorkspaceId::from_uuid(DocumentId::new().as_uuid()),
                &doc,
                header.initial_owner,
                plan
            )
            .is_err()
        );
        let facts =
            expired_routine_authority_at(canonical, workspace, &doc, header.initial_owner, plan)
                .unwrap()
                .unwrap()
                .into_expired_bot_routine()
                .unwrap();
        assert_eq!(facts.slot().schedule, header.schedule);
        assert_eq!(facts.slot().owner, association.owner);
        assert_eq!(facts.planned_run(), plan.planned_run);
        assert_eq!(facts.workspace(), workspace);
    }

    #[test]
    fn routine_claim_location_requires_canonical_physical_store_document() {
        let schedule = koru_delegate_core::ScheduleId::mint();
        let canonical = DocumentId::from_uuid(schedule.as_uuid());
        assert!(routine_claim_location(canonical, Some(schedule)).is_ok());
        assert!(routine_claim_location(DocumentId::new(), Some(schedule)).is_err());
        assert!(routine_claim_location(DocumentId::new(), None).is_ok());
    }

    fn header() -> ScheduleHeader {
        ScheduleHeader {
            schedule: koru_delegate_core::ScheduleId::mint(),
            created_at_ms: 10,
            initial_owner: koru_delegate_core::DeviceId::new(),
            definition: koru_delegate_schedule::ScheduleDefinition {
                routine: None,
                revision: 1,
                effective_at_ms: 10,
                template: koru_delegate_schedule::DelegationTemplate {
                    requested_by: koru_delegate_core::ActorRef::Person {
                        profile: koru_delegate_core::ProfileId::new(),
                    },
                    objective: koru_delegate_core::Objective::new("authored intent").unwrap(),
                    agent: koru_delegate_core::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                    requested_model: Some(koru_delegate_core::ModelName::new("gpt-test").unwrap()),
                    scope: koru_delegate_core::WorkspaceScope::Temporary,
                    grants: BTreeSet::new(),
                    context: Vec::new(),
                },
                recurrence: koru_delegate_schedule::RecurrenceRule::floating(
                    "0 9 * * *".parse().unwrap(),
                ),
                lifecycle: koru_delegate_schedule::ScheduleLifecycle::Active,
            },
        }
    }

    #[test]
    fn schedule_retry_hash_covers_authored_intent_not_worker_stamps() {
        let first = header();
        let name = ScheduleName::new("Research competitors").unwrap();
        let mut retry = first.clone();
        retry.created_at_ms = 99;
        retry.initial_owner = koru_delegate_core::DeviceId::new();
        retry.definition.revision = 7;
        retry.definition.effective_at_ms = 99;
        assert_eq!(
            authored_request_hash(&name, &first).unwrap(),
            authored_request_hash(&name, &retry).unwrap()
        );

        retry.definition.template.objective =
            koru_delegate_core::Objective::new("different intent").unwrap();
        assert_ne!(
            authored_request_hash(&name, &first).unwrap(),
            authored_request_hash(&name, &retry).unwrap()
        );

        let renamed = ScheduleName::new("Research suppliers").unwrap();
        assert_ne!(
            authored_request_hash(&name, &first).unwrap(),
            authored_request_hash(&renamed, &first).unwrap()
        );
    }

    #[test]
    fn schedule_id_is_also_its_cross_root_store_document_identity() {
        let schedule = koru_delegate_core::ScheduleId::mint();
        assert_eq!(schedule_document_id(schedule).as_uuid(), schedule.as_uuid());
    }

    #[test]
    fn seeded_create_conflict_crosses_as_a_stable_schedule_code() {
        let error = schedule_seed_error(koru_store_core::Error::bare(
            koru_store_core::Code::IdempotencyConflict,
        ));
        assert_eq!(error.code, "schedule_conflict");
    }

    #[test]
    fn exact_room_names_do_not_overlap_unrelated_documents() {
        let workspace = WorkspaceId::new();
        let schedule = DocumentId::new();
        let unrelated = DocumentId::new();
        assert_eq!(
            manifest_room(workspace),
            format!("workspace:{}:manifest", workspace.to_storage())
        );
        assert_eq!(
            document_room(schedule),
            format!("document:{}", schedule.to_storage())
        );
        assert_ne!(document_room(schedule), document_room(unrelated));
    }

    #[test]
    fn closing_an_unhealthy_watch_interrupts_its_backoff() {
        let control = Arc::new(WatchControl::new());
        let waiter = Arc::clone(&control);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            entered_tx.send(()).unwrap();
            waiter.wait_to_retry();
            done_tx.send(()).unwrap();
        });

        entered_rx.recv().unwrap();
        control.stop();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown should unblock retry backoff immediately");
        thread.join().unwrap();
    }
}
