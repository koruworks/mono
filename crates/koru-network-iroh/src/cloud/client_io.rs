// SPDX-License-Identifier: MPL-2.0

//! Client-side bidirectional transfer owner, below catalog/VV reconciliation.
//! It never opens an endpoint or grants authority. Actual Iroh callers enclose
//! its entire lifetime in session::with_fence. Store jobs must retain accepted
//! work independently of the result future (StoreWorkerClient does this).
use super::records::{self, Duplex};
use koru_replication::cloud::{
    PreparedSession, Refusal,
    client_store::Receipt,
    transfer::{
        self, Admission, Completed, Control, Decision, Error, MemoryBudget, Record, Reservation,
        Scope, Start, Stream, StreamStatus, Unit, UploadContext,
        metadata::{self, Metadata},
    },
};
use std::{
    collections::VecDeque, future::Future, num::NonZeroUsize, pin::Pin, sync::Arc, time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, sleep_until},
};

pub mod reconcile;

type Pending<'a> = Pin<Box<dyn Future<Output = Result<Receipt, Error>> + Send + 'a>>;

pub struct Config {
    pub accepted: PreparedSession,
    pub scope: Scope,
    /// The same monotonic/wall-clock observation used for admission.
    pub observed: Instant,
    pub wall_millis: i64,
    pub automatic_limit: usize,
    pub capacity: NonZeroUsize,
    pub budget: MemoryBudget,
}

/// Metadata stays charged while its consumer owns it. This is neither a
/// durable inventory nor permission to import an unknown unit.
pub struct Messages {
    pub values: Vec<Metadata>,
    _charge: Reservation,
}

pub enum Event {
    Progress,
    Metadata(Messages),
    /// The remote durable Ack AND local final write have both completed.
    Uploaded {
        unit: Unit,
        batch: [u8; 8],
    },
    /// The local Store committed; the owner still retains/writes its Ack.
    Downloaded(Unit),
    /// No payload is admitted. Human waiting happens outside this stream.
    Deferred(Start),
    /// The remote receiver deferred this exact upload, without admitting bytes.
    UploadDeferred(Start),
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub transfer: StreamStatus,
    pub committing: bool,
    pub awaiting_ack: bool,
    pub queued_controls: usize,
    pub writing: bool,
}

enum Writing {
    Control { policy: bool },
    Metadata,
    Receipt,
    Upload,
}
struct Commit<'a> {
    unit: Unit,
    batch: [u8; 8],
    deadline: u64,
    work: Pending<'a>,
}
struct Upload<'a> {
    unit: Unit,
    batch: [u8; 8],
    deadline: u64,
    start: Option<Record>,
    advertised: Start,
    accepted: bool,
    payload_started: bool,
    written: bool,
    acked: bool,
    retained: Arc<dyn Send + Sync + 'a>,
}

/// One bounded transfer in each direction. next() may be canceled and called
/// again: original I/O, Store result interest and deadlines stay in this owner.
/// Drop/cancel discards network interest, never an already accepted Store job.
pub struct Client<'a, R, W> {
    io: Duplex<'a, R, W>,
    stream: Stream,
    scope: Scope,
    observed: Instant,
    budget: MemoryBudget,
    capacity: usize,
    controls: VecDeque<(Record, bool, u64)>,
    policy_pending: usize,
    writing: Option<Writing>,
    writing_deadline: Option<u64>,
    next_start: Option<records::Received>,
    commit: Option<Commit<'a>>,
    receipt: Option<(Receipt, u64)>,
    upload: Option<Upload<'a>>,
    batch: u64,
    receive: Box<dyn FnMut(Completed) -> Result<Pending<'a>, Error> + Send + 'a>,
    authorize: Box<dyn FnMut(Unit) -> Result<(), Refusal> + Send + 'a>,
    permission: Box<dyn FnMut(&Start) -> Decision + Send + 'a>,
}

impl<'a, R, W> Client<'a, R, W>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    /// permission is a synchronous lookup of an already-valid exact one-shot
    /// grant. Without one it returns Defer immediately; never await a human.
    /// The host invalidates such grants on account/path/policy changes.
    /// receive must synchronously admit the complete transfer to its retained
    /// Store owner before returning Ok(reply). A merely created async enqueue
    /// future is not an ownership handoff. StoreWorkerClient::submit_cloud
    /// implements this boundary; its old receive_cloud async method does not.
    pub fn new<H, F, A, D>(
        reader: R,
        writer: W,
        config: Config,
        mut receive: H,
        authorize: A,
        permission: D,
    ) -> Result<Self, Error>
    where
        H: FnMut(Completed) -> Result<F, Error> + Send + 'a,
        F: Future<Output = Result<Receipt, Error>> + Send + 'a,
        A: FnMut(Unit) -> Result<(), Refusal> + Send + 'a,
        D: FnMut(&Start) -> Decision + Send + 'a,
    {
        let mut stream = Stream::new(
            &config.accepted,
            config.scope,
            config.wall_millis,
            0,
            config.automatic_limit,
            config.budget.clone(),
        )?;
        let policy = stream.initial_policy(0)?;
        let deadline = stream.next_deadline();
        let io = Duplex::new(
            reader,
            writer,
            config.observed + Duration::from_millis(stream.expires_at()),
            config.budget.clone(),
        );
        Ok(Self {
            io,
            stream,
            scope: config.scope,
            observed: config.observed,
            budget: config.budget,
            capacity: config.capacity.get(),
            controls: VecDeque::from([(Record::Control(policy), true, deadline)]),
            policy_pending: 1,
            writing: None,
            writing_deadline: None,
            next_start: None,
            commit: None,
            receipt: None,
            upload: None,
            batch: 0,
            receive: Box::new(move |complete| {
                receive(complete).map(|reply| Box::pin(reply) as Pending<'a>)
            }),
            authorize: Box::new(authorize),
            permission: Box::new(permission),
        })
    }

    pub fn status(&self) -> Status {
        Status {
            transfer: self.stream.status(),
            committing: self.commit.is_some()
                || self.receipt.is_some()
                || matches!(self.writing, Some(Writing::Receipt)),
            awaiting_ack: self.upload.as_ref().is_some_and(|u| u.written && !u.acked),
            queued_controls: self.controls.len(),
            writing: self.writing.is_some(),
        }
    }
    fn now(&self) -> u64 {
        self.observed
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    fn instant(&self, millis: u64) -> Instant {
        self.observed + Duration::from_millis(millis)
    }
    fn deadline(&self) -> u64 {
        let mut deadline = self.stream.next_deadline();
        for value in [
            self.writing_deadline,
            self.commit.as_ref().map(|c| c.deadline),
            self.receipt.as_ref().map(|r| r.1),
            self.upload.as_ref().map(|u| u.deadline),
            self.controls.front().map(|c| c.2),
        ]
        .into_iter()
        .flatten()
        {
            deadline = deadline.min(value);
        }
        if let Some(start) = &self.next_start {
            let first = start.first_byte.duration_since(self.observed).as_millis() as u64;
            deadline = deadline.min(first.saturating_add(transfer::CONTROL_WAIT_MS));
        }
        deadline
    }
    fn check(&mut self) -> Result<(), Error> {
        self.stream.check(self.now())?;
        if self.now() >= self.deadline() {
            return Err(Error::Timeout);
        }
        Ok(())
    }
    pub fn cancel(&mut self, error: Error) -> Error {
        self.io.cancel(error);
        self.controls.clear();
        self.next_start = None;
        self.commit = None;
        self.receipt = None;
        self.upload = None;
        self.writing = None;
        self.writing_deadline = None;
        self.policy_pending = 0;
        self.stream.cancel(error)
    }
    fn available_writer(&self) -> bool {
        self.io.can_send()
            && self.controls.is_empty()
            && self.receipt.is_none()
            && self.policy_pending == 0
            && self.stream.status().peer_generation.is_some()
    }

    /// No unbounded request queue: a busy owner refuses submission unchanged.
    /// Reconciliation retains and retries its bounded pending unit after progress.
    pub fn join(&mut self, unit: Unit, version: Vec<u8>) -> Result<(), Error> {
        self.join_mode(unit, version, false)
    }
    fn join_mode(&mut self, unit: Unit, version: Vec<u8>, live: bool) -> Result<(), Error> {
        self.check()?;
        if !self.available_writer() {
            return Err(Error::Unavailable);
        }
        (self.authorize)(unit).map_err(Error::Authority)?;
        if version.len() > transfer::MAX_PAYLOAD {
            tracing::warn!(
                ?unit,
                direction = "upload",
                check = "client_join_version",
                actual_bytes = version.len(),
                limit_bytes = transfer::MAX_PAYLOAD,
                "cloud sync: size limit rejected version metadata"
            );
            return Err(Error::TooLarge);
        }
        let charge = self.budget.reserve_record(version.len())?;
        let record = metadata::join_request_mode(self.scope, unit, version, live)?;
        let deadline = self.stream.expires_at();
        self.io
            .send((record, charge), |v| &v.0, self.instant(deadline))?;
        self.writing = Some(Writing::Metadata);
        self.writing_deadline = Some(deadline);
        Ok(())
    }

    fn leave(&mut self, unit: Unit) -> Result<(), Error> {
        self.check()?;
        if !self.available_writer() {
            return Err(Error::Unavailable);
        }
        let message = loro_protocol::ProtocolMessage::Leave {
            crdt: loro_protocol::CrdtType::Loro,
            room_id: self.scope.room(unit)?,
        };
        let record = Record::Peer(koru_replication::protocol::PeerFrame::Loro {
            workspace_id: self.scope.eligibility_workspace,
            message: loro_protocol::encoding::encode(&message).map_err(|_| Error::Invalid)?,
        });
        let deadline = self.stream.expires_at();
        self.io.send(record, |v| v, self.instant(deadline))?;
        self.writing = Some(Writing::Metadata);
        self.writing_deadline = Some(deadline);
        Ok(())
    }

    /// Shared inventory leaves, sent as one bounded metadata request. A matching
    /// reply avoids opening a Loro stream for each unchanged document.
    fn compare(
        &mut self,
        documents: Vec<koru_replication::protocol::CloudDocumentDigest>,
    ) -> Result<(), Error> {
        self.check()?;
        metadata::compare_scope(self.scope, self.scope.eligibility_workspace)?;
        if !self.available_writer()
            || documents.is_empty()
            || documents.len() > koru_replication::protocol::CLOUD_COMPARE_LIMIT
        {
            return Err(Error::Invalid);
        }
        for document in &documents {
            (self.authorize)(Unit::Document {
                workspace_id: koru_device::cloud::CloudId::from_bytes(
                    self.scope.eligibility_workspace.as_uuid().into_bytes(),
                ),
                document_id: koru_device::cloud::CloudId::from_bytes(
                    document.document_id.as_uuid().into_bytes(),
                ),
            })
            .map_err(Error::Authority)?;
        }
        let charge = self.budget.reserve_record(documents.len() * 256)?;
        let record = Record::Peer(koru_replication::protocol::PeerFrame::CloudCompare {
            workspace_id: self.scope.eligibility_workspace,
            documents,
        });
        let deadline = self.stream.expires_at();
        self.io
            .send((record, charge), |v| &v.0, self.instant(deadline))?;
        self.writing = Some(Writing::Metadata);
        self.writing_deadline = Some(deadline);
        Ok(())
    }

    /// The original payload moves into the carrier. Any export/ownership guard
    /// stays alive through writes and the exact remote Ack, including crossing
    /// Ack-before-local-flush. The host must bound/charge export allocation too.
    pub fn upload<T: Send + Sync + 'a>(
        &mut self,
        unit: Unit,
        bytes: Vec<u8>,
        retained: T,
    ) -> Result<(), Error> {
        self.upload_with(unit, bytes, retained, UploadContext::default())
    }

    /// [`Client::upload`] with what the Store said about it: the start carries
    /// `context.claim` (TECHNICAL §5.4 item 4). Only an app connection's
    /// session may send one; on any other the upload is refused as
    /// `sync_transfer_invalid` before anything is written.
    pub fn upload_with<T: Send + Sync + 'a>(
        &mut self,
        unit: Unit,
        bytes: Vec<u8>,
        retained: T,
        context: UploadContext,
    ) -> Result<(), Error> {
        self.check()?;
        if !self.available_writer() || self.upload.is_some() {
            return Err(Error::Unavailable);
        }
        let now = self.now();
        self.batch = self.batch.checked_add(1).ok_or(Error::Unavailable)?;
        let batch = self.batch.to_be_bytes();
        let (start, admission) =
            self.stream
                .begin_upload(unit, batch, bytes, context, now, now, &mut self.authorize)?;
        self.upload = Some(Upload {
            unit,
            batch,
            deadline: self.stream.outgoing_deadline().ok_or(Error::Invalid)?,
            advertised: match &start {
                Control::TransferStart(start) => start.clone(),
                _ => return Err(Error::Invalid),
            },
            start: Some(Record::Control(start)),
            accepted: admission == Admission::Automatic,
            payload_started: false,
            written: false,
            acked: false,
            retained: Arc::new(retained),
        });
        Ok(())
    }

    /// The caller has already invalidated one-shot permissions. An idle
    /// direction publishes the next generation before reading another start.
    /// An unfinished inbound transfer closes with PolicyChanged. The completed
    /// Store job, if any, retains its existing non-retroactive handoff.
    pub fn change_policy(&mut self, limit: usize) -> Result<(), Error> {
        let result = (|| {
            self.check()?;
            if self.controls.len() >= self.capacity {
                return Err(Error::Unavailable);
            }
            let policy = self.stream.change_policy(limit, self.now())?;
            self.controls.push_back((
                Record::Control(policy),
                true,
                self.now()
                    .saturating_add(transfer::CONTROL_WAIT_MS)
                    .min(self.stream.expires_at()),
            ));
            self.policy_pending += 1;
            Ok(())
        })();
        result.map_err(|e| self.cancel(e))
    }

    fn advance(&mut self) -> Result<(), Error> {
        self.check()?;
        if self.stream.status().incoming_complete && !self.status().committing {
            let deadline = self.stream.incoming_deadline().ok_or(Error::Invalid)?;
            let complete = self
                .stream
                .handoff(self.now(), &mut self.authorize, |v| v)?;
            self.commit = Some(Commit {
                unit: complete.unit,
                batch: complete.batch_id,
                deadline,
                work: (self.receive)(complete)?,
            });
        }
        if !self.io.can_send() {
            return Ok(());
        }
        if let Some((record, policy, deadline)) = self.controls.pop_front() {
            self.io.send(record, |v| v, self.instant(deadline))?;
            self.writing = Some(Writing::Control { policy });
            self.writing_deadline = Some(deadline);
        } else if let Some((receipt, deadline)) = self.receipt.take() {
            (self.authorize)(receipt.unit()).map_err(Error::Authority)?;
            self.io
                .send(receipt, Receipt::acknowledgement, self.instant(deadline))?;
            self.writing = Some(Writing::Receipt);
            self.writing_deadline = Some(deadline);
        } else if let Some(upload) = &mut self.upload {
            if upload.written {
                return Ok(());
            }
            (self.authorize)(upload.unit).map_err(Error::Authority)?;
            let deadline = self.observed + Duration::from_millis(upload.deadline);
            if let Some(start) = upload.start.take() {
                self.io
                    .send((start, upload.retained.clone()), |v| &v.0, deadline)?;
            } else if upload.accepted {
                let now = self
                    .observed
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX);
                if let Some(record) = self.stream.next_outgoing(now, &mut self.authorize)? {
                    upload.payload_started = true;
                    self.io.send(
                        (record, upload.retained.clone()),
                        |v| v.0.record(),
                        deadline,
                    )?;
                } else {
                    self.stream.outbound_written(now)?;
                    upload.written = true;
                    return Ok(());
                }
            } else {
                return Ok(());
            }
            self.writing = Some(Writing::Upload);
            self.writing_deadline = Some(upload.deadline);
        }
        Ok(())
    }

    fn received(&mut self, received: records::Received) -> Result<Event, Error> {
        let (first, record, charge) = received.into_parts();
        let now = self.now();
        let first = first
            .checked_duration_since(self.observed)
            .ok_or(Error::Invalid)?
            .as_millis()
            .try_into()
            .map_err(|_| Error::Invalid)?;
        match record {
            Record::Control(control) => {
                let start = match &control {
                    Control::TransferStart(start) => Some(start.clone()),
                    _ => None,
                };
                if start.is_some() && self.status().committing {
                    return Err(Error::Invalid);
                }
                if let Control::TransferPolicy {
                    policy_generation, ..
                } = &control
                    && self.upload.is_some()
                    && self
                        .stream
                        .status()
                        .peer_generation
                        .is_some_and(|g| *policy_generation > g)
                {
                    return Err(Error::PolicyChanged);
                }
                let result =
                    self.stream
                        .receive_control(control, first, now, &mut self.authorize)?;
                match result {
                    Admission::AwaitDecision => {
                        if self.controls.len() >= self.capacity {
                            return Err(Error::Unavailable);
                        }
                        let start = start.ok_or(Error::Invalid)?;
                        let decision = (self.permission)(&start);
                        let record =
                            self.stream
                                .decide(&start, decision, now, &mut self.authorize)?;
                        self.controls.push_back((
                            Record::Control(record),
                            false,
                            first
                                .saturating_add(transfer::CONTROL_WAIT_MS)
                                .min(self.stream.expires_at()),
                        ));
                        if decision == Decision::Defer {
                            return Ok(Event::Deferred(start));
                        }
                    }
                    Admission::Accepted => {
                        self.upload.as_mut().ok_or(Error::Invalid)?.accepted = true;
                    }
                    Admission::Deferred => {
                        let upload = self.upload.take().ok_or(Error::Invalid)?;
                        return Ok(Event::UploadDeferred(upload.advertised));
                    }
                    _ => {}
                }
                Ok(Event::Progress)
            }
            Record::Peer(frame) => {
                let (_, metadata) =
                    self.stream
                        .receive_peer_owned(frame, now, &mut self.authorize)?;
                let Some(frame) = metadata else {
                    return Ok(Event::Progress);
                };
                let mut values = metadata::decode(self.scope, frame, self.capacity)?;
                for message in &values {
                    if let Metadata::Ack { unit, batch, ok } = message {
                        (self.authorize)(*unit).map_err(Error::Authority)?;
                        let upload = self.upload.as_mut().ok_or(Error::Invalid)?;
                        if !*ok
                            || upload.unit != *unit
                            || upload.batch != *batch
                            || upload.acked
                            || !upload.accepted
                            || !upload.payload_started
                        {
                            return Err(Error::Invalid);
                        }
                        upload.acked = true;
                    }
                }
                values.retain(|m| !matches!(m, Metadata::Ack { .. } | Metadata::Ignore));
                if values.is_empty() {
                    Ok(Event::Progress)
                } else {
                    Ok(Event::Metadata(Messages {
                        values,
                        _charge: charge,
                    }))
                }
            }
        }
    }

    pub async fn next(&mut self) -> Result<Event, Error> {
        let deadline = self.instant(self.deadline());
        let result = self.next_inner().await;
        if let Err(error) = result.as_ref() {
            tracing::debug!(
                error = error.code(),
                status = ?self.status(),
                "cloud carrier: the dataset stream ended"
            );
            // The outer session::with_fence still surrounds this write. Do not
            // write through lost authority, a partial frame or an expired timer.
            if !matches!(error, Error::Authority(_))
                && self.io.can_send()
                && Instant::now() < deadline
                && self
                    .io
                    .send(Record::Peer(error.frame()), |v| v, deadline)
                    .is_ok()
            {
                // No input is dispatched after terminal refusal. This write
                // retains its own framing and original deadline.
                let _ = self.io.next_write().await;
            }
        }
        result.map_err(|e| self.cancel(e))
    }
    async fn next_inner(&mut self) -> Result<Event, Error> {
        self.advance()?;
        if self.upload.as_ref().is_some_and(|u| u.written && u.acked) {
            let upload = self.upload.take().expect("completed upload");
            return Ok(Event::Uploaded {
                unit: upload.unit,
                batch: upload.batch,
            });
        }
        enum Ready {
            Io(Result<records::Event, Error>),
            Commit(Result<Receipt, Error>),
        }
        let deadline = self.instant(self.deadline());
        let pause_read = self.next_start.is_some() || self.policy_pending > 0;
        let ready = tokio::select! {
            biased;
            _ = sleep_until(deadline) => { self.check()?; return Err(Error::Timeout); }
            ready = async {
                tokio::select! {
                    event = async {
                        if pause_read { self.io.next_write().await } else { self.io.next().await }
                    } => Ready::Io(event),
                    result = async { self.commit.as_mut().expect("guarded commit").work.as_mut().await },
                        if self.commit.is_some() => Ready::Commit(result),
                }
            } => ready,
        };
        self.check()?;
        match ready {
            Ready::Commit(result) => {
                let commit = self.commit.take().expect("finished commit");
                let receipt = result?;
                if receipt.unit() != commit.unit {
                    return Err(Error::Invalid);
                }
                let Record::Peer(frame) = receipt.acknowledgement() else {
                    return Err(Error::Invalid);
                };
                if !matches!(metadata::decode(self.scope, frame.clone(), 1)?.as_slice(),
                    [Metadata::Ack { unit, batch, ok: true }]
                    if *unit == commit.unit && *batch == commit.batch)
                {
                    return Err(Error::Invalid);
                }
                self.receipt = Some((receipt, commit.deadline));
                Ok(Event::Downloaded(commit.unit))
            }
            Ready::Io(Err(error)) => Err(error),
            Ready::Io(Ok(records::Event::Written)) => {
                if matches!(self.writing, Some(Writing::Control { policy: true })) {
                    self.policy_pending -= 1;
                }
                self.writing = None;
                self.writing_deadline = None;
                if self.policy_pending == 0
                    && let Some(start) = self.next_start.take()
                {
                    return self.received(start);
                }
                Ok(Event::Progress)
            }
            Ready::Io(Ok(records::Event::Received(value))) => {
                if matches!(self.writing, Some(Writing::Receipt))
                    && matches!(value.record(), Record::Control(Control::TransferStart(_)))
                {
                    self.next_start = Some(value);
                    Ok(Event::Progress)
                } else {
                    self.received(value)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
