// SPDX-License-Identifier: MPL-2.0

//! The Run documents, through the gateway — never a direct SQLite open.
//!
//! §7.1 step 3 is the Store's **seeded create**: the manifest entry and
//! the document's first bytes land in one durable operation, idempotent by
//! the run id, so a retry after a lost answer finds the document it made.
//! Every commit after it goes over the Store's **sync socket** — the room
//! `document:<id>`, joined once per open handle. A commit *sends* and
//! returns; the Store's acknowledgement that the update is durable arrives
//! on the same socket and is taken in on the next pass. That split is the
//! point: the actor that commits is the one thread every request on this
//! device is answered from, and a durable acknowledgement costs what the
//! Store's disk owes at that moment, not what this call did. Where
//! durability *is* the answer — a person's request being confirmed, a fact
//! about to travel (§17.1) — [`Link::settle`] waits for it, bounded, and
//! [`Link::durable`] says whether there is anything to wait for.
//!
//! Reads are snapshots over HTTP, the way every reader opens a run.

use std::{
    collections::VecDeque,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use koru_api_client::Client;
use koru_delegate_core::{DocumentId, RunHeader, WorkspaceId};
use koru_delegate_document::{RUN_KIND, RunDocument, RunDocumentError};
use koru_delegate_runtime::{
    ListedRun, Port, PortError, PortResult, RunDocumentHandle, RunDocuments,
};
use koru_store_client::sync::{Ack, BatchId, Connection, Event};
use loro::VersionVector;

use crate::LaunchBootstrap;

/// How long one commit waits for the Store's durable acknowledgement.
const DURABILITY_DEADLINE: Duration = Duration::from_secs(15);
/// How long a join waits for the Store to answer it.
const JOIN_DEADLINE: Duration = Duration::from_secs(10);
/// The socket's read timeout, so a wait can check its deadline.
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// The gateway, connected.
pub struct Gateway {
    pub client: Client,
}

/// Connect to the gateway the Agent supervises, waiting for it to publish.
pub fn connect_gateway(
    bootstrap: &LaunchBootstrap,
    explicit_root: Option<&Path>,
    deadline: Instant,
    stop: &mpsc::Receiver<&'static str>,
) -> Result<Gateway, crate::Wait> {
    // The documents the worker reads and commits, and the journal it
    // publishes a run's facts to. `events.write` is asked for explicitly:
    // the gateway gives a client that named store scopes alone no events
    // scope at all, deliberately, and publishing is a write.
    // `artifacts.*` are asked for on the same grounds: issuing a run's
    // authority and revoking it are writes, and reading back what a run
    // registered is a read. Without them every delegated run reaches an
    // endpoint that refuses it (P1.11, `authority.rs`).
    const SCOPES: &[&str] = &[
        "store.read_documents",
        "store.edit_documents",
        "events.write",
        "artifacts.read",
        "artifacts.write",
    ];
    let mut last;
    loop {
        let attempt = match explicit_root {
            Some(root) => Client::connect_at_with_scopes(&bootstrap.segment, root, SCOPES),
            None => Client::connect_with_scopes(&bootstrap.segment, SCOPES),
        };
        match attempt {
            Ok(client) => return Ok(Gateway { client }),
            Err(error) => last = error.to_string(),
        }
        if Instant::now() >= deadline {
            return Err(crate::Wait::Failed(format!(
                "the gateway did not publish in time: {last}"
            )));
        }
        if stop.recv_timeout(Duration::from_millis(250)).is_ok() {
            return Err(crate::Wait::Stopped);
        }
    }
}

pub struct StoreDocuments {
    client: Client,
}

impl StoreDocuments {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

pub(crate) fn store_error(operation: &'static str, error: impl std::fmt::Display) -> PortError {
    PortError::new(Port::Documents, operation, error.to_string())
}

fn room(document: DocumentId) -> String {
    format!("document:{}", document.to_storage())
}

impl RunDocuments for StoreDocuments {
    fn publication_open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn koru_delegate_runtime::RunDocumentOpen>> {
        Ok(Box::new(PublicationOpen {
            client: self.client.clone(),
            workspace,
            document,
        }))
    }
    fn create(&mut self, workspace: WorkspaceId, header: &RunHeader) -> PortResult<DocumentId> {
        let document = RunDocument::create(header)?;
        let initial_update = document.export_updates_since(&VersionVector::default())?;
        // The title is the manifest's, and a run is hidden from Explore
        // (ADR-343): a short, content-free name that is not read anywhere.
        let title = format!("run-{}", &header.run.to_storage()[..8]);
        let seeded = self
            .client
            .store()
            .seed_document(
                workspace,
                &title,
                RUN_KIND,
                None,
                None,
                &header.run.to_storage(),
                &format!("koru-run:{}", header.run),
                initial_update,
            )
            .map_err(|error| store_error("seed", error))?;
        Ok(seeded.id)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentHandle>> {
        let snapshot = self
            .client
            .store()
            .snapshot(workspace, document)
            .map_err(|error| store_error("snapshot", error))?;
        let doc = RunDocument::from_snapshot(&snapshot)?;
        // The snapshot is the Store's own: everything in it is acknowledged.
        let acked = doc.version();
        let link = Link::join(&self.client, document, &doc, acked)?;
        Ok(Box::new(StoreHandle {
            client: self.client.clone(),
            workspace,
            document,
            doc,
            link,
            projection: koru_delegate_document::RunProjectionStaging::default(),
        }))
    }

    /// **Trashed runs are listed too**, with when they went in.
    ///
    /// A run in the trash is still a run — the Trash tab draws it and
    /// restoring puts it back whole — and the manifest's `trashed_at` is
    /// the whole of what says so, because a run *is* a document and its
    /// deletion is that document's move into the workspace's bin (ADR-69).
    /// Splitting the two halves is the orchestrator's.
    fn list(&mut self, workspace: WorkspaceId) -> PortResult<Vec<ListedRun>> {
        let entries = self
            .client
            .store()
            .documents(workspace)
            .map_err(|error| store_error("documents", error))?;
        let mut runs = Vec::new();
        for entry in entries.into_iter().filter(|entry| entry.kind == RUN_KIND) {
            let snapshot = self
                .client
                .store()
                .snapshot(workspace, entry.id)
                .map_err(|error| store_error("snapshot", error))?;
            let Ok(document) = RunDocument::from_snapshot(&snapshot) else {
                continue;
            };
            if let Ok(header) = document.header() {
                runs.push(ListedRun {
                    document: entry.id,
                    header,
                    trashed_at_ms: entry.trashed_at.map(|at| Some(at.millis())),
                });
            }
        }
        runs.sort_by_key(|listed| listed.header.created_at_ms);
        Ok(runs)
    }
}

struct PublicationOpen {
    client: Client,
    workspace: WorkspaceId,
    document: DocumentId,
}
impl koru_delegate_runtime::RunDocumentOpen for PublicationOpen {
    fn open(self: Box<Self>) -> PortResult<Box<dyn RunDocumentHandle>> {
        StoreDocuments::new(self.client).open(self.workspace, self.document)
    }
}

/// One joined room on the sync socket, and the version the Store holds.
pub(crate) trait SyncedDocument {
    fn version(&self) -> VersionVector;
    fn import(&self, bytes: &[u8]) -> Result<(), impl std::fmt::Display>;
    fn export_updates_since(
        &self,
        since: &VersionVector,
    ) -> Result<Vec<u8>, impl std::fmt::Display>;
}

impl SyncedDocument for RunDocument {
    fn version(&self) -> VersionVector {
        RunDocument::version(self)
    }
    fn import(&self, bytes: &[u8]) -> Result<(), impl std::fmt::Display> {
        RunDocument::import(self, bytes)
    }
    fn export_updates_since(
        &self,
        since: &VersionVector,
    ) -> Result<Vec<u8>, impl std::fmt::Display> {
        RunDocument::export_updates_since(self, since)
    }
}

pub(crate) struct Link {
    connection: Connection,
    room: String,
    /// What the Store has confirmed durable.
    pub(crate) acked: VersionVector,
    /// What has been sent: the point the next commit exports from. Ahead of
    /// `acked` by exactly the batches in flight.
    sent: VersionVector,
    /// Batches sent and not yet acknowledged, oldest first, each with the
    /// document version it carried the replica to.
    in_flight: VecDeque<(BatchId, VersionVector)>,
    /// A refusal or a room problem that arrived while nobody was waiting for
    /// it. Reported by the next commit or settle, so it is never lost.
    deferred: Option<PortError>,
}

impl Link {
    /// Join the document's room. `acked` is what the Store is known to
    /// hold: the snapshot's version on a first open, and the last
    /// acknowledged version on a rejoin — so a commit the socket lost is
    /// resent rather than assumed.
    pub(crate) fn join<D: SyncedDocument>(
        client: &Client,
        document: DocumentId,
        doc: &D,
        acked: VersionVector,
    ) -> PortResult<Self> {
        let mut connection = Connection::open(&client.store().discovery())
            .map_err(|error| store_error("sync_open", error))?;
        connection
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(|error| store_error("sync_timeout", error))?;
        let room = room(document);
        // The snapshot is what this replica holds; the Store decides what
        // is owed against the version it carries.
        connection
            .join(&room, &doc.version())
            .map_err(|error| store_error("sync_join", error))?;
        let deadline = Instant::now() + JOIN_DEADLINE;
        loop {
            match connection.next_event() {
                Ok(Some(Event::Joined { room: joined, .. })) if joined == room => break,
                Ok(Some(Event::JoinRefused { code, .. })) => {
                    return Err(store_error("sync_join_refused", format!("{code:?}")));
                }
                Ok(Some(Event::Updates { updates, .. })) => {
                    for update in updates {
                        doc.import(&update)
                            .map_err(|error| store_error("sync_import", error))?;
                    }
                }
                Ok(_) => {}
                Err(error) => return Err(store_error("sync_read", error)),
            }
            if Instant::now() >= deadline {
                return Err(store_error(
                    "sync_join",
                    "the Store did not answer the join",
                ));
            }
        }
        Ok(Self {
            connection,
            room,
            sent: acked.clone(),
            acked,
            in_flight: VecDeque::new(),
            deferred: None,
        })
    }

    /// Whether the Store has called every sent commit durable.
    pub(crate) fn durable(&self) -> bool {
        self.in_flight.is_empty() && self.deferred.is_none()
    }

    /// One acknowledgement, matched against what is in flight. A durable
    /// one settles it and everything sent before it; a refusal is kept for
    /// the next caller that asks.
    fn acknowledged(&mut self, ack: &Ack) {
        let Some(position) = self
            .in_flight
            .iter()
            .position(|(batch, _)| *batch == ack.batch)
        else {
            return;
        };
        if ack.durable {
            let (_, version) = self.in_flight[position].clone();
            self.in_flight.drain(..=position);
            self.acked = version;
        } else {
            self.in_flight.remove(position);
            self.deferred
                .get_or_insert_with(|| store_error("sync_refused", format!("{:?}", ack.status)));
        }
    }

    /// Take in what the socket has already delivered — acknowledgements and
    /// updates — waiting at most `wait` for the first of them.
    fn drain<D: SyncedDocument>(&mut self, doc: &D, wait: Duration) -> PortResult<()> {
        let _ = self.connection.set_read_timeout(Some(wait));
        let result = loop {
            match self.connection.next_event() {
                Ok(Some(Event::Acked { ack, .. })) => self.acknowledged(&ack),
                Ok(Some(Event::Updates { updates, .. })) => {
                    if let Err(error) = updates
                        .iter()
                        .try_for_each(|update| doc.import(update).map_err(|e| e.to_string()))
                    {
                        break Err(store_error("sync_import", error));
                    }
                }
                Ok(Some(Event::RoomProblem { code, .. })) => {
                    break Err(store_error("sync_room", format!("{code:?}")));
                }
                Ok(Some(_)) => {}
                Ok(None) => break Ok(()),
                Err(error) => break Err(store_error("sync_read", error)),
            }
        };
        let _ = self.connection.set_read_timeout(Some(READ_TIMEOUT));
        result
    }

    /// Wait, bounded, until nothing is in flight. The one place waiting
    /// belongs: before a person's request is answered, before a fact about
    /// the document travels.
    pub(crate) fn settle<D: SyncedDocument>(&mut self, doc: &D) -> PortResult<()> {
        let deadline = Instant::now() + DURABILITY_DEADLINE;
        while !self.in_flight.is_empty() {
            self.drain(doc, READ_TIMEOUT)?;
            if let Some(refused) = self.deferred.take() {
                return Err(refused);
            }
            if Instant::now() >= deadline {
                return Err(store_error(
                    "durability_timeout",
                    "the Store did not acknowledge the commit in time",
                ));
            }
        }
        self.deferred.take().map_or(Ok(()), Err)
    }

    /// Take in whatever the room has **already** delivered, and say whether
    /// anything did. Never waits: this runs on the actor's one thread, and a
    /// reader that blocked here would stall every other run on the device.
    ///
    /// It is how a device that is only *reading* a run sees the executing
    /// one's progress (D15, UC5). A run this device drives needs none of it
    /// — the orchestrator is the writer — so the caller asks only for the
    /// others, and only while somebody is watching.
    pub(crate) fn absorb<D: SyncedDocument>(&mut self, doc: &D) -> PortResult<bool> {
        let _ = self
            .connection
            .set_read_timeout(Some(Duration::from_millis(1)));
        let mut arrived = false;
        loop {
            match self.connection.next_event() {
                Ok(Some(Event::Updates { updates, .. })) => {
                    for update in updates {
                        doc.import(&update)
                            .map_err(|error| store_error("sync_import", error))?;
                    }
                    arrived = true;
                }
                Ok(Some(Event::Acked { ack, .. })) => self.acknowledged(&ack),
                // A room problem is news the next commit will report; a
                // drain is not the place to fail a read.
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        let _ = self.connection.set_read_timeout(Some(READ_TIMEOUT));
        Ok(arrived)
    }

    /// Send everything since the last send, and return. The acknowledgement
    /// is taken in by the next `drain`, `settle` or `absorb`; a refusal that
    /// arrived since the last call is reported here first, so no caller
    /// builds on a commit the Store threw away.
    pub(crate) fn commit<D: SyncedDocument>(&mut self, doc: &D) -> PortResult<()> {
        if let Some(refused) = self.deferred.take() {
            return Err(refused);
        }
        let now = doc.version();
        if now != self.sent {
            let update = doc
                .export_updates_since(&self.sent)
                .map_err(|error| store_error("sync_export", error))?;
            let batch = self
                .connection
                .send_update(&self.room, update)
                .map_err(|error| store_error("sync_send", error))?;
            self.in_flight.push_back((batch, now.clone()));
            self.sent = now;
        }
        // Whatever has already arrived, without waiting for what has not.
        self.drain(doc, Duration::from_millis(1))
    }
}

struct StoreHandle {
    client: Client,
    workspace: WorkspaceId,
    document: DocumentId,
    doc: RunDocument,
    link: Link,
    projection: koru_delegate_document::RunProjectionStaging,
}

mod projection;

impl RunDocumentHandle for StoreHandle {
    fn document(&self) -> &RunDocument {
        &self.doc
    }

    fn prepare_decision_projection(
        &mut self,
        intent: &koru_delegate_runtime::RunProjectionIntent,
    ) -> PortResult<koru_delegate_core::decisions::DecisionPreparedProjection> {
        self.prepare_projection(intent)
    }
    fn apply_prepared_projection(
        &mut self,
        prepared: &koru_delegate_core::decisions::DecisionPreparedProjection,
    ) -> PortResult<koru_delegate_runtime::AppliedRunProjection> {
        self.apply_projection(prepared)
    }
    fn discard_prepared_projection(
        &mut self,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
    ) -> PortResult<()> {
        self.projection.discard(transition)?;
        Ok(())
    }

    fn absorb(&mut self) -> PortResult<bool> {
        self.link.absorb(&self.doc)
    }

    fn durable(&self) -> bool {
        self.link.durable()
    }

    fn settle(&mut self) -> PortResult<()> {
        settle_link(&self.client, self.document, &self.doc, &mut self.link)
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        write(&self.doc)?;
        match self.link.commit(&self.doc) {
            Ok(()) => Ok(()),
            Err(first) => {
                // One reconnect: the socket may have been cut by a gateway
                // restart. The document still holds every operation, so the
                // rejoin names its version and the resend carries the rest.
                tracing::info!(
                    target: "koru::delegate",
                    code = %first.code,
                    "the sync socket refused a commit; rejoining once"
                );
                let acked = self.link.acked.clone();
                self.link = Link::join(&self.client, self.document, &self.doc, acked)?;
                self.link.commit(&self.doc)
            }
        }
    }
}

fn settle_link(
    client: &Client,
    document: DocumentId,
    doc: &RunDocument,
    link: &mut Link,
) -> PortResult<()> {
    match link.settle(doc) {
        Ok(()) => Ok(()),
        Err(first) => {
            // The same one reconnect a commit gets: rejoin at the last
            // acknowledged version, resend what is above it, wait again.
            tracing::info!(
                target: "koru::delegate",
                code = %first.code,
                "the sync socket failed a settle; rejoining once"
            );
            let acked = link.acked.clone();
            *link = Link::join(client, document, doc, acked)?;
            link.commit(doc)?;
            link.settle(doc)
        }
    }
}
