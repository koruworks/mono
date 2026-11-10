// SPDX-License-Identifier: MPL-2.0

//! The sync side: what a run means on every device (D14).
//!
//! A [`RunHeader`] is what the Run document carries — objective, agent,
//! scope, context as given, status, executing device, artifact references,
//! the economics summary and the rating — and a [`DelegatedRun`] is the
//! header with its timeline. Every field here syncs (D3) and every device
//! reads it; only the executing device writes it (D15).
//!
//! **Nothing here says how the attempt ran.** The provider session key, the
//! process facts and the pending publications are [`crate::AttemptState`]'s,
//! and there is deliberately no `From` in either direction: the two types
//! share a [`RunId`] and nothing else, so a field cannot land on the wrong
//! side by conversion.
//!
//! **Serde tolerates what it does not know.** No type on this side uses
//! `deny_unknown_fields`, and the fields a later build added carry
//! `#[serde(default)]`: a header written by a newer build must open on an
//! older one. Preserving those unknown fields across a *save* is not this
//! struct's job — the Run document is a Loro map and P1.4's representation
//! keeps keys it does not read — but refusing to read them would be.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    ActorRef, ArtifactTitle, ArtifactVersionId, DelegationRequest, DeviceId, EconomicsSummary,
    ExecutionEnvironment, Objective, RatingNote, RunContextItem, RunId, RunOrigin, RunStatus,
    SessionProfileKey, TimelineEntry, WorkspaceId, WorkspaceScope,
};

/// One gesture on a finished run (D44): good or not good. An unrated run is
/// a normal state, which is why the header holds an `Option<RunRating>` and
/// this enum has no "unrated" arm.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    Good,
    NotGood,
}

/// A rating as recorded: the gesture, the optional note, who and when.
///
/// The note lives here — in the run's own content — and nowhere else (D44).
/// `delegate.run.rated` carries the rating and the ids; a metric carries the
/// rating; neither ever carries the note.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunRating {
    pub rating: Rating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<RatingNote>,
    pub rated_by: ActorRef,
    pub rated_at_ms: i64,
}

/// A result, named (D27).
///
/// The fields a device that cannot open the bytes still shows: what it is,
/// how big, when, and which device holds it. Opening begins working when
/// replication ships (M7); until then a second device reads this and says
/// the content is elsewhere.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ResultReference {
    pub artifact: ArtifactVersionId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub held_by: DeviceId,
    pub title: ArtifactTitle,
    pub size_bytes: u64,
    pub registered_at_ms: i64,
}

/// What the Run document carries (D14).
///
/// There is no conversion to or from [`crate::AttemptState`], and these two
/// doctests keep it that way: the day someone writes the `From`, they fail.
///
/// ```compile_fail
/// use koru_delegate_core::{AttemptState, RunHeader};
/// fn sync_side(state: AttemptState) -> RunHeader { state.into() }
/// ```
///
/// ```compile_fail
/// use koru_delegate_core::{AttemptState, RunHeader};
/// fn execution_side(header: RunHeader) -> AttemptState { header.into() }
/// ```
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<crate::BotProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_results: Vec<crate::NativeResultReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_delivery: Option<crate::BotDeliveryReceipt>,
    pub run: RunId,
    pub created_at_ms: i64,
    pub requested_by: ActorRef,
    pub objective: Objective,
    pub agent: SessionProfileKey,
    /// The person's explicit choice; absent means the provider default.
    /// Retained intent, never proof of the model the provider actually used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<crate::ModelName>,
    pub scope: WorkspaceScope,
    /// Context as given — pinned, and unchanged by anything that happens to
    /// the source afterwards (§7.2).
    pub context: Vec<RunContextItem>,
    /// The grant set (D28). A resume re-reads it and never widens it.
    #[serde(default)]
    pub grants: BTreeSet<WorkspaceId>,
    /// The one canonical status (§7.1).
    pub status: RunStatus,
    /// Safe terminal cause, absent for older runs. Raw provider text is never retained here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<crate::RunFailureReason>,
    pub environment: ExecutionEnvironment,
    /// Every result registered, in registration order.
    #[serde(default)]
    pub artifacts: Vec<ResultReference>,
    /// A projection of the retained usage facts and the timeline (§17.1),
    /// recomputable; kept here so a device reads the numbers without
    /// replaying.
    #[serde(default)]
    pub economics: EconomicsSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<RunRating>,
    /// Durable provenance copied from the creation intent. Additive and
    /// defaulted so runs written before scheduling remain manual.
    #[serde(default)]
    pub origin: RunOrigin,
}

impl RunHeader {
    /// Local shape validation, not authorization. A transported provenance is
    /// historical evidence; the trusted execution owner still admits all work.
    pub fn bot_fields_valid(&self) -> bool {
        let Some(bot) = &self.bot else {
            return self.native_results.is_empty()
                && self.chat_delivery.is_none()
                && !matches!(self.scope, WorkspaceScope::BotFolder { .. });
        };
        bot.valid()
            && matches!(self.scope, WorkspaceScope::BotFolder { .. })
            && self
                .native_results
                .iter()
                .all(crate::NativeResultReference::valid)
            && self
                .native_results
                .iter()
                .map(|r| r.operation_id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.native_results.len()
            && self.chat_delivery.as_ref().is_none_or(|receipt| {
                receipt.valid()
                    && receipt.run == self.run
                    && receipt.chat_id == bot.chat_id
                    && bot.ai_attempt_id == Some(receipt.ai_attempt_id)
                    && bot.assistant_message_id == Some(receipt.assistant_message_id)
            })
    }

    /// The header the Run document is committed with (§7.1 step 3): the
    /// request as given, `Created`, nothing produced yet.
    pub fn from_request(request: DelegationRequest, created_at_ms: i64) -> Self {
        Self {
            bot: None,
            native_results: Vec::new(),
            chat_delivery: None,
            run: request.run,
            created_at_ms,
            requested_by: request.requested_by,
            objective: request.objective,
            agent: request.agent,
            requested_model: request.requested_model,
            scope: request.scope,
            context: request.context,
            grants: request.grants,
            status: RunStatus::Created,
            failure_reason: None,
            environment: ExecutionEnvironment::local(request.executing_device),
            artifacts: Vec::new(),
            economics: EconomicsSummary::default(),
            rating: None,
            origin: request.origin,
        }
    }

    /// The device executing the run, when the environment names one.
    pub fn executing_device(&self) -> Option<DeviceId> {
        self.environment.device()
    }
}

/// A run as a device reads it: the header and every step of its history
/// (D32: all of it, everywhere, retained). Whether the timeline is stored
/// inside the same document or paged beside it is P1.0's to establish
/// (§7.9); this type is the *read* shape either way.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DelegatedRun {
    pub header: RunHeader,
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
}
