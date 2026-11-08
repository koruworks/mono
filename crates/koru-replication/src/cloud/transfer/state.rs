// SPDX-License-Identifier: MPL-2.0

use super::fragments::Incoming;
use super::{
    CONTROL_WAIT_MS, ClaimReferences, Control, Decision, Error, MAX_PAYLOAD, MemoryBudget,
    Outgoing, RESTRICTED_LIMIT, Record, Reservation, Start, Unit, UploadContext, budget_millis,
};
use crate::{
    cloud::{PreparedSession, Refusal},
    protocol::PeerFrame,
};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_store_core::{DocumentId, WorkspaceId};

/// Exact values from the admitted credential and current Store context.
/// Current authorization and modes are still checked through the caller's port.
#[derive(Clone, Copy)]
pub struct Scope {
    pub principal: CloudReplicaPrincipalV1,
    pub eligibility_workspace: WorkspaceId,
    pub profile: CloudId,
    pub manifest: DocumentId,
}
/// The manifest identity a worker holds for a Workspace dataset the catalog
/// has not retained yet (ADR-358): a fixed canonical v4 placeholder, never a
/// Store id. The manifest room maps to it until the device's first manifest
/// `TransferStart` names the real document, which is the client-supplied
/// intent the first checkpoint reservation then binds. It is valid as a unit
/// id so authorization and room mapping work before the bind; it is refused
/// as a catalog identity, so nothing is ever retained under it.
const UNBOUND_MANIFEST: uuid::Uuid =
    uuid::Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0000);

impl Scope {
    /// A worker's scope for a Workspace whose manifest identity is not
    /// retained yet. See [`Scope::manifest_bound`].
    pub fn unbound_manifest() -> DocumentId {
        DocumentId::from_uuid(UNBOUND_MANIFEST)
    }

    /// False only for a worker scope still waiting for the device's first
    /// manifest transfer. A client scope, and a worker scope read from the
    /// catalog, are always bound.
    pub fn manifest_bound(&self) -> bool {
        self.manifest.as_uuid() != UNBOUND_MANIFEST
    }

    /// The existing Cloud-only room mapping, reused by the durable catalog.
    /// Scope validation precedes mapping; no client catalog id is accepted.
    pub fn object_unit(&self, unit: Unit) -> Result<crate::cloud::object::Unit, Error> {
        if !unit.valid() {
            return Err(Error::Invalid);
        }
        self.room(unit)?;
        use crate::cloud::object::Unit as Object;
        Ok(match unit {
            Unit::Document { document_id, .. }
                if document_id.as_bytes() == self.manifest.as_uuid().as_bytes() =>
            {
                Object::WorkspaceManifest
            }
            Unit::Document { document_id, .. } => Object::WorkspaceDocument { document_id },
            Unit::Identity { .. } => Object::ProfileIdentity,
            Unit::Private { .. } => Object::ProfilePrivate,
            Unit::Personal { workspace_id, .. } => Object::ProfilePersonal { workspace_id },
        })
    }

    /// Encodes the ordinary Loro success Ack. Only the durable commit owner
    /// may call this for received content; this encoder does not prove storage.
    pub fn committed_ack(&self, unit: Unit, batch: [u8; 8]) -> Result<Record, Error> {
        let message = loro_protocol::ProtocolMessage::Ack {
            crdt: loro_protocol::CrdtType::Loro,
            room_id: self.room(unit)?,
            ref_id: loro_protocol::BatchId(batch),
            status: loro_protocol::UpdateStatusCode::Ok,
        };
        Ok(Record::Peer(PeerFrame::Loro {
            workspace_id: self.eligibility_workspace,
            message: loro_protocol::encoding::encode(&message).map_err(|_| Error::Invalid)?,
        }))
    }

    pub(super) fn unit_for_room(&self, room: &str) -> Result<Unit, Error> {
        let parse = |raw: &str| {
            serde_json::from_value::<CloudId>(serde_json::Value::String(raw.to_owned()))
                .map_err(|_| Error::Invalid)
        };
        let parts: Vec<_> = room.split(':').collect();
        let unit = match parts.as_slice() {
            ["document", document] => Unit::Document {
                workspace_id: CloudId::from_bytes(*self.eligibility_workspace.as_uuid().as_bytes()),
                document_id: parse(document)?,
            },
            ["workspace", workspace, "manifest"] => Unit::Document {
                workspace_id: parse(workspace)?,
                document_id: CloudId::from_bytes(*self.manifest.as_uuid().as_bytes()),
            },
            ["profile", profile, "identity"] => Unit::Identity {
                profile_id: parse(profile)?,
            },
            ["profile", profile, "private"] => Unit::Private {
                profile_id: parse(profile)?,
            },
            ["profile", profile, "personal", workspace] => Unit::Personal {
                profile_id: parse(profile)?,
                workspace_id: parse(workspace)?,
            },
            _ => return Err(Error::Invalid),
        };
        if !unit.valid() || self.room(unit)? != room {
            return Err(Error::Invalid);
        }
        Ok(unit)
    }
    fn metadata_unit(&self, encoded: &[u8]) -> Result<Unit, Error> {
        use loro_protocol::{CrdtType, ProtocolMessage as M, encoding};
        let message = encoding::try_decode(encoded).ok_or(Error::Invalid)?;
        if encoding::encode(&message).map_err(|_| Error::Invalid)? != encoded {
            return Err(Error::Invalid);
        }
        let room = match message {
            M::JoinRequest {
                crdt: CrdtType::Loro,
                room_id,
                ..
            }
            | M::JoinResponseOk {
                crdt: CrdtType::Loro,
                room_id,
                ..
            }
            | M::JoinError {
                crdt: CrdtType::Loro,
                room_id,
                ..
            }
            | M::Leave {
                crdt: CrdtType::Loro,
                room_id,
                ..
            }
            | M::RoomError {
                crdt: CrdtType::Loro,
                room_id,
                ..
            }
            | M::Ack {
                crdt: CrdtType::Loro,
                room_id,
                ..
            } => room_id,
            _ => return Err(Error::Invalid),
        };
        self.unit_for_room(&room)
    }
    pub fn room(&self, unit: Unit) -> Result<String, Error> {
        let id = |id: CloudId| {
            serde_json::to_string(&id)
                .expect("UUID encodes")
                .trim_matches('"')
                .to_owned()
        };
        match (self.principal, unit) {
            (
                CloudReplicaPrincipalV1::Workspace { workspace_id, .. },
                Unit::Document {
                    workspace_id: w,
                    document_id: d,
                },
            ) if workspace_id == w
                && w.as_bytes() == self.eligibility_workspace.as_uuid().as_bytes() =>
            {
                if d.as_bytes() == self.manifest.as_uuid().as_bytes() {
                    Ok(format!("workspace:{}:manifest", id(w)))
                } else {
                    Ok(format!("document:{}", id(d)))
                }
            }
            (
                CloudReplicaPrincipalV1::Profile { profile_id, .. },
                Unit::Identity { profile_id: p },
            ) if p == profile_id && p == self.profile => Ok(format!("profile:{}:identity", id(p))),
            (
                CloudReplicaPrincipalV1::Profile { profile_id, .. },
                Unit::Private { profile_id: p },
            ) if p == profile_id && p == self.profile => Ok(format!("profile:{}:private", id(p))),
            (
                CloudReplicaPrincipalV1::Profile { profile_id, .. },
                Unit::Personal {
                    profile_id: p,
                    workspace_id: w,
                },
            ) if p == profile_id
                && p == self.profile
                && w.as_bytes() == self.eligibility_workspace.as_uuid().as_bytes() =>
            {
                Ok(format!("profile:{}:personal:{}", id(p), id(w)))
            }
            _ => Err(Error::Authority(Refusal::WorkspaceNotAuthorized)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Automatic,
    AwaitDecision,
    Deferred,
    Accepted,
    Metadata,
    Complete,
    Policy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamStatus {
    pub local_generation: i64,
    pub peer_generation: Option<i64>,
    pub incoming_bytes: usize,
    pub outgoing_bytes: usize,
    pub incoming_complete: bool,
    pub draining: bool,
    pub closed: Option<Error>,
}

/// Only constructed by complete/hash-checked handoff. This is not a durable ACK.
/// The existing Store import/causal-check/flush port must succeed first.
pub struct Completed {
    pub unit: Unit,
    pub bytes: Vec<u8>,
    pub batch_id: [u8; 8],
    pub origin: koru_store_core::ports::UpdateOrigin,
    /// The claim reference the transfer's start carried, retained with the
    /// rest of the upload for as long as its work is (TECHNICAL §5.7, S2).
    /// Only a worker serving an app connection ever holds one: every other
    /// session refuses a start that carries it.
    pub claim: Option<[u8; 32]>,
    _reservation: Reservation,
}
struct Sending {
    export: Outgoing,
    deadline: u64,
    decision_deadline: u64,
    accepted: bool,
}

/// One bidirectional dataset stream under one serialized owner. Times are
/// monotonic milliseconds in the owner's clock domain; no method refreshes expiry.
pub struct Stream {
    scope: Scope,
    expires: u64,
    initialized_by: u64,
    local: (i64, usize),
    local_sent: bool,
    peer: Option<(i64, usize)>,
    incoming: Option<Incoming>,
    outgoing: Option<Sending>,
    waiting: Option<(Start, u64, u64)>,
    budget: MemoryBudget,
    claims: ClaimReferences,
    draining: bool,
    closed: Option<Error>,
}
impl Stream {
    /// Create after verified acceptance. Convert the earliest signed expiry
    /// conservatively using the same wall-clock observation as acceptance.
    /// This helper does not turn on PreparedSession::admit_transfer().
    pub fn new(
        accepted: &PreparedSession,
        scope: Scope,
        wall_now_ms: i64,
        mono_now_ms: u64,
        automatic_limit: usize,
        budget: MemoryBudget,
    ) -> Result<Self, Error> {
        if accepted.principal != scope.principal
            || ![0, RESTRICTED_LIMIT, MAX_PAYLOAD].contains(&automatic_limit)
        {
            return Err(Error::Invalid);
        }
        let remaining = accepted
            .expires_at_seconds
            .checked_mul(1000)
            .and_then(|v| v.checked_sub(wall_now_ms))
            .filter(|v| *v > 0)
            .ok_or_else(|| {
                unavailable("credential_expired", 0, 0, 0);
                Error::Unavailable
            })?;
        let expires = mono_now_ms
            .checked_add(remaining as u64)
            .ok_or(Error::Unavailable)?;
        Ok(Self {
            scope,
            expires,
            initialized_by: mono_now_ms
                .checked_add(CONTROL_WAIT_MS)
                .ok_or(Error::Invalid)?,
            local: (1, automatic_limit),
            local_sent: false,
            peer: None,
            incoming: None,
            outgoing: None,
            waiting: None,
            budget,
            claims: accepted.claim_references,
            draining: false,
            closed: None,
        })
    }
    /// The scope as it stands, including a manifest identity bound by the
    /// first manifest transfer; a worker refreshes its own copy from here
    /// after every admitted `TransferStart`.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn status(&self) -> StreamStatus {
        StreamStatus {
            local_generation: self.local.0,
            peer_generation: self.peer.map(|v| v.0),
            incoming_bytes: self.incoming.as_ref().map_or(0, |v| v.bytes.len()),
            outgoing_bytes: self
                .outgoing
                .as_ref()
                .map_or(0, |v| v.export.start.total_length),
            incoming_complete: self.incoming.as_ref().is_some_and(Incoming::complete),
            draining: self.draining,
            closed: self.closed,
        }
    }
    pub fn expires_at(&self) -> u64 {
        self.expires
    }
    pub fn incoming_deadline(&self) -> Option<u64> {
        self.incoming.as_ref().map(|v| v.deadline)
    }
    pub fn outgoing_deadline(&self) -> Option<u64> {
        self.outgoing.as_ref().map(|v| v.deadline)
    }
    /// Earliest existing absolute timer, in the supplied monotonic clock domain.
    /// The serving owner must also retain deadlines for handed-off Store work.
    pub fn next_deadline(&self) -> u64 {
        let mut deadline = self.expires;
        if !self.local_sent || self.peer.is_none() {
            deadline = deadline.min(self.initialized_by);
        }
        if let Some(incoming) = &self.incoming {
            deadline = deadline.min(incoming.deadline);
        }
        if let Some((_, transfer, decision)) = &self.waiting {
            deadline = deadline.min(*transfer).min(*decision);
        }
        if let Some(outgoing) = &self.outgoing {
            deadline = deadline.min(outgoing.deadline);
            if !outgoing.accepted {
                deadline = deadline.min(outgoing.decision_deadline);
            }
        }
        deadline
    }
    pub fn cancel(&mut self, reason: Error) -> Error {
        self.closed.get_or_insert(reason);
        self.incoming = None;
        self.outgoing = None;
        self.waiting = None;
        self.closed.unwrap()
    }
    /// A generation replacement allows only work already admitted to drain.
    /// Call cancel instead for account/authority/lease loss.
    pub fn drain(&mut self) {
        self.draining = true;
        self.waiting = None;
        if self.outgoing.as_ref().is_some_and(|v| !v.accepted) {
            self.outgoing = None;
        }
    }
    pub fn check(&mut self, now: u64) -> Result<(), Error> {
        if let Some(e) = self.closed {
            return Err(e);
        }
        let failure = if now >= self.expires {
            Some(Error::Authority(Refusal::Credential(
                koru_account_session::TokenCapabilityVerificationError::CredentialExpired,
            )))
        } else if (!self.local_sent || self.peer.is_none()) && now >= self.initialized_by {
            Some(Error::Timeout)
        } else if self.incoming.as_ref().is_some_and(|v| now >= v.deadline)
            || self
                .waiting
                .as_ref()
                .is_some_and(|(_, deadline, decision)| now >= *deadline || now >= *decision)
            || self
                .outgoing
                .as_ref()
                .is_some_and(|v| now >= v.deadline || (!v.accepted && now >= v.decision_deadline))
        {
            Some(Error::Timeout)
        } else {
            None
        };
        match failure {
            Some(e) => Err(self.cancel(e)),
            None => Ok(()),
        }
    }
    pub fn initial_policy(&mut self, now: u64) -> Result<Control, Error> {
        self.check(now)?;
        if self.local_sent {
            return Err(self.cancel(Error::Invalid));
        }
        self.local_sent = true;
        Ok(Control::policy(self.local.0, self.local.1))
    }
    /// Called on every relevant observation change, even equal numeric ceilings.
    /// A caller must write this returned policy before receiving new starts.
    pub fn change_policy(&mut self, limit: usize, now: u64) -> Result<Control, Error> {
        self.check(now)?;
        if !self.local_sent || ![0, RESTRICTED_LIMIT, MAX_PAYLOAD].contains(&limit) {
            return Err(self.cancel(Error::Invalid));
        }
        if self.incoming.is_some() || self.waiting.is_some() {
            return Err(self.cancel(Error::PolicyChanged));
        }
        self.local = (
            self.local
                .0
                .checked_add(1)
                .ok_or_else(|| self.cancel(Error::Invalid))?,
            limit,
        );
        Ok(Control::policy(self.local.0, limit))
    }
    fn deadline(&self, start: &Start, first: u64, now: u64) -> Result<u64, Error> {
        if now < first || now - first >= CONTROL_WAIT_MS {
            return Err(Error::Timeout);
        }
        let budget = budget_millis(start.total_length)?;
        let remaining = self.expires.saturating_sub(now);
        if remaining < budget + 15000 {
            unavailable("deadline", remaining, budget, start.total_length);
            return Err(Error::Unavailable);
        }
        first
            .checked_add(budget)
            .filter(|v| *v > now)
            .ok_or(Error::Timeout)
    }
    /// Controls may cross an opposite-direction content transfer. Scope checks
    /// use current trusted policy, not a remembered successful handshake.
    pub fn receive_control(
        &mut self,
        control: Control,
        first: u64,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
    ) -> Result<Admission, Error> {
        let result = self.receive_control_inner(control, first, now, authorize);
        result.map_err(|e| self.cancel(e))
    }
    fn receive_control_inner(
        &mut self,
        control: Control,
        first: u64,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
    ) -> Result<Admission, Error> {
        self.check(now)?;
        control.validate()?;
        if !self.local_sent {
            return Err(Error::Invalid);
        }
        if self.peer.is_none() && !matches!(control, Control::TransferPolicy { .. }) {
            return Err(Error::Invalid);
        }
        match control {
            Control::TransferPolicy {
                policy_generation: g,
                automatic_limit_bytes: l,
                ..
            } => {
                match self.peer {
                    None if g == 1 => {}
                    Some(old) if old == (g, l) => return Ok(Admission::Policy),
                    Some((old, _)) if old.checked_add(1) == Some(g) => {
                        if self.outgoing.is_some() {
                            return Err(Error::PolicyChanged);
                        }
                    }
                    _ => return Err(Error::Invalid),
                }
                self.peer = Some((g, l));
                Ok(Admission::Policy)
            }
            Control::TransferStart(start) => {
                if self.draining || self.incoming.is_some() || self.waiting.is_some() {
                    return Err(Error::Invalid);
                }
                // The session, not the unit, decides (DS-D46): a reference
                // crosses only from an app connection to the worker that
                // admitted it. Checked before the start can bind anything.
                if start.claim.is_some() && self.claims != ClaimReferences::Received {
                    return Err(Error::Invalid);
                }
                if start.policy_generation < self.local.0 {
                    return Err(Error::PolicyChanged);
                }
                if start.policy_generation != self.local.0 || self.local.1 == 0 {
                    return Err(Error::Invalid);
                }
                // A worker whose Workspace has no retained manifest identity
                // learns it here, from the device's first Document transfer:
                // the manifest is the first unit a device reconciles, and the
                // fragments that follow must carry the manifest room, so a
                // transfer that is not the manifest is refused by its own
                // frames rather than bound. Nothing is retained until the
                // first checkpoint reservation carries this id (ADR-358).
                if !self.scope.manifest_bound()
                    && let Unit::Document {
                        workspace_id,
                        document_id,
                    } = start.unit
                    && matches!(
                        self.scope.principal,
                        CloudReplicaPrincipalV1::Workspace { workspace_id: w, .. } if w == workspace_id
                    )
                    && start.unit.valid()
                {
                    self.scope.manifest =
                        DocumentId::from_uuid(uuid::Uuid::from_bytes(*document_id.as_bytes()));
                }
                let room = self.scope.room(start.unit)?;
                authorize(start.unit).map_err(Error::Authority)?;
                let deadline = self.deadline(&start, first, now)?;
                if start.total_length <= self.local.1 {
                    self.incoming = Some(Incoming::new(start, room, deadline, &self.budget)?);
                    Ok(Admission::Automatic)
                } else {
                    self.waiting = Some((
                        start,
                        deadline,
                        now.checked_add(CONTROL_WAIT_MS).ok_or(Error::Invalid)?,
                    ));
                    Ok(Admission::AwaitDecision)
                }
            }
            Control::TransferDecision {
                policy_generation,
                unit,
                batch_id,
                sha256,
                decision,
                ..
            } => {
                let sending = self.outgoing.as_mut().ok_or(Error::Invalid)?;
                let s = &sending.export.start;
                if sending.accepted
                    || s.policy_generation != policy_generation
                    || s.unit != unit
                    || s.batch_id != batch_id
                    || s.sha256 != sha256
                {
                    return Err(Error::Invalid);
                }
                authorize(unit).map_err(Error::Authority)?;
                if decision == Decision::Defer {
                    self.outgoing = None;
                    Ok(Admission::Deferred)
                } else {
                    let budget = budget_millis(s.total_length)?;
                    let remaining = self.expires.saturating_sub(now);
                    if remaining < budget + 15000 {
                        unavailable("peer_accepted", remaining, budget, s.total_length);
                        return Err(Error::Unavailable);
                    }
                    sending.accepted = true;
                    Ok(Admission::Accepted)
                }
            }
        }
    }
    /// Human waiting calls Defer immediately. Accept requires an exact current
    /// one-shot permission supplied by the coordinator, not a persistent receipt.
    pub fn decide(
        &mut self,
        expected: &Start,
        decision: Decision,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
    ) -> Result<Control, Error> {
        let result = (|| {
            self.check(now)?;
            let (start, deadline, _) = self.waiting.take().ok_or(Error::Invalid)?;
            if start != *expected {
                return Err(Error::Invalid);
            }
            if decision == Decision::Accept {
                let budget = budget_millis(start.total_length)?;
                let remaining = self.expires.saturating_sub(now);
                if remaining < budget + 15000 {
                    unavailable("accept", remaining, budget, start.total_length);
                    return Err(Error::Unavailable);
                }
                authorize(start.unit).map_err(Error::Authority)?;
                let room = self.scope.room(start.unit)?;
                self.incoming = Some(Incoming::new(start.clone(), room, deadline, &self.budget)?);
            }
            Ok(Control::decision(&start, decision))
        })();
        result.map_err(|e| self.cancel(e))
    }
    pub fn receive_peer(
        &mut self,
        frame: PeerFrame,
        now: u64,
        authorize: impl FnMut(Unit) -> Result<(), Refusal>,
    ) -> Result<Admission, Error> {
        self.receive_peer_owned(frame, now, authorize).map(|v| v.0)
    }
    /// Consume payload frames, but return validated metadata to its dispatcher
    /// without cloning expanded payloads or releasing their transport charge.
    pub fn receive_peer_owned(
        &mut self,
        frame: PeerFrame,
        now: u64,
        mut authorize: impl FnMut(Unit) -> Result<(), Refusal>,
    ) -> Result<(Admission, Option<PeerFrame>), Error> {
        let result = (|| {
            self.check(now)?;
            if !self.local_sent || self.peer.is_none() {
                return Err(Error::Invalid);
            }
            match frame {
                PeerFrame::LoroBatch {
                    workspace_id,
                    messages,
                } => {
                    if workspace_id != self.scope.eligibility_workspace {
                        return Err(Error::Invalid);
                    }
                    if let Some(pending) = self.incoming.as_mut() {
                        authorize(pending.start.unit).map_err(Error::Authority)?;
                        Ok((
                            if pending.frame(messages)? {
                                Admission::Complete
                            } else {
                                Admission::Accepted
                            },
                            None,
                        ))
                    } else {
                        if messages.is_empty() {
                            return Err(Error::Invalid);
                        }
                        for message in &messages {
                            authorize(self.scope.metadata_unit(message)?)
                                .map_err(Error::Authority)?;
                        }
                        Ok((
                            Admission::Metadata,
                            Some(PeerFrame::LoroBatch {
                                workspace_id,
                                messages,
                            }),
                        ))
                    }
                }
                PeerFrame::Loro {
                    workspace_id,
                    message,
                } => {
                    if workspace_id != self.scope.eligibility_workspace {
                        return Err(Error::Invalid);
                    }
                    authorize(self.scope.metadata_unit(&message)?).map_err(Error::Authority)?;
                    Ok((
                        Admission::Metadata,
                        Some(PeerFrame::Loro {
                            workspace_id,
                            message,
                        }),
                    ))
                }
                PeerFrame::ProfileUpdates { .. }
                | PeerFrame::BlobRequest { .. }
                | PeerFrame::BlobChunk(..)
                | PeerFrame::TrustRecord { .. }
                | PeerFrame::TrustRecords { .. }
                | PeerFrame::TrustAck => Err(Error::Invalid),
                metadata => Ok((Admission::Metadata, Some(metadata))),
            }
        })();
        result.map_err(|e| self.cancel(e))
    }
    /// Serialize this call with policy changes. Store work already handed off
    /// may finish; current authority/lease commit fences remain Store-owned.
    pub fn handoff<T>(
        &mut self,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
        commit: impl FnOnce(Completed) -> T,
    ) -> Result<T, Error> {
        let prepared = (|| {
            self.check(now)?;
            let incoming = self.incoming.take().ok_or(Error::Invalid)?;
            if !incoming.complete() {
                return Err(Error::Invalid);
            }
            authorize(incoming.start.unit).map_err(Error::Authority)?;
            Ok(Completed {
                unit: incoming.start.unit,
                bytes: incoming.bytes,
                batch_id: incoming.start.batch_id,
                origin: self.scope.principal.update_origin(),
                claim: incoming.start.claim.map(|claim| claim.0),
                _reservation: incoming.reservation,
            })
        })();
        match prepared {
            Ok(complete) => Ok(commit(complete)),
            Err(e) => Err(self.cancel(e)),
        }
    }
    pub fn begin_outgoing(
        &mut self,
        unit: Unit,
        batch: [u8; 8],
        bytes: Vec<u8>,
        first: u64,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
    ) -> Result<(Control, Admission), Error> {
        self.begin_upload(
            unit,
            batch,
            bytes,
            UploadContext::default(),
            first,
            now,
            authorize,
        )
    }
    /// [`Stream::begin_outgoing`] with what the Store said about the upload:
    /// its start carries `context.claim`. A reference on a session that may
    /// not send one (a device's, or the worker's own starts) is refused
    /// before anything is written, and ends the stream like any other
    /// invalid transfer.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_upload(
        &mut self,
        unit: Unit,
        batch: [u8; 8],
        bytes: Vec<u8>,
        context: UploadContext,
        first: u64,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
    ) -> Result<(Control, Admission), Error> {
        let result = (|| {
            self.check(now)?;
            if self.draining || !self.local_sent || self.outgoing.is_some() {
                return Err(Error::Invalid);
            }
            if context.claim.is_some() && self.claims != ClaimReferences::Sent {
                return Err(Error::Invalid);
            }
            let (generation, limit) = self.peer.ok_or(Error::Invalid)?;
            if limit == 0 {
                unavailable(
                    "peer_limit_zero",
                    self.expires.saturating_sub(now),
                    0,
                    bytes.len(),
                );
                return Err(Error::Unavailable);
            }
            let room = self.scope.room(unit)?;
            authorize(unit).map_err(Error::Authority)?;
            let export = Outgoing::new(
                unit,
                self.scope.eligibility_workspace,
                room,
                generation,
                batch,
                bytes,
                context.claim,
                &self.budget,
            )?;
            let deadline = self.deadline(&export.start, first, now)?;
            let accepted = export.start.total_length <= limit;
            let control = Control::TransferStart(export.start.clone());
            self.outgoing = Some(Sending {
                export,
                deadline,
                decision_deadline: now.checked_add(CONTROL_WAIT_MS).ok_or(Error::Invalid)?,
                accepted,
            });
            Ok((
                control,
                if accepted {
                    Admission::Automatic
                } else {
                    Admission::AwaitDecision
                },
            ))
        })();
        result.map_err(|e| self.cancel(e))
    }
    pub fn next_outgoing(
        &mut self,
        now: u64,
        authorize: impl FnOnce(Unit) -> Result<(), Refusal>,
    ) -> Result<Option<super::ReservedRecord>, Error> {
        let result = (|| {
            self.check(now)?;
            let sending = self.outgoing.as_mut().ok_or(Error::Invalid)?;
            if !sending.accepted {
                return Err(Error::Invalid);
            }
            authorize(sending.export.start.unit).map_err(Error::Authority)?;
            if sending.export.finished() {
                return Ok(None);
            }
            let reservation = self
                .budget
                .reserve_record(sending.export.next_frame_bound())?;
            Ok(sending
                .export
                .next_frame()?
                .map(|frame| super::ReservedRecord {
                    record: Record::Peer(frame),
                    _reservation: reservation,
                }))
        })();
        result.map_err(|e| self.cancel(e))
    }
    /// Only after the transport finishes writing the final payload frame.
    /// A local successful write is not a remote durable acknowledgement.
    pub fn outbound_written(&mut self, now: u64) -> Result<(), Error> {
        self.check(now)?;
        if !self.outgoing.as_ref().is_some_and(|v| v.export.finished()) {
            return Err(self.cancel(Error::Invalid));
        }
        self.outgoing = None;
        Ok(())
    }
}

/// **Every `sync_unavailable` names its check.** A worker that ended a
/// session with that code on the same document sixteen passes running, at
/// a phone that then skipped every dataset after it, could not be told
/// apart from a worker out of memory or a credential minted too early:
/// five sites answered the one code and none of them said so. Millis,
/// sizes and a site name; nothing about what the bytes hold.
fn unavailable(site: &'static str, remaining_ms: u64, budget_ms: u64, total_length: usize) {
    tracing::warn!(
        site,
        remaining_ms,
        budget_ms,
        total_length,
        "cloud transfer: unavailable"
    );
}
