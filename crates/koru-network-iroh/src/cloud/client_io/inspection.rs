// SPDX-License-Identifier: MPL-2.0

//! Authenticated metadata and detached content reads. No Store job, live
//! subscription, upload, durable acknowledgement, or adoption is admitted.
//! The caller still owns the complete
//! session::with_fence lifetime and rechecks account/lifecycle before use.
use super::*;

/// Original JoinResponse bytes, bound to the admitted principal and exact unit.
/// This proves a sampled version only, never permission to edit or a receipt.
pub struct ObservedVersion {
    scope: Scope,
    unit: Unit,
    version: Vec<u8>,
    _charge: Reservation,
}
impl std::fmt::Debug for ObservedVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservedVersion(<redacted>)")
    }
}
impl ObservedVersion {
    pub fn head(&self) -> [u8; 32] {
        koru_store_core::sync_digest::revision(&self.version)
    }
    /// Compare the inventory digest against original Cloud bytes, and compare
    /// the cold local state semantically. Re-encoding is never head evidence.
    pub fn matches(&self, scope: Scope, unit: Unit, head: [u8; 32], local_version: &[u8]) -> bool {
        self.scope.principal == scope.principal
            && self.scope.profile == scope.profile
            && self.scope.eligibility_workspace == scope.eligibility_workspace
            && self.scope.manifest == scope.manifest
            && self.unit == unit
            && koru_store_core::sync_digest::revision(&self.version) == head
            && loro::VersionVector::decode(&self.version)
                .ok()
                .zip(loro::VersionVector::decode(local_version).ok())
                .is_some_and(|(remote, local)| remote == local)
    }
}

/// Open only a metadata probe on an already authenticated, fenced carrier.
/// The stream closes immediately after the one exact response. Passing the
/// detached local VV avoids unnecessary remote payload work; any attempted
/// payload/start is nevertheless refused before admission.
pub async fn inspect_version<'a, R, W, A>(
    reader: R,
    writer: W,
    config: Config,
    unit: Unit,
    local_version: Vec<u8>,
    authorize: A,
) -> Result<ObservedVersion, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    A: FnMut(Unit) -> Result<(), Refusal> + Send + 'a,
{
    let client = Client::new(
        reader,
        writer,
        config,
        |_| -> Result<std::future::Ready<Result<Receipt, Error>>, Error> { Err(Error::Invalid) },
        authorize,
        |_| Decision::Defer,
    )?;
    tokio::time::timeout(Duration::from_secs(5), probe(client, unit, local_version))
        .await
        .map_err(|_| Error::Timeout)?
}

pub(super) async fn probe<'a, R, W>(
    mut client: Client<'a, R, W>,
    unit: Unit,
    local_version: Vec<u8>,
) -> Result<ObservedVersion, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    client.inspection_only = true;
    client.scope.object_unit(unit)?;
    (client.authorize)(unit).map_err(Error::Authority)?;
    loro::VersionVector::decode(&local_version).map_err(|_| Error::Invalid)?;
    let mut version = Some(local_version);
    loop {
        if client.available_writer()
            && let Some(version) = version.take()
        {
            client.join(unit, version)?;
        }
        match client.next().await? {
            Event::Progress => {}
            Event::Metadata(mut messages) if version.is_none() => {
                let Metadata::Joined {
                    unit: received,
                    version,
                    live: false,
                } = messages.values.pop().ok_or(Error::Invalid)?
                else {
                    return Err(Error::Invalid);
                };
                if received != unit || !messages.values.is_empty() {
                    return Err(Error::Invalid);
                }
                (client.authorize)(unit).map_err(Error::Authority)?;
                let observed = ObservedVersion {
                    scope: client.scope,
                    unit,
                    version,
                    _charge: messages._charge,
                };
                client.cancel(Error::Closed);
                return Ok(observed);
            }
            _ => return Err(Error::Invalid),
        }
    }
}

#[cfg(test)]
mod tests;

/// A complete, hash-checked download with the original remote version token.
/// It has never entered a Store and is not a durable receive receipt. Keep this
/// owner alive during detached replay so the carrier memory reservation lives
/// as long as its bytes. A caller must validate causal/schema state and `matches`
/// before rendering or editing it.
pub struct ObservedDownload {
    observed: ObservedVersion,
    complete: Completed,
}
impl ObservedDownload {
    pub fn bytes(&self) -> &[u8] {
        &self.complete.bytes
    }
    pub fn observed(&self) -> &ObservedVersion {
        &self.observed
    }
    /// Replay into detached memory only. Missing ancestors or a state other
    /// than the exact sampled remote position never become a partial view.
    /// Domain schema and identity validation remain the projection's boundary.
    pub fn replay(&self) -> Result<loro::LoroDoc, Error> {
        let doc = loro::LoroDoc::new();
        let status = doc
            .import(&self.complete.bytes)
            .map_err(|_| Error::Invalid)?;
        let expected =
            loro::VersionVector::decode(&self.observed.version).map_err(|_| Error::Invalid)?;
        if status.pending.is_some() || doc.state_vv() != expected || doc.oplog_vv() != expected {
            return Err(Error::Invalid);
        }
        Ok(doc)
    }
}
impl std::fmt::Debug for ObservedDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservedDownload(<redacted>)")
    }
}

pub(super) fn idle<'a, R, W>(client: &Client<'a, R, W>) -> bool
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    client.available_writer()
        && !(client.status().committing
            || client.writing.is_some()
            || !client.controls.is_empty()
            || client.policy_pending != 0
            || client.upload.is_some()
            || client.stream.incoming_deadline().is_some()
            || client.stream.outgoing_deadline().is_some()
            || client.stream.status().draining
            || client.next_start.is_some())
}

/// Admitted only after the retained reconciler returned its idle carrier.
/// Dropping an admitted inspection cancels that inspection, never a rejected
/// pre-existing transfer. It owns no normal Store jobs once admitted.
pub struct InspectionCarrier<'a, R, W> {
    pub(super) client: Client<'a, R, W>,
}
impl<'a, R, W> InspectionCarrier<'a, R, W>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    pub async fn download(mut self, unit: Unit) -> Result<ObservedDownload, Error> {
        let result = download(&mut self.client, unit).await;
        self.client.cancel(Error::Closed);
        result
    }
}

/// Consume an idle retained carrier for one detached read. The caller ends this
/// session afterwards; no success Ack is sent because nothing was persisted.
/// Empty Join knowledge requests the complete existing source, never a seed.
pub(super) async fn download<'a, R, W>(
    client: &mut Client<'a, R, W>,
    unit: Unit,
) -> Result<ObservedDownload, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    if !idle(client) {
        return Err(Error::Unavailable);
    }
    client.scope.object_unit(unit)?;
    (client.authorize)(unit).map_err(Error::Authority)?;
    client.inspection_only = true;
    client.inspection_download = Some(unit);
    client.inspection_joined = false;
    client.inspection_finished = false;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut requested = false;
        let mut observed = None;
        loop {
            if client.available_writer() && !requested {
                client.join(unit, loro::VersionVector::default().encode())?;
                requested = true;
            }
            if client.stream.status().incoming_complete {
                let complete =
                    client
                        .stream
                        .handoff(client.now(), &mut client.authorize, |complete| complete)?;
                if complete.unit != unit
                    || complete.origin != client.scope.principal.update_origin()
                    || complete.bytes.is_empty()
                {
                    return Err(Error::Invalid);
                }
                (client.authorize)(unit).map_err(Error::Authority)?;
                client.inspection_finished = true;
                return Ok(ObservedDownload {
                    observed: observed.take().ok_or(Error::Invalid)?,
                    complete,
                });
            }
            match client.next().await? {
                Event::Progress => {}
                Event::Metadata(mut messages) if requested && observed.is_none() => {
                    let Metadata::Joined {
                        unit: received,
                        version,
                        live: false,
                    } = messages.values.pop().ok_or(Error::Invalid)?
                    else {
                        return Err(Error::Invalid);
                    };
                    if received != unit
                        || !messages.values.is_empty()
                        || loro::VersionVector::decode(&version)
                            .map_err(|_| Error::Invalid)?
                            .is_empty()
                    {
                        return Err(Error::Invalid);
                    }
                    (client.authorize)(unit).map_err(Error::Authority)?;
                    observed = Some(ObservedVersion {
                        scope: client.scope,
                        unit,
                        version,
                        _charge: messages._charge,
                    });
                    client.inspection_joined = true;
                }
                _ => return Err(Error::Invalid),
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|v| v);
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    SourceChanged,
    Unavailable,
    /// Publication may have happened. Reread Cloud before another attempt.
    OutcomeUnknown,
}
impl<'a, R, W> InspectionCarrier<'a, R, W>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
{
    /// Prepare one targeted ordinary CRDT delta from the observed Cloud copy.
    /// This is not compare-and-swap: later concurrent CRDT edits merge normally.
    /// The original source reservation lives through the final write and exact
    /// remote durable Ack. No local receipt or download Ack is manufactured.
    pub async fn edit<F>(
        mut self,
        unit: Unit,
        expected_head: [u8; 32],
        prepare: F,
    ) -> Result<(), EditError>
    where
        F: FnOnce(ObservedDownload) -> Result<(Vec<u8>, ObservedDownload), Error> + Send + 'static,
    {
        let mut publication_started = false;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let source = download(&mut self.client, unit)
                .await
                .map_err(|_| EditError::Unavailable)?;
            if source.observed.head() != expected_head {
                return Err(EditError::SourceChanged);
            }
            let (bytes, source) = tokio::task::spawn_blocking(move || prepare(source))
                .await
                .map_err(|_| EditError::Unavailable)?
                .map_err(|_| EditError::Unavailable)?;
            if bytes.is_empty()
                || bytes.len() > transfer::MAX_PAYLOAD
                || source.observed.unit != unit
                || source.observed.scope.principal != self.client.scope.principal
                || source.observed.head() != expected_head
            {
                return Err(EditError::Unavailable);
            }
            (self.client.authorize)(unit).map_err(|_| EditError::Unavailable)?;
            self.client
                .upload(unit, bytes, source)
                .map_err(|_| EditError::Unavailable)?;
            publication_started = true;
            loop {
                match self
                    .client
                    .next()
                    .await
                    .map_err(|_| EditError::OutcomeUnknown)?
                {
                    Event::Uploaded { unit: received, .. } if received == unit => return Ok(()),
                    Event::Progress => {}
                    Event::Metadata(messages)
                        if messages
                            .values
                            .iter()
                            .all(|m| matches!(m, Metadata::SyncRequest)) => {}
                    _ => return Err(EditError::OutcomeUnknown),
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(if publication_started {
                EditError::OutcomeUnknown
            } else {
                EditError::Unavailable
            })
        });
        self.client.cancel(Error::Closed);
        result
    }
}
