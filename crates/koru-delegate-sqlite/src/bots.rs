// SPDX-License-Identifier: MPL-2.0

//! Bot admission vocabulary and the single-writer ledger contract. These
//! records are local reconciliation evidence, never canonical Run status.
use crate::LedgerError;
mod repository;
pub(crate) mod routines;
pub use routines::RoutinePolicyRecord;
pub use routines::{
    RoutineHeldCursor, RoutineOccurrenceFact, RoutinePage, RoutineQueuedCursor,
    RoutineRecoveryEntry, RoutineSlotCursor,
};
#[cfg(test)]
mod tests;
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, BotAdmissionBarrier, BotAdmissionState, BotBootPage,
    BotDeliveryReceipt, BotId, BotOwner, BotRequestId, BotRunKey, BotSubmission, BotSubmissionKey,
    IdempotencyKey, RunAttemptId, RunId,
};

pub const BOT_ACTIVE_BUDGET_MS: u64 = 600_000;
pub const BOT_PENDING_LIMIT: usize = 50;
pub const BOT_HOST_EXECUTION_LIMIT: usize = 2;

#[derive(Clone, PartialEq, Eq)]
pub struct BotInitialSend {
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub key: IdempotencyKey,
    pub fingerprint: [u8; 32],
    pub turn: Option<AgentTurnId>,
}
impl std::fmt::Debug for BotInitialSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotInitialSend")
            .field("attempt", &self.attempt)
            .field("generation", &self.generation)
            .field("turn", &self.turn)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotAdmissionRecord {
    pub submission_version: u8,
    pub submission: BotSubmission,
    pub sequence: u64,
    pub state: BotAdmissionState,
    pub active_elapsed_ms: u64,
    pub active_since_ms: Option<i64>,
    pub initial_send: Option<BotInitialSend>,
    pub delivery: Option<BotDeliveryReceipt>,
    pub delivery_publication: Option<koru_delegate_core::EventId>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Bounded column-only projection for observation/control and active budgets.
/// It contains no task, context, policy body, or transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotExecutionRecord {
    pub run: RunId,
    pub owner: BotOwner,
    pub bot: BotId,
    pub request: BotRequestId,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub sequence: u64,
    pub state: BotAdmissionState,
    pub active_elapsed_ms: u64,
    pub active_since_ms: Option<i64>,
    pub initial_send: Option<BotInitialSend>,
}
impl BotExecutionRecord {
    pub fn active_elapsed_at(&self, now_ms: i64) -> u64 {
        active_elapsed(self.active_elapsed_ms, self.active_since_ms, now_ms)
    }
}
impl BotAdmissionRecord {
    pub fn execution(&self) -> BotExecutionRecord {
        let p = &self.submission.provenance;
        BotExecutionRecord {
            run: self.submission.request.run,
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
            config_revision: p.config_revision,
            policy_revision: p.policy_revision,
            sequence: self.sequence,
            state: self.state,
            active_elapsed_ms: self.active_elapsed_ms,
            active_since_ms: self.active_since_ms,
            initial_send: self.initial_send.clone(),
        }
    }
}
fn active_elapsed(elapsed: u64, since: Option<i64>, now: i64) -> u64 {
    elapsed.saturating_add(since.map_or(0, |since| {
        now.checked_sub(since)
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(BOT_ACTIVE_BUDGET_MS)
    }))
}

/// Queue scans never load the bounded-but-large current task payload. The
/// actor loads one full admission only after choosing a dispatch candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedBotAdmission {
    pub run: RunId,
    pub owner: BotOwner,
    pub bot: BotId,
    pub sequence: u64,
    pub config_revision: u64,
    pub policy_revision: u64,
}
impl BotAdmissionRecord {
    /// A clock moving backwards cannot manufacture more trial budget. An
    /// uncertain open interval is conservatively exhausted, not reset.
    pub fn active_elapsed_at(&self, now_ms: i64) -> u64 {
        active_elapsed(self.active_elapsed_ms, self.active_since_ms, now_ms)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotReservation {
    Acquired,
    AlreadyOwned,
    Paused,
    BotBusy,
    HostBusy,
    Settled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotSettlement {
    Settled,
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotLedgerError {
    Ledger(LedgerError),
    Invalid,
    Conflict,
    Capacity,
    Stale,
    Fenced,
}
impl From<LedgerError> for BotLedgerError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}
impl std::fmt::Display for BotLedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(error) => error.fmt(f),
            Self::Invalid => f.write_str("invalid Bot admission"),
            Self::Conflict => f.write_str("Bot request payload conflicts"),
            Self::Capacity => f.write_str("Bot pending capacity reached"),
            Self::Stale => f.write_str("stale Bot revision"),
            Self::Fenced => f.write_str("Bot execution is fenced"),
        }
    }
}
impl std::error::Error for BotLedgerError {}

/// Implemented by the existing RunsDb connection, never another writer.
/// Authentication happens before this port. Each mutation is one existing
/// crash-injectable IMMEDIATE transaction, including its idempotency lookup.
pub trait BotLedger {
    fn read_routine_claim(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<koru_delegate_core::RoutineClaimObservation, BotLedgerError>;
    fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &koru_delegate_core::RoutineLinkAcknowledgement,
    ) -> Result<koru_delegate_core::RoutineClaimRecord, BotLedgerError>;
    /// Retain a deny-only route. Exact retries return the immutable original;
    /// changed payloads conflict. No execution authority is granted.
    fn register_routine_route(
        &mut self,
        identity: &koru_delegate_core::RoutineRouteIdentity,
    ) -> Result<koru_delegate_core::RoutineRouteIdentity, BotLedgerError>;
    fn stop_unstarted_routine(
        &mut self,
        key: &BotRunKey,
    ) -> Result<Option<koru_delegate_core::RoutineClaimRecord>, BotLedgerError>;
    /// At most sixteen unsettled admission identities, never payloads. The
    /// high-water mark bounds a scan, not snapshot membership: purging settled
    /// rows can allow sequence reuse. New submissions need their own replay.
    fn bot_boot_inventory(
        &self,
        owner: BotOwner,
        after: u64,
        through: Option<u64>,
    ) -> Result<BotBootPage, BotLedgerError>;
    fn bot_has_unsettled(&self, owner: BotOwner, bot: BotId) -> Result<bool, BotLedgerError>;
    /// Release only a reservation with no attempt, send, delivery, accounting or
    /// native effect evidence. Unknown evidence leaves capacity owned.
    fn recover_bot_reservation(&mut self, key: &BotRunKey) -> Result<bool, BotLedgerError>;
    /// Bounded payload-free projection of the two reserved/executing host
    /// slots. Retained reservations remain eligible for reconciliation even
    /// when no additional queued admission can acquire capacity.
    fn active_bot_executions(
        &self,
        device: koru_delegate_core::DeviceId,
    ) -> Result<Vec<BotExecutionRecord>, BotLedgerError>;
    /// Atomically insert the ordinary Run intent and Bot admission. Exact
    /// retries return the original row; changed full typed payload conflicts.
    /// Enforce 50 queued jobs/Bot before inserting either row; a rejected call
    /// leaves neither runnable intent nor reservation. A paused/newer barrier
    /// rejects new admission; old accepted queued jobs keep their policy.
    fn admit_bot(
        &mut self,
        submission: &BotSubmission,
    ) -> Result<BotAdmissionRecord, BotLedgerError>;
    fn bot_admission(&self, run: RunId) -> Result<Option<BotAdmissionRecord>, BotLedgerError>;
    fn bot_execution(&self, run: RunId) -> Result<Option<BotExecutionRecord>, BotLedgerError>;
    fn reconcile_bot_execution(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotExecutionRecord>, BotLedgerError>;

    fn reconcile_bot(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotAdmissionRecord>, BotLedgerError>;
    /// Ordered, bounded dispatch candidates (queued or reserved before a
    /// crash); limit is clamped to 1..=1000. No task payload. The actor must
    /// reconcile the journal before dispatching a recovered reservation;
    /// executing/unknown work is never offered for a new launch.
    fn queued_bots(
        &self,
        after: u64,
        limit: u16,
    ) -> Result<Vec<QueuedBotAdmission>, BotLedgerError>;
    /// One Bot's unsettled admissions, payload-free: its reserved or
    /// executing one (at most one, through `bot_admissions_execution`), then
    /// up to `limit` (clamped to 1..=64) queued ones in queue order, through
    /// `bot_admissions_queue`. Settled history is never walked. The flag is
    /// whether more queued admissions exist past the page.
    fn unsettled_bot(
        &self,
        owner: BotOwner,
        bot: BotId,
        limit: u16,
    ) -> Result<(Vec<BotExecutionRecord>, bool), BotLedgerError>;
    fn bot_barrier(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<BotAdmissionBarrier>, BotLedgerError>;
    /// Compare the complete typed tuple as an exact semantic payload binding.
    /// Same epoch + altered tuple conflicts, lower epoch is stale; persist
    /// before ack. A barrier needs no placeholder Run to exist.
    fn apply_bot_barrier(
        &mut self,
        barrier: &BotAdmissionBarrier,
    ) -> Result<BotAdmissionBarrier, BotLedgerError>;
    /// Atomic one/Bot and two/device reservation. Unknown executions retain
    /// their reserved/executing row; an old queued revision may dispatch
    /// after resume without widening its captured policy.
    fn reserve_bot(&mut self, run: RunId) -> Result<BotReservation, BotLedgerError>;
    /// Require the journal's current attempt/generation. Starts accounting
    /// before provider work; waiting for initial AI input is bounded too.
    fn activate_bot(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
    ) -> Result<(), BotLedgerError>;
    /// Charge the previous open interval before changing it. Native approval
    /// wait passes false; resume passes true. Restart never zeros elapsed.
    fn set_bot_active(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        active: bool,
    ) -> Result<(), BotLedgerError>;
    /// Release only after the journal proves the latest attempt retired, or
    /// no attempt was ever opened. Unknown/merely-stopping cannot release.
    fn settle_bot(&mut self, run: RunId, outcome: BotSettlement) -> Result<(), BotLedgerError>;
    /// Persist first-send identity/digest before forwarding transient input.
    /// Exact duplicate reconciles; changed key/hash/attempt is fenced. The
    /// native input idempotency owner resolves a lost acknowledgement.
    /// A Bot Run never opens a fresh generation after interruption: explicit
    /// Continue creates a new linked Run. Recovery keeps this immutable tuple.
    /// Preparation always offers turn=None; after acknowledgement, the same
    /// immutable key/digest/attempt tuple returns the retained Some(turn).
    fn prepare_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError>;
    fn acknowledge_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError>;
    /// Retain durable AI response delivery and its stable completion publication
    /// identity atomically. Exact correlation/provenance must match. Migrated
    /// confirmed rows allocate a missing identity only on exact receipt replay.
    /// No message contents travel here.
    fn record_bot_delivery(
        &mut self,
        receipt: &BotDeliveryReceipt,
    ) -> Result<koru_delegate_core::EventId, BotLedgerError>;
}
