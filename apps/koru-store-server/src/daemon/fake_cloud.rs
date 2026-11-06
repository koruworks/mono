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
//!
//! **Faults a test can arm**, each a port-level stand-in for what the real
//! worker does (Cloud MCP Plan 2.3): first creation wins (C23's rule, which
//! the real worker applies to an app connection's uploads only), a refusal
//! frame answering an upload, a commit whose acknowledgement is lost with the
//! session, a join that is never answered, and a checkpoint that rewrites a
//! unit's stored history as one history-retaining snapshot.
//!
//! **An app connection's session** ([`FakeCloud::app_session`], Cloud MCP
//! Plan 3.4) may carry a claim reference on each upload, which the log
//! records; a test can claim a unit, and an upload of it whose reference is
//! not the claim's is then refused `sync_document_claimed` and not stored —
//! a port-level stand-in for the worker's claim rule (Plan 3.3), which also
//! judges the claim's connection and its expiry.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_device::cloud::CloudId;
use koru_network_iroh::cloud::records;
use koru_replication::cloud::{
    Bytes, HandshakeV1, PreparedSession, Version1,
    transfer::{
        self, Admission, ClaimReferences, Control, MemoryBudget, Record, Scope, Stream, Unit,
        metadata,
    },
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
pub struct FakeCloud {
    units: Arc<Mutex<HashMap<Unit, LoroDoc>>>,
    /// While set, an upload is received whole and never acknowledged.
    hold_uploads: Arc<std::sync::atomic::AtomicBool>,
    /// While set, a session is closed as soon as it opens.
    close_at_once: Arc<std::sync::atomic::AtomicBool>,
    /// While set, an upload rooted at the empty document for a unit Cloud
    /// already holds is refused `sync_unit_exists` and not stored: the rule
    /// the real worker applies to an app connection (TECHNICAL C23).
    first_creation_wins: Arc<std::sync::atomic::AtomicBool>,
    /// While set, every upload is answered with this refusal frame instead of
    /// being stored, and the session ends.
    refuse_uploads: Arc<Mutex<Option<String>>>,
    /// While set, an upload is stored and the session then closes before it
    /// is acknowledged: a commit whose acknowledgement is lost.
    lose_acknowledgements: Arc<std::sync::atomic::AtomicBool>,
    /// While set, a join is received and never answered.
    hold_joins: Arc<std::sync::atomic::AtomicBool>,
    /// While set, a held upload outlives its session, as the worker's
    /// retained work does: it is judged when released, stored or refused.
    retain_held: Arc<std::sync::atomic::AtomicBool>,
    /// While set, the upload faults above apply to this unit's uploads only.
    target: Arc<Mutex<Option<Target>>>,
    /// Units under a claim, and the claim's reference: an upload of one that
    /// carries another reference, or none, is refused `sync_document_claimed`.
    claims: Arc<Mutex<HashMap<Unit, [u8; 32]>>>,
    /// Bumped to let every held upload through.
    release: Arc<tokio::sync::watch::Sender<u64>>,
    pub log: Arc<Mutex<Log>>,
    held: Arc<tokio::sync::Notify>,
}

/// Which uploads an armed fault applies to.
#[derive(Clone, Copy, Debug)]
enum Target {
    Unit(Unit),
    Fresh,
}

/// What every session of this worker saw.
#[derive(Debug, Default)]
pub struct Log {
    pub joined: Vec<Unit>,
    pub uploaded: Vec<Unit>,
    pub downloaded: Vec<Unit>,
    pub held: Vec<Unit>,
    /// Uploads answered with a refusal frame, and its code.
    pub refused: Vec<(Unit, String)>,
    /// Joins received and never answered.
    pub held_joins: Vec<Unit>,
    pub sessions_closed: usize,
    /// Every complete upload, in order, with the claim reference its start
    /// carried.
    pub references: Vec<(Unit, Option<[u8; 32]>)>,
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

    /// Hold every upload received from now on: received whole and not
    /// acknowledged until [`FakeCloud::release_held`]. Turning it off holds no
    /// new upload and leaves the held ones held.
    pub fn hold_uploads(&self, hold: bool) {
        self.hold_uploads
            .store(hold, std::sync::atomic::Ordering::SeqCst);
    }

    /// Let every held upload through, as if it had just arrived unheld: it is
    /// stored and acknowledged, or refused by an armed rule.
    pub fn release_held(&self) {
        self.release.send_modify(|released| *released += 1);
    }

    /// Aim the upload faults at one unit's uploads, or at all of them.
    pub fn target(&self, unit: Option<Unit>) {
        *self.target.lock().unwrap() = unit.map(Target::Unit);
    }

    /// Aim the upload faults at units Cloud does not hold yet: a new
    /// document's first content.
    pub fn target_fresh(&self) {
        *self.target.lock().unwrap() = Some(Target::Fresh);
    }

    /// Wait until at least `count` uploads have been held.
    pub async fn uploads_held(&self, count: usize) {
        loop {
            let notified = self.held.notified();
            if self.log.lock().unwrap().held.len() >= count {
                return;
            }
            notified.await;
        }
    }

    pub fn first_creation_wins(&self, on: bool) {
        self.first_creation_wins
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Answer every upload with `code`'s refusal frame, or stop doing so.
    pub fn refuse_uploads(&self, code: Option<&str>) {
        *self.refuse_uploads.lock().unwrap() = code.map(str::to_owned);
    }

    pub fn lose_acknowledgements(&self, lose: bool) {
        self.lose_acknowledgements
            .store(lose, std::sync::atomic::Ordering::SeqCst);
    }

    /// Keep a held upload past the end of its session, until it is released:
    /// an old upload from a replica that has since been fenced or a host that
    /// restarted. Off, a held upload dies with its session.
    pub fn retain_held(&self, retain: bool) {
        self.retain_held
            .store(retain, std::sync::atomic::Ordering::SeqCst);
    }

    /// Claim `unit` under `reference`, or end its claim with `None`.
    pub fn claim(&self, unit: Unit, reference: Option<[u8; 32]>) {
        let mut claims = self.claims.lock().unwrap();
        match reference {
            Some(reference) => {
                claims.insert(unit, reference);
            }
            None => {
                claims.remove(&unit);
            }
        }
    }

    pub fn hold_joins(&self, hold: bool) {
        self.hold_joins
            .store(hold, std::sync::atomic::Ordering::SeqCst);
    }

    /// Every unit Cloud holds now.
    pub fn units(&self) -> Vec<Unit> {
        self.units.lock().unwrap().keys().copied().collect()
    }

    /// Rewrite a unit's stored history as one history-retaining snapshot, as
    /// a worker checkpoint retires the segments before it. The content and
    /// every change, with its commit message, are kept.
    pub fn checkpoint(&self, unit: Unit) {
        let mut units = self.units.lock().unwrap();
        if let Some(doc) = units.get_mut(&unit) {
            let snapshot = doc.export(ExportMode::Snapshot).unwrap();
            let fresh = LoroDoc::new();
            fresh.import(&snapshot).unwrap();
            *doc = fresh;
        }
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

    /// One admitted session over `scope`, served until the client closes it:
    /// a device's, whose uploads carry no claim reference.
    pub fn session(
        &self,
        scope: Scope,
        budget: MemoryBudget,
    ) -> Opened<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>> {
        self.open(scope, budget, false)
    }

    /// The same for an app connection's replica: its uploads may carry a
    /// claim reference, as a worker serving the connection receives one.
    pub fn app_session(
        &self,
        scope: Scope,
        budget: MemoryBudget,
    ) -> Opened<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>> {
        self.open(scope, budget, true)
    }

    fn open(
        &self,
        scope: Scope,
        budget: MemoryBudget,
        app_connection: bool,
    ) -> Opened<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>> {
        let (client_end, worker_end) = if app_connection {
            (ClaimReferences::Sent, ClaimReferences::Received)
        } else {
            (ClaimReferences::None, ClaimReferences::None)
        };
        let accepted = accepted_as(scope, client_end);
        // The worker's accounting is its own, so the client's budget measures
        // the client's carrier alone.
        let worker_budget = MemoryBudget::new(256 * 1024 * 1024);
        let server = Stream::new(
            &accepted_as(scope, worker_end),
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
        let mut released = self.release.subscribe();
        loop {
            let read = records::read(&mut peer, deadline(), &budget);
            let received = if matches!(incoming, Some((_, true))) {
                tokio::select! {
                    received = read => Some(received),
                    _ = released.changed() => None,
                }
            } else {
                Some(read.await)
            };
            let Some(received) = received else {
                // A held upload let through: answered as if it had just
                // arrived.
                if self
                    .received(&mut peer, &mut stream, scope, &budget)
                    .await
                    .is_err()
                {
                    return self.closed();
                }
                incoming = None;
                continue;
            };
            let Ok(received) = received else {
                if matches!(incoming, Some((_, true)))
                    && self.retain_held.load(std::sync::atomic::Ordering::SeqCst)
                {
                    // Retained past its session: judged once released.
                    let _ = released.changed().await;
                    let _ = self.received(&mut peer, &mut stream, scope, &budget).await;
                }
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
                        let aimed = match *self.target.lock().unwrap() {
                            None => true,
                            Some(Target::Unit(target)) => target == unit,
                            Some(Target::Fresh) => self
                                .units
                                .lock()
                                .unwrap()
                                .get(&unit)
                                .is_none_or(|doc| doc.oplog_vv().is_empty()),
                        };
                        if aimed && let Some(code) = self.refusal(&mut stream) {
                            self.log.lock().unwrap().refused.push((unit, code.clone()));
                            let _ = records::write(
                                &mut peer,
                                &Record::Peer(PeerFrame::Error { code }),
                                deadline(),
                                &budget,
                            )
                            .await;
                            return self.closed();
                        }
                        if aimed
                            && self
                                .lose_acknowledgements
                                .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            if let Ok(complete) = stream.handoff(0, |_| Ok(()), |complete| complete)
                            {
                                let mut units = self.units.lock().unwrap();
                                let _ = units
                                    .entry(complete.unit)
                                    .or_default()
                                    .import(&complete.bytes);
                            }
                            self.log.lock().unwrap().uploaded.push(unit);
                            drop(peer);
                            return self.closed();
                        }
                        if aimed && self.hold_uploads.load(std::sync::atomic::Ordering::SeqCst) {
                            released.mark_unchanged();
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

    /// The refusal an armed fault answers this complete upload with, if any.
    /// A refused upload is taken off the carrier and never stored.
    fn refusal(&self, stream: &mut Stream) -> Option<String> {
        let code = self.refuse_uploads.lock().unwrap().clone()?;
        let _ = stream.handoff(0, |_| Ok(()), |complete| complete);
        Some(code)
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
            if self.hold_joins.load(std::sync::atomic::Ordering::SeqCst) {
                self.log.lock().unwrap().held_joins.push(request.unit);
                continue;
            }
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
        self.log
            .lock()
            .unwrap()
            .references
            .push((complete.unit, complete.claim));
        let claimed = self.claims.lock().unwrap().get(&complete.unit).copied();
        if claimed.is_some_and(|claim| complete.claim != Some(claim)) {
            // Refused before anything is stored, as the worker refuses before
            // `reserve` or rolls the publish back: the unit does not move.
            let code = transfer::Error::DocumentClaimed.code().to_owned();
            self.log
                .lock()
                .unwrap()
                .refused
                .push((complete.unit, code.clone()));
            let _ = records::write(
                peer,
                &Record::Peer(PeerFrame::Error { code }),
                Instant::now() + QUIET,
                budget,
            )
            .await;
            return Err(transfer::Error::DocumentClaimed);
        }
        if self
            .first_creation_wins
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let held = self
                .units
                .lock()
                .unwrap()
                .get(&complete.unit)
                .is_some_and(|doc| !doc.oplog_vv().is_empty());
            let creation = koru_replication::cloud::object::starts_from_empty(&complete.bytes)
                .unwrap_or(false);
            if held && creation {
                // Refused before anything is stored, as the worker refuses
                // before `reserve`.
                let code = transfer::Error::UnitExists.code().to_owned();
                self.log
                    .lock()
                    .unwrap()
                    .refused
                    .push((complete.unit, code.clone()));
                let _ = records::write(
                    peer,
                    &Record::Peer(PeerFrame::Error { code }),
                    Instant::now() + QUIET,
                    budget,
                )
                .await;
                return Err(transfer::Error::UnitExists);
            }
        }
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

/// The admitted session, with the claim references its end may carry.
fn accepted_as(scope: Scope, claim_references: ClaimReferences) -> PreparedSession {
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
        claim_references,
    }
}

/// A Workspace session's scope.
pub fn workspace_scope(
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
pub fn document_unit(
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
pub struct Sessions {
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
