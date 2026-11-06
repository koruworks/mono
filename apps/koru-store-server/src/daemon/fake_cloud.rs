// SPDX-License-Identifier: MPL-2.0

//! A Cloud worker in memory, for tests of a whole pass.
//!
//! It speaks the real carrier (`transfer::Stream` over a duplex) and holds
//! each unit as a Loro document: a join is answered with its version, a unit
//! the client lacks is sent, an upload is imported and acknowledged, and a
//! comparison answers that nothing matches so every document is exchanged.
//! Its acknowledgement is an explicit model, not a durability claim. Units
//! outlive a session, so one device can upload and another download.
//!
//! **What it does not model** is the worker's authority: every unit the scope
//! can name is served. The stage and direction rules are the worker's own
//! tests (Cloud MCP Plan 1.3).

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_device::cloud::CloudId;
use koru_network_iroh::cloud::records;
use koru_replication::cloud::{
    Bytes, HandshakeV1, PreparedSession, Version1,
    transfer::{self, Admission, Control, MemoryBudget, Record, Scope, Stream, Unit, metadata},
};
use koru_replication::protocol::PeerFrame;
use loro::{ExportMode, LoroDoc, VersionVector};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};
use tokio::time::Instant;

use super::cloud_sync::{Session, driver::Opened};

/// How long the worker waits for the client's next record. Longer than any
/// test runs, so a quiet client is never mistaken for a gone one.
const QUIET: Duration = Duration::from_secs(120);

#[derive(Clone, Default)]
pub(crate) struct FakeCloud {
    units: Arc<Mutex<HashMap<Unit, LoroDoc>>>,
    /// While set, an upload is received whole and never acknowledged.
    hold_uploads: Arc<std::sync::atomic::AtomicBool>,
    /// While set, a session is closed as soon as it opens.
    close_at_once: Arc<std::sync::atomic::AtomicBool>,
    pub log: Arc<Mutex<Log>>,
    held: Arc<tokio::sync::Notify>,
}

/// What every session of this worker saw.
#[derive(Debug, Default)]
pub(crate) struct Log {
    pub joined: Vec<Unit>,
    pub uploaded: Vec<Unit>,
    pub downloaded: Vec<Unit>,
    pub held: Vec<Unit>,
    pub sessions_closed: usize,
}

impl FakeCloud {
    /// The unit's state in Cloud, if any.
    pub fn unit(&self, unit: Unit) -> Option<LoroDoc> {
        let units = self.units.lock().unwrap();
        units.get(&unit).map(|doc| {
            let copy = LoroDoc::new();
            copy.import(&doc.export(ExportMode::Snapshot).unwrap())
                .unwrap();
            copy
        })
    }

    /// Put bytes into Cloud as a unit's content, as another device's upload
    /// would.
    pub fn put(&self, unit: Unit, bytes: &[u8]) {
        let mut units = self.units.lock().unwrap();
        let doc = units.entry(unit).or_default();
        doc.import(bytes).unwrap();
    }

    pub fn close_at_once(&self, close: bool) {
        self.close_at_once
            .store(close, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn hold_uploads(&self, hold: bool) {
        self.hold_uploads
            .store(hold, std::sync::atomic::Ordering::SeqCst);
    }

    /// Wait until an upload is being held.
    pub async fn upload_held(&self) {
        loop {
            let notified = self.held.notified();
            if !self.log.lock().unwrap().held.is_empty() {
                return;
            }
            notified.await;
        }
    }

    /// One admitted session over `scope`, served until the client closes it.
    pub fn session(
        &self,
        scope: Scope,
        budget: MemoryBudget,
    ) -> Opened<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>> {
        let accepted = accepted(scope);
        // The worker's accounting is its own, so the client's budget measures
        // the client's carrier alone.
        let worker_budget = MemoryBudget::new(256 * 1024 * 1024);
        let server = Stream::new(
            &accepted,
            scope,
            0,
            0,
            transfer::MAX_PAYLOAD,
            worker_budget.clone(),
        )
        .unwrap();
        let (client, peer) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(client);
        tokio::spawn(self.clone().serve(peer, server, scope, worker_budget));
        Opened {
            reader,
            writer,
            session: Session {
                accepted,
                scope,
                automatic_limit: transfer::MAX_PAYLOAD,
                capacity: NonZeroUsize::new(8).unwrap(),
                budget,
                observed: Instant::now(),
                wall_millis: 0,
            },
        }
    }

    async fn serve(
        self,
        mut peer: DuplexStream,
        mut stream: Stream,
        scope: Scope,
        budget: MemoryBudget,
    ) {
        let deadline = || Instant::now() + QUIET;
        if self.close_at_once.load(std::sync::atomic::Ordering::SeqCst) {
            drop(peer);
            return self.closed();
        }
        if records::write(
            &mut peer,
            &Record::Control(stream.initial_policy(0).unwrap()),
            deadline(),
            &budget,
        )
        .await
        .is_err()
        {
            return self.closed();
        }
        // The unit of the upload in flight, and whether it is being held.
        let mut incoming: Option<(Unit, bool)> = None;
        loop {
            let Ok(received) = records::read(&mut peer, deadline(), &budget).await else {
                return self.closed();
            };
            match received.into_parts().1 {
                Record::Control(control) => {
                    let start = match &control {
                        Control::TransferStart(start) => Some(start.clone()),
                        _ => None,
                    };
                    if let Some(start) = &start {
                        incoming = Some((start.unit, false));
                    }
                    let Ok(admission) = stream.receive_control(control, 0, 0, |_| Ok(())) else {
                        return self.closed();
                    };
                    if admission == Admission::AwaitDecision
                        && let Some(start) = start
                    {
                        let decision = stream
                            .decide(&start, transfer::Decision::Accept, 0, |_| Ok(()))
                            .unwrap();
                        if records::write(
                            &mut peer,
                            &Record::Control(decision),
                            deadline(),
                            &budget,
                        )
                        .await
                        .is_err()
                        {
                            return self.closed();
                        }
                    }
                }
                Record::Peer(frame) => {
                    let Ok((_, frame)) = stream.receive_peer_owned(frame, 0, |_| Ok(())) else {
                        return self.closed();
                    };
                    if let Some(frame) = frame
                        && self
                            .metadata(&mut peer, &mut stream, scope, &budget, frame)
                            .await
                            .is_err()
                    {
                        return self.closed();
                    }
                    if stream.status().incoming_complete
                        && let Some((unit, held)) = incoming
                        && !held
                    {
                        if self.hold_uploads.load(std::sync::atomic::Ordering::SeqCst) {
                            incoming = Some((unit, true));
                            self.log.lock().unwrap().held.push(unit);
                            self.held.notify_waiters();
                        } else if self
                            .received(&mut peer, &mut stream, scope, &budget)
                            .await
                            .is_err()
                        {
                            return self.closed();
                        } else {
                            incoming = None;
                        }
                    }
                }
            }
        }
    }

    fn closed(&self) {
        self.log.lock().unwrap().sessions_closed += 1;
    }

    async fn metadata(
        &self,
        peer: &mut DuplexStream,
        stream: &mut Stream,
        scope: Scope,
        budget: &MemoryBudget,
        frame: PeerFrame,
    ) -> Result<(), transfer::Error> {
        let deadline = Instant::now() + QUIET;
        if let PeerFrame::CloudCompare { workspace_id, .. } = &frame {
            // Nothing matches: every document is exchanged, which is the
            // case a cold replica is in.
            let answer = Record::Peer(PeerFrame::CloudCompared {
                workspace_id: *workspace_id,
                matching: Vec::new(),
            });
            return records::write(peer, &answer, deadline, budget).await;
        }
        for message in metadata::decode(scope, frame, 64)? {
            let metadata::Metadata::Request(request) = message else {
                continue;
            };
            self.log.lock().unwrap().joined.push(request.unit);
            let (version, payload) = {
                let mut units = self.units.lock().unwrap();
                let doc = units.entry(request.unit).or_default();
                let version = doc.oplog_vv().encode();
                let known = request
                    .known_version
                    .as_ref()
                    .map(|known| VersionVector::decode(known).unwrap())
                    .unwrap_or_default();
                let payload = request.needs_payload(&version)?.then(|| {
                    if known.is_empty() {
                        doc.export(ExportMode::Snapshot)
                    } else {
                        doc.export(ExportMode::updates(&known))
                    }
                    .unwrap()
                });
                (version, payload)
            };
            if let Some(response) = request.response(scope, version)? {
                records::write(peer, &response, deadline, budget).await?;
            }
            if let Some(bytes) = payload {
                let (start, _) =
                    stream.begin_outgoing(request.unit, [7; 8], bytes, 0, 0, |_| Ok(()))?;
                records::write(peer, &Record::Control(start), deadline, budget).await?;
                while let Some(record) = stream.next_outgoing(0, |_| Ok(()))? {
                    records::write(peer, record.record(), deadline, budget).await?;
                }
                stream.outbound_written(0)?;
                self.log.lock().unwrap().downloaded.push(request.unit);
            }
        }
        Ok(())
    }

    async fn received(
        &self,
        peer: &mut DuplexStream,
        stream: &mut Stream,
        scope: Scope,
        budget: &MemoryBudget,
    ) -> Result<(), transfer::Error> {
        let complete = stream.handoff(0, |_| Ok(()), |complete| complete)?;
        {
            let mut units = self.units.lock().unwrap();
            let doc = units.entry(complete.unit).or_default();
            doc.import(&complete.bytes)
                .map_err(|_| transfer::Error::Invalid)?;
        }
        self.log.lock().unwrap().uploaded.push(complete.unit);
        records::write(
            peer,
            &scope.committed_ack(complete.unit, complete.batch_id)?,
            Instant::now() + QUIET,
            budget,
        )
        .await
    }
}

fn accepted(scope: Scope) -> PreparedSession {
    PreparedSession {
        principal: scope.principal,
        generation: 1,
        expires_at_seconds: 600,
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([1; 32]),
            generation: 1,
            expires_at_seconds: 600,
        },
    }
}

/// A Workspace session's scope.
pub(crate) fn workspace_scope(
    workspace: koru_store_core::WorkspaceId,
    profile: koru_store_core::ProfileId,
    manifest: koru_store_core::DocumentId,
) -> Scope {
    let cloud = |uuid: uuid::Uuid| CloudId::from_bytes(*uuid.as_bytes());
    Scope {
        principal: koru_device::cloud::CloudReplicaPrincipalV1::Workspace {
            workspace_id: cloud(workspace.as_uuid()),
            incarnation_id: cloud(uuid::Uuid::new_v4()),
            logical_replica_id: cloud(uuid::Uuid::new_v4()),
        },
        eligibility_workspace: workspace,
        profile: cloud(profile.as_uuid()),
        manifest,
    }
}

/// A document unit of a Workspace.
pub(crate) fn document_unit(
    workspace: koru_store_core::WorkspaceId,
    document: koru_store_core::DocumentId,
) -> Unit {
    let cloud = |uuid: uuid::Uuid| CloudId::from_bytes(*uuid.as_bytes());
    Unit::Document {
        workspace_id: cloud(workspace.as_uuid()),
        document_id: cloud(document.as_uuid()),
    }
}

/// A session source over a [`FakeCloud`]: up to `sessions` sessions over one
/// scope, recording the stage each was asked for.
pub(crate) struct Sessions {
    cloud: FakeCloud,
    scope: Scope,
    budget: MemoryBudget,
    remaining: usize,
    pub asked: Arc<Mutex<Vec<super::cloud_sync::Stage>>>,
}

impl Sessions {
    pub fn new(cloud: FakeCloud, scope: Scope, budget: MemoryBudget, sessions: usize) -> Self {
        Self {
            cloud,
            scope,
            budget,
            remaining: sessions,
            asked: Arc::default(),
        }
    }
}

impl super::cloud_sync::driver::SessionSource for Sessions {
    type Reader = ReadHalf<DuplexStream>;
    type Writer = WriteHalf<DuplexStream>;

    fn open(
        &mut self,
        stage: super::cloud_sync::Stage,
    ) -> impl Future<Output = Option<Opened<Self::Reader, Self::Writer>>> + Send {
        self.asked.lock().unwrap().push(stage);
        let opened = (self.remaining > 0).then(|| {
            self.remaining -= 1;
            self.cloud.session(self.scope, self.budget.clone())
        });
        std::future::ready(opened)
    }
}
