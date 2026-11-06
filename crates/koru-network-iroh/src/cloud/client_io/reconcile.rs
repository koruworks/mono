// SPDX-License-Identifier: MPL-2.0

//! Retained, one-unit reconciliation over the existing carrier and Store jobs.
//! Catalog traversal supplies admitted units; this does not invent discovery,
//! authorize a scope, persist progress, or turn a completed pass into coverage.
use super::*;
use koru_replication::cloud::client_store::{Export, ExportRequest};
use koru_store_core::errors::Code;
use loro::VersionVector;

#[cfg(test)]
mod tests;

/// What a Store port answered.
///
/// [`Answer::Refused`] is a statement about **this unit only**, and it exists
/// because an `Err` cannot say that. Every `Err` out of a port is returned
/// through [`Reconciler::next`], which cancels the carrier — and
/// `transfer::state::cancel` latches its reason, so `check` refuses every
/// later unit for the rest of the session. That latch is the amplifier PLAN19
/// removes: one unit a local Store job would not serve ended the pass for
/// every other unit beside it. A new carrier code could not carry the
/// distinction either, because `cloud-sync-admission` §3 freezes the
/// `PeerFrame::Error` codes a refusal may use: the five carrier codes, and
/// the two its app connection amendment adds for connections' uploads only.
pub enum Answer<T> {
    Ready(T),
    /// The Store would not serve this unit, and the session is still sound.
    /// The `Code` is the Store's own, kept whole: it is the only place the
    /// reason exists on either end, and collapsing it into one transport word
    /// is what sent two investigations into the wrong crate tonight.
    Refused(Code),
}

type Read<'a> = Pin<Box<dyn Future<Output = Result<Answer<VersionVector>, Error>> + Send + 'a>>;
type ExportJob<'a> =
    Pin<Box<dyn Future<Output = Result<Answer<Option<Export>>, Error>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Reading,
    Joining,
    AwaitingJoin,
    Exporting,
    Sending,
    AwaitingUpload,
    Verifying,
    AwaitingDownload,
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub unit: Option<Unit>,
    pub phase: Phase,
    pub transport: super::Status,
}

pub enum Event {
    Progress,
    /// Authenticated, content-free hint: the host should refresh scoped inventory.
    RemoteHint,
    Downloaded(Unit),
    Uploaded {
        unit: Unit,
        batch: [u8; 8],
    },
    Deferred {
        start: Start,
        upload: bool,
    },
    /// A Store job refused this unit and the session carried on.
    ///
    /// Deliberately **not** `Deferred`: that one is the carrier holding a
    /// direction because a policy or a decision said so, and it is the remote's
    /// word. This one is local, it names a `Code` no frame can carry, and a
    /// unit that keeps earning it is a unit that will never sync — which the
    /// host has to bound rather than repeat forever.
    ///
    /// Whether the unit is finished is read from [`Reconciler::status`]: a
    /// refusal at the version read leaves no pass behind, while one at the
    /// export holds the upload and lets the unit settle.
    Refused {
        unit: Unit,
        code: Code,
    },
    /// This captured exchange is finished, possibly with held directions.
    /// This is NOT DocumentCoverageV1 or a persistent/whole-catalog synced flag.
    Settled {
        unit: Unit,
        upload_deferred: bool,
        download_deferred: bool,
        /// Local state moved beyond the remote Join plus acknowledged upload.
        /// The host must schedule another pass, not claim that state arrived.
        local_changed: bool,
    },
}

enum Step<'a> {
    Read(Read<'a>),
    Join(VersionVector),
    AwaitJoin,
    Export(ExportJob<'a>),
    Send(Export),
    Upload,
    Verify(Read<'a>),
    Download,
}
struct Remote {
    version: VersionVector,
    // The decoded message allocation stays charged, including when an accepted
    // Store export job outlives a canceled reconciliation owner.
    _messages: Messages,
}
struct Pass<'a> {
    unit: Unit,
    step: Step<'a>,
    remote: Option<Arc<Remote>>,
    upload_deferred: bool,
    download_deferred: bool,
    /// A committed download raced the retained version read. Finish that
    /// original Store job before admitting another read; never lose its wake.
    verify_again: bool,
    uploaded_version: Option<VersionVector>,
    upload_acked: bool,
}

pub struct Reconciler<'a, R, W> {
    client: Client<'a, R, W>,
    pass: Option<Pass<'a>>,
    rooms: std::collections::HashSet<Unit>,
    leaves: std::collections::HashSet<Unit>,
    read: Box<dyn FnMut(Unit) -> Result<Read<'a>, Error> + Send + 'a>,
    export: Box<dyn FnMut(ExportRequest) -> Result<ExportJob<'a>, Error> + Send + 'a>,
}

impl<'a, R, W> Reconciler<'a, R, W>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    /// Both ports synchronously admit retained Store jobs, then return their
    /// reply futures. Use submit_store(client_store::version) and
    /// submit_cloud_export. Do not perform Store work on the transport executor.
    /// The entire Iroh lifetime still belongs inside session::with_fence.
    pub fn new<V, VF, E, EF>(client: Client<'a, R, W>, mut read: V, mut export: E) -> Self
    where
        V: FnMut(Unit) -> Result<VF, Error> + Send + 'a,
        VF: Future<Output = Result<Answer<VersionVector>, Error>> + Send + 'a,
        E: FnMut(ExportRequest) -> Result<EF, Error> + Send + 'a,
        EF: Future<Output = Result<Answer<Option<Export>>, Error>> + Send + 'a,
    {
        Self {
            client,
            pass: None,
            rooms: Default::default(),
            leaves: Default::default(),
            read: Box::new(move |unit| read(unit).map(|f| Box::pin(f) as Read<'a>)),
            export: Box::new(move |request| export(request).map(|f| Box::pin(f) as ExportJob<'a>)),
        }
    }

    /// Exactly one unit, no hidden unbounded backlog. Inventory/commit/resume
    /// scheduling is the host's bounded, debounced owner, not a document timer.
    pub fn start(&mut self, unit: Unit) -> Result<(), Error> {
        self.client.check()?;
        if self.pass.is_some() {
            return Err(Error::Unavailable);
        }
        self.client.scope.object_unit(unit)?;
        (self.client.authorize)(unit).map_err(Error::Authority)?;
        let work = (self.read)(unit)?;
        self.pass = Some(Pass {
            unit,
            step: Step::Read(work),
            remote: None,
            upload_deferred: false,
            download_deferred: false,
            verify_again: false,
            uploaded_version: None,
            upload_acked: false,
        });
        Ok(())
    }

    pub fn status(&self) -> Status {
        Status {
            unit: self.pass.as_ref().map(|p| p.unit),
            phase: self.pass.as_ref().map_or(Phase::Idle, |p| match p.step {
                Step::Read(_) => Phase::Reading,
                Step::Join(_) => Phase::Joining,
                Step::AwaitJoin => Phase::AwaitingJoin,
                Step::Export(_) => Phase::Exporting,
                Step::Send(_) => Phase::Sending,
                Step::Upload => Phase::AwaitingUpload,
                Step::Verify(_) => Phase::Verifying,
                Step::Download => Phase::AwaitingDownload,
            }),
            transport: self.client.status(),
        }
    }

    /// Bounded viewer interest, scoped by the host's authoritative inventory.
    /// This changes scheduling and requests push; it grants no document access.
    pub fn rooms(&mut self, units: impl IntoIterator<Item = Unit>) -> Result<(), Error> {
        let rooms = units.into_iter().collect::<std::collections::HashSet<_>>();
        if rooms.len() > metadata::MAX_LIVE_ROOMS {
            return Err(Error::Unavailable);
        }
        self.leaves.extend(self.rooms.difference(&rooms).copied());
        self.leaves.retain(|unit| !rooms.contains(unit));
        self.rooms = rooms;
        Ok(())
    }

    /// Reuse the peer inventory's revision comparison before reconciling units.
    /// The remote answer is a sampled equality, not permission or a receipt.
    /// Callers must compare local positions again before claiming completion.
    pub async fn compare_documents(
        &mut self,
        documents: Vec<koru_replication::protocol::CloudDocumentDigest>,
    ) -> Result<(std::collections::HashSet<koru_store_core::DocumentId>, bool), Error> {
        if self.pass.is_some() {
            return Err(Error::Invalid);
        }
        let expected = documents
            .iter()
            .map(|d| d.document_id)
            .collect::<std::collections::HashSet<_>>();
        if expected.len() != documents.len() {
            return Err(Error::Invalid);
        }
        let mut pending = Some(documents);
        let mut hinted = false;
        loop {
            if self.client.available_writer()
                && let Some(documents) = pending.take()
            {
                self.client.compare(documents)?;
            }
            match self.client.next().await? {
                super::Event::Progress => {}
                super::Event::Metadata(messages) => {
                    let mut answer = None;
                    for message in messages.values {
                        match message {
                            Metadata::SyncRequest => hinted = true,
                            Metadata::Compared(matching)
                                if pending.is_none() && answer.is_none() =>
                            {
                                if matching.iter().any(|id| !expected.contains(id)) {
                                    return Err(Error::Invalid);
                                }
                                answer = Some(matching.into_iter().collect());
                            }
                            _ => return Err(Error::Invalid),
                        }
                    }
                    if let Some(matching) = answer {
                        return Ok((matching, hinted));
                    }
                }
                super::Event::Downloaded(_) | super::Event::Deferred(_) => {}
                _ => return Err(Error::Invalid),
            }
        }
    }

    pub fn change_policy(&mut self, limit: usize) -> Result<(), Error> {
        self.client.change_policy(limit).map_err(|e| self.cancel(e))
    }
    pub fn cancel(&mut self, error: Error) -> Error {
        self.pass = None;
        self.client.cancel(error)
    }

    fn verify(&mut self) -> Result<(), Error> {
        let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
        pass.verify_again = false;
        pass.step = Step::Verify((self.read)(pass.unit)?);
        Ok(())
    }

    fn advance(&mut self) -> Result<(), Error> {
        self.client.check()?;
        if self.client.available_writer()
            && let Some(unit) = self.leaves.iter().next().copied()
        {
            self.client.leave(unit)?;
            self.leaves.remove(&unit);
            return Ok(());
        }
        let Some(pass) = &mut self.pass else {
            return Ok(());
        };
        if self.client.available_writer() {
            match &pass.step {
                Step::Join(version) => {
                    self.client.join_mode(
                        pass.unit,
                        version.encode(),
                        self.rooms.contains(&pass.unit),
                    )?;
                    pass.step = Step::AwaitJoin;
                }
                Step::Send(_) if self.client.upload.is_none() => {
                    let Step::Send(export) = std::mem::replace(&mut pass.step, Step::Upload) else {
                        unreachable!()
                    };
                    let (bytes, guard) = export.into_payload();
                    self.client.upload(pass.unit, bytes, guard)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn metadata(&mut self, messages: Messages) -> Result<Event, Error> {
        let mut joined = None;
        let mut hinted = false;
        for message in &messages.values {
            match message {
                Metadata::SyncRequest => hinted = true,
                Metadata::Joined {
                    unit,
                    version,
                    live,
                } if joined.is_none() => {
                    if self.rooms.contains(unit) {
                        tracing::debug!(?unit, supported = live, "cloud room: worker negotiation");
                    }
                    joined = Some((
                        *unit,
                        VersionVector::decode(version).map_err(|_| Error::Invalid)?,
                    ));
                }
                _ => return Err(Error::Invalid),
            }
        }
        if let Some((unit, version)) = joined {
            let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
            if pass.unit != unit || !matches!(pass.step, Step::AwaitJoin) {
                return Err(Error::Invalid);
            }
            let remote = Arc::new(Remote {
                version,
                _messages: messages,
            });
            let request = ExportRequest::new(
                self.client.scope,
                unit,
                remote.version.clone(),
                self.client.budget.clone(),
            )?
            .retain(remote.clone());
            pass.step = Step::Export((self.export)(request)?);
            pass.remote = Some(remote);
        }
        Ok(if hinted {
            Event::RemoteHint
        } else {
            Event::Progress
        })
    }

    fn transport_event(&mut self, event: super::Event) -> Result<Event, Error> {
        Ok(match event {
            super::Event::Progress => Event::Progress,
            super::Event::Metadata(messages) => return self.metadata(messages),
            super::Event::Downloaded(unit) => {
                if let Some(pass) = &mut self.pass
                    && pass.unit == unit
                    && matches!(pass.step, Step::Verify(_))
                {
                    pass.verify_again = true;
                }
                if self
                    .pass
                    .as_ref()
                    .is_some_and(|p| p.unit == unit && matches!(p.step, Step::Download))
                {
                    self.verify()?;
                }
                Event::Downloaded(unit)
            }
            super::Event::Uploaded { unit, batch } => {
                let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
                if pass.unit != unit || !matches!(pass.step, Step::Upload) {
                    return Err(Error::Invalid);
                }
                pass.upload_acked = true;
                self.verify()?;
                Event::Uploaded { unit, batch }
            }
            super::Event::Deferred(start) => {
                if let Some(pass) = &mut self.pass
                    && pass.unit == start.unit
                {
                    pass.download_deferred = true;
                    if matches!(pass.step, Step::Download) {
                        self.verify()?;
                    }
                }
                Event::Deferred {
                    start,
                    upload: false,
                }
            }
            super::Event::UploadDeferred(start) => {
                let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
                if pass.unit != start.unit || !matches!(pass.step, Step::Upload) {
                    return Err(Error::Invalid);
                }
                pass.upload_deferred = true;
                self.verify()?;
                Event::Deferred {
                    start,
                    upload: true,
                }
            }
        })
    }

    pub async fn next(&mut self) -> Result<Event, Error> {
        let result = self.next_inner().await;
        result.map_err(|e| self.cancel(e))
    }

    async fn next_inner(&mut self) -> Result<Event, Error> {
        self.advance()?;
        enum Ready {
            Transport(Result<super::Event, Error>),
            Read(Result<Answer<VersionVector>, Error>),
            Export(Result<Answer<Option<Export>>, Error>),
        }
        let pass = &mut self.pass;
        let ready = tokio::select! {
            event = self.client.next() => Ready::Transport(event),
            result = async {
                match &mut pass.as_mut().expect("guarded pass").step {
                    Step::Read(f) | Step::Verify(f) => Ready::Read(f.as_mut().await),
                    Step::Export(f) => Ready::Export(f.as_mut().await),
                    _ => std::future::pending().await,
                }
            }, if pass.is_some() => result,
        };
        // A ready Store result cannot win over expired transport/session fences.
        self.client.check()?;
        match ready {
            Ready::Transport(event) => self.transport_event(event?),
            Ready::Export(result) => {
                let export = match result? {
                    Answer::Ready(export) => export,
                    // The join is already answered, so the unit cannot simply
                    // be dropped here: the peer would be left holding a
                    // `Joined` nothing ever replies to. It takes the path
                    // `Ok(None)` already takes — hold the upload and verify —
                    // which is a direction deferred, exactly the outcome the
                    // carrier's own `UploadDeferred` produces.
                    Answer::Refused(code) => {
                        let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
                        let unit = pass.unit;
                        pass.upload_deferred = true;
                        self.verify()?;
                        return Ok(Event::Refused { unit, code });
                    }
                };
                if let Some(export) = export {
                    let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
                    if export.unit() != pass.unit {
                        return Err(Error::Invalid);
                    }
                    pass.uploaded_version = Some(export.version().clone());
                    pass.step = Step::Send(export);
                } else {
                    self.verify()?;
                }
                Ok(Event::Progress)
            }
            Ready::Read(result) => {
                let version = match result? {
                    Answer::Ready(version) => version,
                    // Safe at both of the steps a read serves. `Read` is the
                    // unit's first act and nothing for it has reached the wire
                    // yet; `Verify` runs after the exchange is over, so there
                    // is nothing left for the peer to wait on. Either way the
                    // pass is dropped and the carrier is untouched, which is
                    // what lets the next unit start.
                    Answer::Refused(code) => {
                        let pass = self.pass.take().ok_or(Error::Invalid)?;
                        return Ok(Event::Refused {
                            unit: pass.unit,
                            code,
                        });
                    }
                };
                let pass = self.pass.as_mut().ok_or(Error::Invalid)?;
                match pass.step {
                    Step::Read(_) => pass.step = Step::Join(version),
                    Step::Verify(_) => {
                        if pass.verify_again {
                            self.verify()?;
                            return Ok(Event::Progress);
                        }
                        let remote = pass.remote.as_ref().ok_or(Error::Invalid)?;
                        if pass.download_deferred
                            || !koru_replication::reconcile::missing(&version, &remote.version)
                                .download
                        {
                            let mut confirmed = remote.version.clone();
                            if pass.upload_acked
                                && let Some(uploaded) = &pass.uploaded_version
                            {
                                confirmed.merge(uploaded);
                            }
                            let local_changed =
                                !pass.upload_deferred && !confirmed.includes_vv(&version);
                            let pass = self.pass.take().expect("finished pass");
                            return Ok(Event::Settled {
                                unit: pass.unit,
                                upload_deferred: pass.upload_deferred,
                                download_deferred: pass.download_deferred,
                                local_changed,
                            });
                        }
                        pass.step = Step::Download;
                    }
                    _ => return Err(Error::Invalid),
                }
                Ok(Event::Progress)
            }
        }
    }
}
