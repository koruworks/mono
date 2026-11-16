// SPDX-License-Identifier: MPL-2.0

//! The plan of one private read: which picked item the model named, whether
//! this Run may read it now, must ask, or cannot, and the exact operation the
//! delegate is then asked to record.
//!
//! The plan reads ai.db and parses the model's link in memory. It opens no
//! credential, calls no vendor and writes nothing to ai.db. The link is never
//! stored, logged or echoed: it does not leave this module.
//!
//! **What a Run without a grant can learn is one bit.** Every situation that
//! needs the person plans as [`PrivateReadVerdict::NeedsPerson`], whatever the
//! reason; the reason is kept for the person's own card
//! ([`AiWorkerClient::private_setup_request`]) and is never part of what the
//! model is told.

use super::*;
use koru_ai_core::connections::{
    Connection, ConnectionError, ConnectionId, ConnectionItem, ConnectionItemId,
    ConnectionItemState, ConnectionState,
};
use koru_delegate_core::decisions::{DecisionBindingV1, DecisionDigest, DecisionRevision};
use koru_delegate_core::{
    BotProvenance, CapturedBotPolicy, ConnectionGrantRef, FreshBotPolicyProof,
    PrivateOperationKind, PrivateOperationProposal, RunId, private_request_fingerprint,
};

/// How the model named the item. A link is model output: its `Debug` prints
/// nothing of it.
#[derive(Clone)]
pub enum PrivateItemName {
    /// The `item_id` a granted Run took from the list, or the one the
    /// continuation of an approved read names.
    Id(ConnectionItemId),
    /// The link or vendor id the person gave the model in the conversation.
    Link(String),
}
impl std::fmt::Debug for PrivateItemName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => f.debug_tuple("Id").field(id).finish(),
            Self::Link(_) => f.write_str("Link([REDACTED])"),
        }
    }
}

/// Internal composition request. No public API or provider principal can
/// create it: the gateway builds it from the Run's classified authority.
#[derive(Clone)]
pub struct PrivateReadPlanRequest {
    pub run: RunId,
    pub provenance: BotProvenance,
    pub captured: CapturedBotPolicy,
    pub operation_id: Uuid,
    pub item: PrivateItemName,
    pub connection: Option<ConnectionId>,
}

/// Why a read needs the person. For the person's card only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateSetupReason {
    /// No account is connected.
    NoConnection,
    /// An account is connected and this item was never picked.
    NotPicked,
    /// More than one connected account holds the item.
    ChooseAccount,
    /// The connection needs sign-in again.
    NeedsSignIn,
    /// The connection is disconnected, or disconnecting.
    Disconnected,
    /// The item is no longer the one the person chose.
    ItemChanged,
    /// Questions are not answerable in this build: access must be given in
    /// advance.
    GiveAccess,
}

/// What the gateway does with one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateReadVerdict {
    /// The Run's captured and current policy both hold the grant and the
    /// item can be read: prepare the effect and perform the read.
    Granted,
    /// Everything is in order except the grant: ask the person once.
    Ask,
    /// The person must do something first. One code for the model.
    NeedsPerson(PrivateSetupReason),
    /// A typed refusal, by the model-facing code.
    Refused(&'static str),
}

/// The plan. `proposal` is present whenever the named item is one picked
/// item of the Run's owner, whatever the verdict: the exact retry of an
/// approved read must be able to consume its decision even when the read
/// will then be refused, so that the decision settles.
#[derive(Clone)]
pub struct PrivateReadPlan {
    pub verdict: PrivateReadVerdict,
    pub proposal: Option<PrivateOperationProposal>,
    pub proof: FreshBotPolicyProof,
    pub profile: koru_ai_core::ProfileId,
}
impl std::fmt::Debug for PrivateReadPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateReadPlan")
            .field("verdict", &self.verdict)
            .finish_non_exhaustive()
    }
}

pub(in crate::worker) enum Incoming {
    Plan {
        request: Box<PrivateReadPlanRequest>,
        evidence: Box<bots::policy::NativePolicyEvidence>,
        /// Whether a question may be asked in the caller's build.
        questions: bool,
        reply: SyncSender<ApiResult<PrivateReadPlan>>,
    },
    Setup {
        run: RunId,
        reply: SyncSender<Option<PrivateSetupReason>>,
    },
}

/// The model-facing code of a build, platform or worker that hosts no
/// private connection.
pub const PRIVATE_UNAVAILABLE: &str = "private_unavailable";
/// A Routine Run without the grant: a Routine cannot ask (D6).
pub const ROUTINE_NOT_GRANTED: &str = "routine_connection_not_granted";

/// How many Runs' setup reasons are remembered for their cards.
const SETUP_REASONS: usize = 256;
/// How many attempts are remembered as having planned a private read.
const PRIVATE_ATTEMPTS: usize = 1024;

/// What the named item resolved to among the person's picked items.
pub(super) enum Resolved {
    One(Box<(Connection, ConnectionItem)>),
    None(PrivateSetupReason),
}

/// The pure half of the plan: the verdict for a resolved item under the
/// Run's captured policy and the current proof.
pub(super) fn verdict(
    provenance: &BotProvenance,
    captured: &CapturedBotPolicy,
    proof: &FreshBotPolicyProof,
    resolved: &Resolved,
    questions: bool,
) -> PrivateReadVerdict {
    use PrivateReadVerdict as V;
    use PrivateSetupReason as S;
    // The subject and the proof name the Routine together, or neither does.
    let (current, routine) = match (&provenance.routine, &proof.routine) {
        (None, None) => (&proof.global.policy, false),
        (Some(subject), Some(grant))
            if grant.owner == provenance.owner
                && grant.bot == provenance.bot
                && grant.schedule == subject.schedule =>
        {
            (&grant.policy, true)
        }
        _ => return V::Refused("native_policy_unavailable"),
    };
    let effects = proof.global.effects_allowed
        && proof
            .routine
            .as_ref()
            .is_none_or(|grant| grant.effects_allowed);
    let Resolved::One(found) = resolved else {
        let Resolved::None(reason) = resolved else {
            unreachable!("two alternatives")
        };
        // A Routine cannot ask, and is told nothing about what exists.
        return if routine {
            V::Refused(ROUTINE_NOT_GRANTED)
        } else {
            V::NeedsPerson(*reason)
        };
    };
    let (connection, item) = found.as_ref();
    let grant = ConnectionGrantRef {
        connection: connection.id,
        operation: PrivateOperationKind::ReadItem,
    };
    // A grant added after the Run was admitted does not broaden the Run:
    // both the captured and the current policy must hold it.
    let granted = captured.grants_connection(&grant) && current.grants_connection(&grant);
    let unusable = match (connection.state, item.state) {
        (ConnectionState::NeedsSignIn, _) => Some(S::NeedsSignIn),
        (ConnectionState::Disconnecting | ConnectionState::Disconnected, _) => {
            Some(S::Disconnected)
        }
        (ConnectionState::Connected, ConnectionItemState::Changed) => Some(S::ItemChanged),
        (ConnectionState::Connected, ConnectionItemState::Available) => None,
    };
    if granted {
        // A Run that holds the grant is told the specific reason.
        return match unusable {
            Some(S::NeedsSignIn) => V::Refused("connection_needs_sign_in"),
            Some(S::ItemChanged) => V::Refused("item_changed"),
            Some(_) => V::Refused("connection_revoked"),
            None if !effects => V::Refused("native_policy_pending"),
            None => V::Granted,
        };
    }
    if routine {
        return V::Refused(ROUTINE_NOT_GRANTED);
    }
    if let Some(reason) = unusable {
        return V::NeedsPerson(reason);
    }
    let homed = |policy: &CapturedBotPolicy| {
        policy.valid()
            && policy.workspace == captured.workspace
            && policy.destination == captured.destination
    };
    if !effects || !homed(captured) || !homed(current) {
        return V::Refused("native_policy_pending");
    }
    if questions {
        V::Ask
    } else {
        V::NeedsPerson(S::GiveAccess)
    }
}

/// The failure of a worker, build or platform that hosts no private
/// connection. Fixed text.
pub(in crate::worker) fn private_unavailable() -> Failure {
    failure(
        PRIVATE_UNAVAILABLE,
        "Private access is not available in this build.",
        false,
    )
}

/// The one picked item the model named, among the person's own. Reads
/// ai.db and parses a link in memory; nothing is written and the link goes
/// nowhere else.
pub(super) fn resolve(
    repository: &Repository,
    vendor: &dyn koru_connections::ConnectionVendor,
    owner: koru_ai_core::bots::BotOwner,
    request: &PrivateReadPlanRequest,
) -> ApiResult<Resolved> {
    use PrivateSetupReason as S;
    // Another person's row is not this person's item: it resolves to
    // nothing, as a row that does not exist does.
    let foreign = |error: ConnectionError| {
        matches!(
            error,
            ConnectionError::Forbidden | ConnectionError::NotFound
        )
    };
    let none = || -> ApiResult<Resolved> {
        match repository.live_connections(owner) {
            Ok(live) if live.is_empty() => Ok(Resolved::None(S::NoConnection)),
            Ok(_) => Ok(Resolved::None(S::NotPicked)),
            Err(error) if foreign(error) => Ok(Resolved::None(S::NotPicked)),
            Err(_) => Err(unavailable_failure()),
        }
    };
    let wanted = |connection: &Connection| request.connection.is_none_or(|id| id == connection.id);
    match &request.item {
        PrivateItemName::Id(id) => match repository.connection_item(owner, *id) {
            Ok(Some(found)) if wanted(&found.0) => Ok(Resolved::One(Box::new(found))),
            Ok(_) => none(),
            Err(error) if foreign(error) => none(),
            Err(_) => Err(unavailable_failure()),
        },
        PrivateItemName::Link(text) => {
            let provider = vendor.provider();
            let Some(reference) = vendor.item_reference(text) else {
                return none();
            };
            let digest = koru_connections::item_ref_digest(provider, &reference);
            let mut found = match repository.connection_items_by_ref(owner, provider, &digest) {
                Ok(found) => found,
                Err(error) if foreign(error) => return none(),
                Err(_) => return Err(unavailable_failure()),
            };
            found.retain(|(connection, _)| wanted(connection));
            match found.len() {
                0 => none(),
                1 => Ok(Resolved::One(Box::new(found.remove(0)))),
                // The person resolves it by giving the Bot access to one of
                // them; the model is never asked to choose.
                _ => Ok(Resolved::None(S::ChooseAccount)),
            }
        }
    }
}

/// The operation as the delegate is asked to record it, from ai.db's rows.
pub(super) fn proposal(
    operation_id: Uuid,
    connection: &Connection,
    item: &ConnectionItem,
) -> Option<PrivateOperationProposal> {
    let item_ref = DecisionDigest::from_bytes(item.item_ref_digest);
    let operation = PrivateOperationKind::ReadItem;
    Some(PrivateOperationProposal {
        operation_id,
        connection: connection.id,
        connection_revision: DecisionRevision::new(connection.revision).ok()?,
        account: DecisionDigest::from_bytes(connection.account_subject_digest),
        item: item.id,
        item_ref,
        operation,
        request_fingerprint: private_request_fingerprint(
            operation,
            connection.id,
            item.id,
            item_ref,
        ),
    })
}

impl AiWorkerClient {
    /// Trusted host composition only; deliberately absent from
    /// `AiServerPort` and HTTP. Plans one private read for a classified Bot
    /// Run. No credential is opened and no vendor is called.
    ///
    /// `questions` says whether a question may be asked in this build. When
    /// it may not, a read that would have asked needs the person instead.
    pub fn private_read_plan(
        &self,
        request: PrivateReadPlanRequest,
        questions: bool,
    ) -> ApiResult<PrivateReadPlan> {
        // A worker that hosts no connection answers before anything else is
        // asked: no policy job, no row read.
        if self.connections.is_none() {
            return Err(private_unavailable());
        }
        // The same fresh evidence every native tool reads: the Bot's current
        // barrier, the Routine's for a Routine Run, and the profile.
        let evidence = self.native_policy(bots::policy::NativePolicyRequest {
            run: request.run,
            provenance: request.provenance.clone(),
            captured: request.captured.clone(),
            target: None,
        })?;
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::PrivatePlan(Incoming::Plan {
                request: Box::new(request),
                evidence: Box::new(evidence),
                questions,
                reply,
            }))
            .map_err(queue_failure)?;
        receive_api(response)
    }

    /// Why this Run's last private read needed the person, for the card that
    /// tells them. `None` when it did not, or the worker cannot answer. The
    /// reason is never given to a model.
    pub fn private_setup_request(&self, run: RunId) -> Option<PrivateSetupReason> {
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::PrivatePlan(Incoming::Setup { run, reply }))
            .ok()?;
        response.recv_timeout(RESPONSE_TIMEOUT).ok().flatten()
    }
}

impl WorkerState {
    pub(in crate::worker) fn handle_private_plan(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Setup { run, reply } => {
                let _ = reply.send(self.private_setup.get(&run).copied());
            }
            Incoming::Plan {
                request,
                evidence,
                questions,
                reply,
            } => {
                let _ = reply.send(self.private_plan(*request, *evidence, questions));
            }
        }
    }

    fn private_plan(
        &mut self,
        request: PrivateReadPlanRequest,
        evidence: bots::policy::NativePolicyEvidence,
        questions: bool,
    ) -> ApiResult<PrivateReadPlan> {
        let bots::policy::NativePolicyEvidence { proof, profile } = evidence;
        let Some(hosted) = self.connections.clone() else {
            return Err(private_unavailable());
        };
        // From here the turn may be handed private content. The worker keeps
        // no tool input and no tool output for it (contract 2.6).
        if let Some(attempt) = request
            .provenance
            .ai_attempt_id
            .and_then(|id| InferenceAttemptId::parse(&id.to_string()))
        {
            if self.private_attempts.len() >= PRIVATE_ATTEMPTS {
                self.private_attempts.clear();
            }
            self.private_attempts.insert(attempt);
        }
        let owner = koru_ai_core::bots::BotOwner {
            user_scope: request.provenance.owner.user_scope,
            device: request.provenance.owner.device,
        };
        let resolved = resolve(&self.repository, hosted.vendor.as_ref(), owner, &request)?;
        let verdict = verdict(
            &request.provenance,
            &request.captured,
            &proof,
            &resolved,
            questions,
        );
        let proposal = match &resolved {
            Resolved::One(found) => {
                let (connection, item) = found.as_ref();
                // The exact retry of a question already asked replays the
                // operation as it was bound, so a connection that changed
                // since is refused by the read and not mistaken for another
                // operation under the same id.
                self.decision_waits
                    .bound_private(request.run, request.operation_id)
                    .filter(|bound| bound.connection == connection.id && bound.item == item.id)
                    .or_else(|| proposal(request.operation_id, connection, item))
            }
            Resolved::None(_) => None,
        };
        match verdict {
            PrivateReadVerdict::NeedsPerson(reason) => {
                if self.private_setup.len() >= SETUP_REASONS
                    && !self.private_setup.contains_key(&request.run)
                {
                    self.private_setup.clear();
                }
                self.private_setup.insert(request.run, reason);
            }
            _ => {
                self.private_setup.remove(&request.run);
            }
        }
        Ok(PrivateReadPlan {
            verdict,
            proposal,
            proof,
            profile,
        })
    }

    /// Whether an Allow on this private question may still be forwarded:
    /// the connection is connected at the bound revision under the bound
    /// account, and the item is available. A Deny is always forwarded.
    pub(in crate::worker) fn private_question_current(
        &self,
        owner: koru_ai_core::bots::BotOwner,
        bound: &PrivateOperationProposal,
    ) -> bool {
        matches!(
            self.repository.connection_item(owner, bound.item),
            Ok(Some((connection, item)))
                if connection.id == bound.connection
                    && connection.state == ConnectionState::Connected
                    && connection.revision == bound.connection_revision.get()
                    && &connection.account_subject_digest == bound.account.as_bytes()
                    && &item.item_ref_digest == bound.item_ref.as_bytes()
                    && item.state == ConnectionItemState::Available
        )
    }
}

impl bots::decision_wait::DecisionWaitDriver {
    /// The operation of a private question already asked for this Run under
    /// this operation id, exactly as it was bound, if its receipt was handed
    /// over.
    pub(in crate::worker) fn bound_private(
        &self,
        run: RunId,
        operation_id: Uuid,
    ) -> Option<PrivateOperationProposal> {
        self.private_bindings().find_map(|binding| {
            (binding.run == run && binding.operation_id == operation_id).then(|| binding.proposal())
        })
    }
}

/// Whether a decision binding is a private read, for the sites that treat it
/// as a Koru decision.
pub(in crate::worker) fn private_binding(
    binding: &DecisionBindingV1,
) -> Option<&koru_delegate_core::decisions::DecisionPrivateBinding> {
    match binding {
        DecisionBindingV1::PrivateOperation(private) => Some(private),
        _ => None,
    }
}
