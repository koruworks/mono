// SPDX-License-Identifier: MPL-2.0

//! The recoverable lifecycle journal (ai.db 18): one intent per operation,
//! its captured members, and the barrier that holds the subject still.
//!
//! Capture is one AI transaction: the owner/revision fence, the intent, the
//! members and, for Archive/Trash of an active Bot, the existing pause
//! receipt. No Store or delegate call happens here. Child request IDs for
//! the delegate and Store owners are minted at capture and stored, never
//! derived from the operation (AI identities are never derived).
use super::*;
use koru_ai_core::ChatAttachmentId;
use koru_delegate_core::NativePolicyRequestId;
use rusqlite::TransactionBehavior;
use uuid::Uuid;

/// A member as the caller proposes it at capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleMemberDraft {
    pub subject: BotLifecycleSubject,
    pub reason: BotLifecycleReason,
    pub prior_state: BotLifecyclePriorState,
    pub intended: BotLifecycleIntended,
    /// `Retained` (with its reason) when capture already knows it is kept.
    pub retained: Option<BotLifecycleRetained>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleMember {
    pub ordinal: u32,
    pub subject: BotLifecycleSubject,
    pub reason: BotLifecycleReason,
    pub prior_state: BotLifecyclePriorState,
    pub intended: BotLifecycleIntended,
    pub stage: BotLifecycleMemberStage,
    pub retained: Option<BotLifecycleRetained>,
    pub child_request: Option<Uuid>,
    pub updated_at_ms: i64,
}

/// One requested lifecycle operation. Authority is checked by the caller
/// before this is offered; the transaction rechecks owner and revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleCapture {
    pub operation: BotRequestId,
    pub owner: BotOwner,
    pub bot: BotId,
    pub kind: BotLifecycleKind,
    pub chat: Option<ChatId>,
    pub fingerprint: String,
    pub expected_revision: u64,
    pub source: Option<BotRequestId>,
    pub members: Vec<LifecycleMemberDraft>,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleOperation {
    pub operation: BotRequestId,
    pub owner: BotOwner,
    pub bot: BotId,
    pub kind: BotLifecycleKind,
    pub chat: Option<ChatId>,
    pub fingerprint: String,
    pub expected_revision: u64,
    pub barrier_revision: Option<u64>,
    pub source: Option<BotRequestId>,
    pub prior_lifecycle: BotLifecycle,
    pub pause_request: Option<BotRequestId>,
    pub policy_request: Option<NativePolicyRequestId>,
    pub policy_barrier: Option<BotLifecycleBarrierState>,
    pub stage: BotLifecycleStage,
    pub refusal: Option<BotLifecycleRefusal>,
    pub requested_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub members: Vec<LifecycleMember>,
}

impl LifecycleOperation {
    /// The public result: progress and each member's outcome, without the
    /// owner correlation (child request, pause and policy IDs) it is built from.
    pub fn progress(&self) -> BotLifecycleProgress {
        BotLifecycleProgress {
            request_id: self.operation,
            bot_id: self.bot,
            kind: self.kind,
            chat_id: self.chat,
            source_request_id: self.source,
            stage: self.stage,
            refusal: self.refusal,
            members: self
                .members
                .iter()
                .map(|member| BotLifecycleMemberOutcome {
                    subject: member.subject.clone(),
                    intended: member.intended,
                    stage: member.stage,
                    retained: member.retained,
                })
                .collect(),
            left_in_place: BotLeftInPlace::of(self.kind),
        }
    }
}

/// Which member kinds an operation may capture. Restore acts on its source
/// operation's members and captures none of its own.
fn allowed_members(kind: BotLifecycleKind) -> &'static [BotLifecycleMemberKind] {
    use BotLifecycleMemberKind as M;
    match kind {
        // Archive and Trash pause the Bot's Routines (ai.db 23).
        BotLifecycleKind::Archive => &[M::Bot, M::Chat, M::Routine],
        BotLifecycleKind::Trash => {
            &[M::Bot, M::Chat, M::RunReference, M::OwnedRole, M::Routine]
        }
        BotLifecycleKind::TrashChat => &[M::Chat, M::RunReference],
        BotLifecycleKind::Restore | BotLifecycleKind::RestoreChat => &[],
        BotLifecycleKind::PurgeBot => &[M::Bot, M::Chat, M::AttachmentCopy],
        BotLifecycleKind::PurgeChat => &[M::Chat, M::AttachmentCopy],
    }
}

/// The Bot lifecycle each operation may start from.
fn allowed_prior(kind: BotLifecycleKind, prior: BotLifecycle) -> bool {
    use BotLifecycle::*;
    match kind {
        BotLifecycleKind::Archive => prior == Active,
        BotLifecycleKind::Trash => matches!(prior, Active | Archived),
        BotLifecycleKind::TrashChat | BotLifecycleKind::RestoreChat => {
            matches!(prior, Active | Archived)
        }
        // An incomplete Trash may leave the Bot active and paused.
        BotLifecycleKind::Restore => true,
        BotLifecycleKind::PurgeBot => prior == Trashed,
        BotLifecycleKind::PurgeChat => matches!(prior, Active | Archived),
    }
}

/// The operation a reversal or purge must name, and the stages it may be in.
fn source_rule(kind: BotLifecycleKind) -> Option<(&'static [BotLifecycleKind], bool)> {
    use BotLifecycleKind::*;
    // (accepted source kinds, whether an incomplete source is accepted)
    match kind {
        Restore => Some((&[Archive, Trash], true)),
        RestoreChat => Some((&[TrashChat], true)),
        PurgeBot => Some((&[Trash], false)),
        PurgeChat => Some((&[TrashChat], false)),
        _ => None,
    }
}

impl Repository {
    /// Capture an operation, or return the one already captured under the
    /// same identity. A changed request under that identity conflicts.
    pub fn capture_bot_lifecycle(
        &mut self,
        capture: &LifecycleCapture,
    ) -> Result<LifecycleOperation, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let (operation, _) = capture_on(&tx, capture)?;
        tx.commit().map_err(bad)?;
        Ok(operation)
    }
}

/// Capture inside the caller's transaction. The flag says whether this call
/// wrote the operation (`false`: an exact repeat found the stored one).
fn capture_on(
    tx: &Connection,
    capture: &LifecycleCapture,
) -> Result<(LifecycleOperation, bool), BotError> {
    validate_capture(capture)?;
    if let Some(existing) = read_operation(tx, capture.owner.user_scope, capture.operation)? {
        return if existing.bot == capture.bot
            && existing.fingerprint == capture.fingerprint
            && existing.owner == capture.owner
        {
            Ok((existing, false))
        } else {
            Err(BotError::Conflict)
        };
    }
    let (current, prior) = admission::guard_any_lifecycle(tx, capture.owner, capture.bot)?;
    if !allowed_prior(capture.kind, prior) {
        return Err(BotError::Conflict);
    }
    if let Some(chat) = capture.chat
        && bot_for_chat_on(&tx, chat)? != Some(capture.bot)
    {
        return Err(BotError::Forbidden);
    }
    require_no_open_operation(&tx, capture)?;
    if let Some(source) = capture.source {
        require_source(&tx, capture, source)?;
    }
    let pauses = capture.kind.pauses(prior);
    let (pause_request, policy_request, barrier_revision) = if pauses {
        let request = BotRequestId::mint();
        let pause = admission::prepare_bot_pause_on(
            &tx,
            capture.owner,
            capture.bot,
            request,
            capture.expected_revision,
            true,
            &capture.fingerprint,
            capture.now,
        )?;
        // Work not admitted when it is captured stays refused for good,
        // behind this pause, whatever later reverses the operation.
        admission::refuse_captured_on(
            tx,
            capture.owner,
            capture.bot,
            None,
            Some(&pause.intent.barrier),
            capture.now,
        )?;
        (
            Some(request),
            Some(NativePolicyRequestId::from(BotRequestId::mint())),
            Some(capture.expected_revision + 1),
        )
    } else {
        if current.revision != capture.expected_revision {
            return Err(BotError::RevisionConflict);
        }
        // A conversation's Trash refuses that conversation's work not yet
        // admitted, behind one fence of its own.
        if capture.kind == BotLifecycleKind::TrashChat {
            admission::refuse_captured_on(
                tx,
                capture.owner,
                capture.bot,
                capture.chat,
                None,
                capture.now,
            )?;
        }
        (None, None, None)
    };
    tx.execute(
            "INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,chat_id,fingerprint,expected_revision,barrier_revision,source_operation_id,prior_lifecycle,pause_request_id,policy_request_id,policy_barrier_state,stage,refusal,requested_at_ms,updated_at_ms,completed_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,NULL,?16,?16,NULL)",
            params![
                capture.owner.user_scope.to_storage(),
                capture.operation.to_storage(),
                capture.bot.to_storage(),
                capture.owner.device.to_storage(),
                capture.kind.token(),
                capture.chat.map(ChatId::to_storage),
                capture.fingerprint,
                capture.expected_revision,
                barrier_revision,
                capture.source.map(BotRequestId::to_storage),
                prior.token(),
                pause_request.map(BotRequestId::to_storage),
                policy_request.map(NativePolicyRequestId::to_storage),
                policy_request.map(|_| BotLifecycleBarrierState::Pending.token()),
                BotLifecycleStage::Captured.token(),
                capture.now,
            ],
        )
        .map_err(bad)?;
    for (ordinal, member) in capture.members.iter().enumerate() {
        insert_member(&tx, capture, ordinal, member)?;
    }
    let operation = read_operation(tx, capture.owner.user_scope, capture.operation)?
        .ok_or(BotError::Storage)?;
    Ok((operation, true))
}

impl Repository {
    pub fn bot_lifecycle_operation(
        &self,
        scope: UserScopeId,
        operation: BotRequestId,
    ) -> Result<Option<LifecycleOperation>, BotError> {
        read_operation(&self.connection, scope, operation)
    }

    /// The unfinished operation (moving or parked) holding a conversation still.
    pub fn open_chat_lifecycle(
        &self,
        chat: ChatId,
    ) -> Result<Option<(UserScopeId, BotRequestId)>, BotError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(&open_chat_sql(), [chat.to_storage()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(bad)?;
        row.map(|(scope, operation)| {
            Ok((
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                operation.parse().map_err(bad)?,
            ))
        })
        .transpose()
    }

    /// One bounded page of moving operations to resume after a restart,
    /// oldest progress first. `after` is the last page's final position.
    pub fn resumable_bot_lifecycle(
        &self,
        after: Option<(i64, BotRequestId)>,
        limit: u16,
    ) -> Result<Vec<(i64, UserScopeId, BotRequestId)>, BotError> {
        let (at, operation) = after
            .map(|(at, operation)| (at, operation.to_storage()))
            .unwrap_or((i64::MIN, String::new()));
        let mut query = self.connection.prepare(RESUMABLE_SQL).map_err(bad)?;
        let rows = query
            .query_map(params![at, operation, limit.clamp(1, 64)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (at, scope, operation) = row.map_err(bad)?;
            Ok((
                at,
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                operation.parse().map_err(bad)?,
            ))
        })
        .collect()
    }

    /// Up to `limit` driven operations (Archive, Trash, a conversation's
    /// Trash) parked because their owner did not answer, in
    /// `bot_lifecycle_open`'s order (Bot, then row), starting just past
    /// `after`: the ones that take up their work again once it answers.
    /// Each comes with its own position, to pass back as `after` so a caller
    /// can walk them all in turn. Read through `bot_lifecycle_open`, which
    /// holds only unfinished operations, never completed history.
    pub fn parked_for_owner_bot_lifecycle(
        &self,
        after: Option<&ParkedCursor>,
        limit: u16,
    ) -> Result<Vec<ParkedForOwner>, BotError> {
        let mut query = self
            .connection
            .prepare(&parked_for_owner_sql())
            .map_err(bad)?;
        // Every Bot id sorts after the empty string.
        let (bot, row) = after.map_or((String::new(), i64::MIN), |cursor| {
            (cursor.bot.clone(), cursor.row)
        });
        let rows = query
            .query_map(params![bot, row, limit.clamp(1, 64)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (scope, operation, bot, row) = row.map_err(bad)?;
            Ok(ParkedForOwner {
                scope: UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                operation: operation.parse().map_err(bad)?,
                bot: bot.parse().map_err(bad)?,
                cursor: ParkedCursor { bot, row },
            })
        })
        .collect()
    }
}

/// One operation parked for its owner, from
/// [`Repository::parked_for_owner_bot_lifecycle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkedForOwner {
    pub scope: UserScopeId,
    pub operation: BotRequestId,
    pub bot: BotId,
    /// Where a page that should start after this operation starts.
    pub cursor: ParkedCursor,
}

/// A position in the operations parked for their owner, opaque to callers:
/// a Bot's id and the operation's row, the order `bot_lifecycle_open` keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkedCursor {
    bot: String,
    row: i64,
}

/// [`Repository::parked_for_owner_bot_lifecycle`]'s statement. The
/// `(bot_id, rowid)` cursor is the partial index's own order (rowid is its
/// implicit last column), so the page is a range of the index, never sorted.
pub(crate) fn parked_for_owner_sql() -> String {
    format!(
        "SELECT user_scope_id,operation_id,bot_id,i.rowid FROM bot_lifecycle_intents i INDEXED BY bot_lifecycle_open
         WHERE (i.bot_id,i.rowid)>(?1,?2) AND i.stage='incomplete' AND i.refusal='owner_unavailable'
           AND i.kind IN ('archive','trash','trash_chat') AND {OPEN}
         ORDER BY i.bot_id,i.rowid LIMIT ?3"
    )
}

/// An unfinished operation, moving or parked. One a completed Restore
/// reversed reads `reversed` (the Restore's transaction wrote it), so it is
/// not open and holds nothing. This is exactly the predicate of
/// `bot_lifecycle_open` and `bot_lifecycle_chat_open`, which SQLite needs
/// word for word to read either index. `i` is the intent row.
pub(crate) const OPEN: &str = "i.stage NOT IN ('completed','reversed')";

/// [`Repository::resumable_bot_lifecycle`]'s statement: the moving
/// operations, through `bot_lifecycle_resume`, whose predicate it repeats.
pub(crate) const RESUMABLE_SQL: &str =
    "SELECT updated_at_ms,user_scope_id,operation_id FROM bot_lifecycle_intents
     WHERE stage NOT IN ('completed','incomplete','reversed')
       AND (updated_at_ms>?1 OR (updated_at_ms=?1 AND operation_id>?2))
     ORDER BY updated_at_ms,operation_id LIMIT ?3";

// The reads of unfinished operations, each one statement whose query plan
// `tests/plans.rs` pins by index name. None asks whether a Restore names
// the operation: its stage says so.

/// [`Repository::open_chat_lifecycle`]: through `bot_lifecycle_chat_open`.
pub(crate) fn open_chat_sql() -> String {
    format!(
        "SELECT user_scope_id,operation_id FROM bot_lifecycle_intents i WHERE chat_id=?1 AND {OPEN}"
    )
}

/// [`whole_bot_operation_open`]: through `bot_lifecycle_open`.
pub(crate) fn whole_bot_open_sql() -> String {
    format!(
        "SELECT EXISTS(SELECT 1 FROM bot_lifecycle_intents i WHERE bot_id=?1 AND chat_id IS NULL AND {OPEN})"
    )
}

/// [`Repository::open_bot_lifecycle`]: through `bot_lifecycle_open`.
pub(crate) fn open_bot_barrier_sql() -> String {
    format!(
        "SELECT user_scope_id,operation_id FROM bot_lifecycle_intents i
         WHERE bot_id=?1 AND chat_id IS NULL AND policy_request_id IS NOT NULL AND {OPEN}
         ORDER BY requested_at_ms DESC LIMIT 1"
    )
}

/// [`Repository::bot_hold`]'s whole-Bot part: through `bot_lifecycle_open`.
pub(crate) fn bot_hold_sql() -> String {
    format!(
        "SELECT EXISTS(SELECT 1 FROM bots WHERE bot_id=?1 AND lifecycle='trashed'),
            EXISTS(SELECT 1 FROM bot_lifecycle_intents i WHERE bot_id=?1 AND chat_id IS NULL AND i.stage<>'incomplete' AND {OPEN}),
            EXISTS(SELECT 1 FROM bot_lifecycle_intents i WHERE bot_id=?1 AND chat_id IS NULL AND i.stage='incomplete' AND {OPEN})"
    )
}

/// [`Repository::bot_open_lifecycle`]: through `bot_lifecycle_open`, named.
pub(crate) fn bot_open_sql() -> String {
    format!(
        "SELECT user_scope_id,operation_id FROM bot_lifecycle_intents i INDEXED BY bot_lifecycle_open
         WHERE bot_id=?1 AND {OPEN} ORDER BY requested_at_ms DESC,operation_id DESC LIMIT 1"
    )
}

/// [`Repository::bot_chat_hold`]'s own part: through `bot_lifecycle_chat_open`.
pub(crate) fn chat_hold_sql() -> String {
    format!(
        "SELECT EXISTS(SELECT 1 FROM bot_chat_trash WHERE chat_id=?1),
            EXISTS(SELECT 1 FROM bot_lifecycle_intents i WHERE chat_id=?1 AND i.stage<>'incomplete' AND {OPEN}),
            EXISTS(SELECT 1 FROM bot_lifecycle_intents i WHERE chat_id=?1 AND i.stage='incomplete' AND {OPEN})"
    )
}

/// The open-operation rule at capture: through `bot_lifecycle_open`.
pub(crate) fn open_operations_sql() -> String {
    format!(
        "SELECT user_scope_id,operation_id,stage FROM bot_lifecycle_intents i WHERE bot_id=?1 AND {OPEN}"
    )
}

/// Whether a whole-Bot operation (Archive, Trash) of this Bot is unfinished
/// and unreversed, moving or parked. While one is, the only writes that may
/// move the Bot's revision are the refusal fences of its own settlement
/// (`prepare_bot_refusal_fence`) and its own transactions: a configuration,
/// access, pause or resume change is refused here as well as by the hold.
pub(super) fn whole_bot_operation_open(conn: &Connection, bot: BotId) -> Result<bool, BotError> {
    // A database is always migrated before it is written, so the journal is
    // there; only the populated ai17 fixture (`Repository::at_schema(17)`)
    // runs these writers on a schema that predates it, and nothing can be
    // open there.
    let journal: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='bot_lifecycle_intents')",
            [],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if !journal {
        return Ok(false);
    }
    conn.query_row(&whole_bot_open_sql(), [bot.to_storage()], |r| r.get(0))
        .map_err(bad)
}

/// The revision rule of a whole-Bot operation's completion: the Bot's
/// revision may have moved past the operation's barrier only through the
/// refusal fences of the operation's own settlement, which refuse the work it
/// found not yet admitted. Every other writer of the revision is refused
/// while the operation is open ([`whole_bot_operation_open`]), so a revision
/// at or past the barrier proves nobody else changed the Bot; one below it
/// cannot happen and is a conflict. A parked `revision_changed` therefore
/// resumes to completion when nothing but the operation moved it.
fn revision_held(current: u64, barrier: Option<u64>) -> bool {
    barrier.is_some_and(|barrier| current >= barrier)
}

/// What the delegate's schedule coordinator answered when asked to pause one
/// captured Routine. Only its answer settles a Routine member: the driver's
/// own read of the Schedule never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleRoutineAnswer {
    /// The coordinator paused it, or found it already paused.
    Paused,
    /// The coordinator found no live Schedule: it was deleted or retired.
    Gone,
    /// Another device owns it, a handoff is pending, or no answer came.
    Unavailable,
}

/// A whole-Bot Archive or Trash completes only once every captured Routine
/// is confirmed paused (`acknowledged`) or kept (`retained`). A Routine that
/// is still `pending` or `unavailable` may still be on.
fn require_routines_settled(
    tx: &Connection,
    scope: UserScopeId,
    operation: BotRequestId,
) -> Result<(), BotError> {
    let unsettled: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_lifecycle_members WHERE user_scope_id=?1 AND operation_id=?2
             AND kind='routine' AND stage NOT IN ('acknowledged','retained'))",
            params![scope.to_storage(), operation.to_storage()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if unsettled {
        return Err(BotError::Conflict);
    }
    Ok(())
}

/// The driver's writes (P1.7 S4). Each is one short transaction on the AI
/// writer; every owner call happens before or after, never inside.
impl Repository {
    /// Every Routine of a Bot, with its home and whether its creation is
    /// bound: what a whole-Bot Archive or Trash captures. Column-only; no
    /// seed or grant is read.
    pub fn lifecycle_bot_routines(
        &self,
        bot: BotId,
    ) -> Result<Vec<(koru_delegate_core::ScheduleId, WorkspaceId, bool)>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT CASE WHEN typeof(schedule_id)='text' AND length(CAST(schedule_id AS BLOB))=36 THEN schedule_id END,
                 CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,
                 creation_state='bound' FROM bot_routines WHERE bot_id=?1 ORDER BY schedule_id",
            )
            .map_err(bad)?;
        let rows = query
            .query_map([bot.to_storage()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (schedule, workspace, bound) = row.map_err(bad)?;
            Ok((
                schedule.parse().map_err(bad)?,
                WorkspaceId::parse(&workspace).ok_or(BotError::Storage)?,
                bound,
            ))
        })
        .collect()
    }

    /// Record the coordinator's answer for one captured Routine of a moving
    /// Archive or Trash. `Paused` acknowledges it; `Gone` keeps it as
    /// already trashed; `Unavailable` leaves it for a retry, and the caller
    /// parks the operation. A member that is already acknowledged or
    /// retained is final and is returned as stored: a late answer never
    /// turns a confirmed pause back into a doubt.
    pub fn settle_bot_lifecycle_routine(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        document: DocumentId,
        answer: LifecycleRoutineAnswer,
        now: i64,
    ) -> Result<LifecycleMember, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let op = read_operation(&tx, scope, operation)?.ok_or(BotError::NotFound)?;
        let member = |op: &LifecycleOperation| {
            op.members
                .iter()
                .find(|member| {
                    matches!(&member.subject, BotLifecycleSubject::Routine { document_id, .. }
                        if *document_id == document)
                })
                .cloned()
        };
        let stored = member(&op).ok_or(BotError::NotFound)?;
        if matches!(
            stored.stage,
            BotLifecycleMemberStage::Acknowledged | BotLifecycleMemberStage::Retained
        ) {
            return Ok(stored);
        }
        if !matches!(op.kind, BotLifecycleKind::Archive | BotLifecycleKind::Trash)
            || !op.stage.moving()
        {
            return Err(BotError::Conflict);
        }
        let (stage, retained) = match answer {
            LifecycleRoutineAnswer::Paused => (BotLifecycleMemberStage::Acknowledged, None),
            LifecycleRoutineAnswer::Gone => (
                BotLifecycleMemberStage::Retained,
                Some(BotLifecycleRetained::AlreadyTrashed),
            ),
            LifecycleRoutineAnswer::Unavailable => (BotLifecycleMemberStage::Unavailable, None),
        };
        let changed = tx
            .execute(
                "UPDATE bot_lifecycle_members SET stage=?4,retained_reason=?5,updated_at_ms=?6
                 WHERE user_scope_id=?1 AND operation_id=?2 AND ordinal=?3 AND kind='routine'
                 AND stage IN ('pending','unavailable')",
                params![
                    scope.to_storage(),
                    operation.to_storage(),
                    stored.ordinal,
                    stage.token(),
                    retained.map(BotLifecycleRetained::token),
                    now
                ],
            )
            .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        let done = read_operation(&tx, scope, operation)?
            .as_ref()
            .and_then(member)
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }

    /// Every conversation of a Bot, with whether it is in Trash: what a
    /// whole-Bot operation captures.
    pub fn lifecycle_bot_chats(&self, bot: BotId) -> Result<Vec<(ChatId, bool)>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT bc.chat_id,EXISTS(SELECT 1 FROM bot_chat_trash t WHERE t.chat_id=bc.chat_id)
                 FROM bot_chats bc WHERE bc.bot_id=?1 ORDER BY bc.purpose<>'main',bc.created_at_ms,bc.chat_id",
            )
            .map_err(bad)?;
        let rows = query
            .query_map([bot.to_storage()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (chat, trashed) = row.map_err(bad)?;
            Ok((ChatId::parse(&chat).ok_or(BotError::Storage)?, trashed))
        })
        .collect()
    }

    /// Up to `limit` of one Bot's finalized submission receipts its owner has not
    /// settled yet, through ai17's per-Bot `bot_command_guard` index.
    pub fn bot_unresolved_submissions(
        &self,
        bot: BotId,
        limit: u16,
    ) -> Result<Vec<(UserScopeId, BotRequestId)>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_command_guard
                 WHERE bot_id=?1 AND command_kind IN ('start','continue','routine_occurrence') AND stage IN ('prepared','accepted')
                 LIMIT ?2",
            )
            .map_err(bad)?;
        let rows = query
            .query_map(params![bot.to_storage(), limit.clamp(1, 64)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (scope, request) = row.map_err(bad)?;
            Ok((
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                request.parse().map_err(bad)?,
            ))
        })
        .collect()
    }

    /// The Bot task attempts of a Bot (or of one of its conversations) that
    /// have not ended, whatever their receipt's stage: an operation stops
    /// each as the person's Stop and completes only once there is none. A
    /// running attempt's reply is the conversation's one pending assistant
    /// message (`one_pending_assistant_per_chat`), so this reads at most one
    /// per conversation and never a conversation's history; `bot_chats` has
    /// no index on `bot_id`, so the association table is scanned, as the
    /// Bots view's reads do.
    pub fn bot_unended_attempts(
        &self,
        bot: BotId,
        chat: Option<ChatId>,
    ) -> Result<Vec<InferenceAttemptId>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT r.ai_attempt_id FROM bot_chats AS bc
                  CROSS JOIN messages AS m INDEXED BY one_pending_assistant_per_chat
                          ON m.chat_id = bc.chat_id AND m.state = 'pending' AND m.role = 'assistant'
                  CROSS JOIN bot_chat_runs AS r ON r.assistant_message_id = m.id
                  CROSS JOIN inference_attempts AS a ON a.id = r.ai_attempt_id
                 WHERE bc.bot_id = ?1 AND (?2 IS NULL OR bc.chat_id = ?2)
                   AND a.status IN ('pending', 'streaming')",
            )
            .map_err(bad)?;
        let rows = query
            .query_map(
                params![bot.to_storage(), chat.map(ChatId::to_storage)],
                |r| r.get::<_, String>(0),
            )
            .map_err(bad)?;
        rows.map(|row| InferenceAttemptId::parse(&row.map_err(bad)?).ok_or(BotError::Storage))
            .collect()
    }

    /// The unfinished, unreversed whole-Bot operation that installed an
    /// effects-off barrier, if any (at most one moves, and a parked one
    /// blocks any other).
    pub fn open_bot_lifecycle(
        &self,
        bot: BotId,
    ) -> Result<Option<(UserScopeId, BotRequestId)>, BotError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(&open_bot_barrier_sql(), [bot.to_storage()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(bad)?;
        row.map(|(scope, operation)| {
            Ok((
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                operation.parse().map_err(bad)?,
            ))
        })
        .transpose()
    }

    /// Move a moving operation from `from` to `to`, and record its
    /// effects-off barrier effective when the owner said so. `false` when
    /// the operation was no longer at `from` (parked, or moved on already).
    pub fn advance_bot_lifecycle(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        from: BotLifecycleStage,
        to: BotLifecycleStage,
        policy_effective: bool,
        now: i64,
    ) -> Result<bool, BotError> {
        if !to.moving() || !from.moving() {
            return Err(BotError::Invalid);
        }
        let changed = self
            .connection
            .execute(
                "UPDATE bot_lifecycle_intents SET stage=?4,updated_at_ms=?6,
                   policy_barrier_state=CASE WHEN ?5 AND policy_request_id IS NOT NULL THEN 'effective' ELSE policy_barrier_state END
                 WHERE user_scope_id=?1 AND operation_id=?2 AND stage=?3",
                params![
                    scope.to_storage(),
                    operation.to_storage(),
                    from.token(),
                    to.token(),
                    policy_effective,
                    now
                ],
            )
            .map_err(bad)?;
        Ok(changed == 1)
    }

    /// Park a moving operation as incomplete. Its barrier stays: the Bot stays
    /// paused and its effects off until a retry finishes it or a Restore
    /// reverses it.
    pub fn park_bot_lifecycle(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        refusal: BotLifecycleRefusal,
        now: i64,
    ) -> Result<bool, BotError> {
        let changed = self
            .connection
            .execute(
                "UPDATE bot_lifecycle_intents SET stage='incomplete',refusal=?3,updated_at_ms=?4
                 WHERE user_scope_id=?1 AND operation_id=?2 AND stage NOT IN ('completed','incomplete','reversed')",
                params![
                    scope.to_storage(),
                    operation.to_storage(),
                    refusal.token(),
                    now
                ],
            )
            .map_err(bad)?;
        Ok(changed == 1)
    }

    /// A repeated request resumes its parked operation from `captured`, which
    /// re-asks every owner idempotently. A reversed operation is no longer
    /// `incomplete`, so it is never resumed (`false`).
    pub fn resume_bot_lifecycle(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        now: i64,
    ) -> Result<bool, BotError> {
        let changed = self
            .connection
            .execute(
                "UPDATE bot_lifecycle_intents SET stage='captured',refusal=NULL,updated_at_ms=?3
                 WHERE user_scope_id=?1 AND operation_id=?2 AND stage='incomplete'",
                params![scope.to_storage(), operation.to_storage(), now],
            )
            .map_err(|error| match error {
                // Another operation of the Bot is moving: one winner.
                rusqlite::Error::SqliteFailure(e, _)
                    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    BotError::Conflict
                }
                other => bad(other),
            })?;
        Ok(changed == 1)
    }

    /// Archive's last step, once its work is settled and its effects-off
    /// barrier is effective: the Bot becomes archived, its revision goes up,
    /// every captured member is acknowledged and the operation completes.
    /// Nothing is hidden or removed.
    pub fn complete_bot_archive(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        now: i64,
    ) -> Result<LifecycleOperation, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let op = read_operation(&tx, scope, operation)?.ok_or(BotError::NotFound)?;
        if op.kind != BotLifecycleKind::Archive
            || op.stage != BotLifecycleStage::Settling
            || op.policy_barrier != Some(BotLifecycleBarrierState::Effective)
        {
            return Err(BotError::Conflict);
        }
        require_routines_settled(&tx, scope, operation)?;
        let (current, prior) = admission::guard_any_lifecycle(&tx, op.owner, op.bot)?;
        if prior != BotLifecycle::Active {
            return Err(BotError::Conflict);
        }
        if !revision_held(current.revision, op.barrier_revision) {
            return Err(BotError::RevisionConflict);
        }
        let revision = current.revision.checked_add(1).ok_or(BotError::Invalid)?;
        tx.execute(
            "UPDATE bots SET lifecycle='archived',revision=?2,updated_at_ms=?3 WHERE bot_id=?1 AND revision=?4",
            params![op.bot.to_storage(), revision, now, current.revision],
        )
        .map_err(bad)?;
        // A Routine member is settled only by the coordinator's answer
        // (`settle_bot_lifecycle_routine`), never by completion.
        tx.execute(
            "UPDATE bot_lifecycle_members SET stage='acknowledged',updated_at_ms=?3
             WHERE user_scope_id=?1 AND operation_id=?2 AND stage='pending' AND kind<>'routine'",
            params![scope.to_storage(), operation.to_storage(), now],
        )
        .map_err(bad)?;
        tx.execute(
            "UPDATE bot_lifecycle_intents SET stage='completed',completed_at_ms=?3,updated_at_ms=?3
             WHERE user_scope_id=?1 AND operation_id=?2 AND stage='settling'",
            params![scope.to_storage(), operation.to_storage(), now],
        )
        .map_err(bad)?;
        let done = read_operation(&tx, scope, operation)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }

    /// Unarchive: a Restore naming a completed or parked Archive, captured and
    /// applied in one transaction. The Bot is active again and still paused;
    /// its revision goes up. No owner is called and nothing is resumed. An
    /// exact repeat returns the stored Restore.
    pub fn unarchive_bot(
        &mut self,
        capture: &LifecycleCapture,
    ) -> Result<LifecycleOperation, BotError> {
        if capture.kind != BotLifecycleKind::Restore || !capture.members.is_empty() {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let source = capture.source.ok_or(BotError::Invalid)?;
        let original =
            read_operation(&tx, capture.owner.user_scope, source)?.ok_or(BotError::NotFound)?;
        if original.kind != BotLifecycleKind::Archive {
            return Err(BotError::Invalid);
        }
        let (restore, fresh) = capture_on(&tx, capture)?;
        if !fresh {
            return Ok(restore);
        }
        let (current, prior) = admission::guard_any_lifecycle(&tx, capture.owner, capture.bot)?;
        let revision = current.revision.checked_add(1).ok_or(BotError::Invalid)?;
        // A parked Archive left the Bot active and paused; a completed one
        // archived it. Either way it returns active, and stays paused.
        if !matches!(prior, BotLifecycle::Archived | BotLifecycle::Active) {
            return Err(BotError::Conflict);
        }
        tx.execute(
            "UPDATE bots SET lifecycle='active',paused=1,revision=?2,updated_at_ms=?3 WHERE bot_id=?1 AND revision=?4",
            params![capture.bot.to_storage(), revision, capture.now, current.revision],
        )
        .map_err(bad)?;
        reverse_source(&tx, capture.owner.user_scope, source, capture.now)?;
        complete_operation(
            &tx,
            capture.owner.user_scope,
            capture.operation,
            BotLifecycleStage::Captured,
            capture.now,
        )?;
        let done = read_operation(&tx, capture.owner.user_scope, capture.operation)?
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }
}

/// Trash and Restore (P1.7 S6, S7). In this version they have an AI effect
/// only (O41): a trashed Bot and a conversation in Trash are hidden, and no
/// page, Run record, folder or credential is moved, so no Store or delegate
/// owner is called here or by the driver for them.
impl Repository {
    /// A whole-Bot Trash's last step, once its work is settled and, for an
    /// active Bot, its effects-off barrier is effective. One transaction: the
    /// Bot becomes trashed and its revision goes up; each captured
    /// conversation that was not already in Trash goes into Trash under this
    /// operation (one already there keeps its own operation's row, so its own
    /// Restore stays the only way back); every pending member is
    /// acknowledged and the operation completes.
    pub fn complete_bot_trash(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        now: i64,
    ) -> Result<LifecycleOperation, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let op = read_operation(&tx, scope, operation)?.ok_or(BotError::NotFound)?;
        if op.kind != BotLifecycleKind::Trash
            || op.stage != BotLifecycleStage::Settling
            || (op.policy_request.is_some()
                && op.policy_barrier != Some(BotLifecycleBarrierState::Effective))
        {
            return Err(BotError::Conflict);
        }
        require_routines_settled(&tx, scope, operation)?;
        let (current, prior) = admission::guard_any_lifecycle(&tx, op.owner, op.bot)?;
        if prior != op.prior_lifecycle || prior == BotLifecycle::Trashed {
            return Err(BotError::Conflict);
        }
        // An active Bot's capture paused it (one revision); an archived one
        // was captured as it stood, and nothing can move an archived Bot's
        // revision (a refusal fence needs an active one).
        if !revision_held(
            current.revision,
            Some(op.barrier_revision.unwrap_or(op.expected_revision)),
        ) {
            return Err(BotError::RevisionConflict);
        }
        let revision = current.revision.checked_add(1).ok_or(BotError::Invalid)?;
        tx.execute(
            "UPDATE bots SET lifecycle='trashed',revision=?2,updated_at_ms=?3 WHERE bot_id=?1 AND revision=?4",
            params![op.bot.to_storage(), revision, now, current.revision],
        )
        .map_err(bad)?;
        insert_captured_chat_trash(&tx, scope, operation, now)?;
        complete_operation(&tx, scope, operation, BotLifecycleStage::Settling, now)?;
        let done = read_operation(&tx, scope, operation)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }

    /// Restore of a whole-Bot Trash, completed or parked, captured and
    /// applied in one transaction. The Bot returns to the lifecycle it had
    /// when it was trashed (active, or archived), paused, with its revision
    /// raised; exactly the conversations this Trash put into Trash come back.
    /// No owner is called and nothing is resumed. An exact repeat returns the
    /// stored Restore.
    pub fn restore_trashed_bot(
        &mut self,
        capture: &LifecycleCapture,
    ) -> Result<LifecycleOperation, BotError> {
        if capture.kind != BotLifecycleKind::Restore || !capture.members.is_empty() {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let source = capture.source.ok_or(BotError::Invalid)?;
        let original =
            read_operation(&tx, capture.owner.user_scope, source)?.ok_or(BotError::NotFound)?;
        if original.kind != BotLifecycleKind::Trash {
            return Err(BotError::Invalid);
        }
        let (restore, fresh) = capture_on(&tx, capture)?;
        if !fresh {
            return Ok(restore);
        }
        let (current, prior) = admission::guard_any_lifecycle(&tx, capture.owner, capture.bot)?;
        // A completed Trash left the Bot trashed; a parked one left it as it
        // was. Either way it returns to that lifecycle, paused.
        let back = original.prior_lifecycle;
        if back == BotLifecycle::Trashed || !(prior == BotLifecycle::Trashed || prior == back) {
            return Err(BotError::Conflict);
        }
        let revision = current.revision.checked_add(1).ok_or(BotError::Invalid)?;
        tx.execute(
            "UPDATE bots SET lifecycle=?5,paused=1,revision=?2,updated_at_ms=?3 WHERE bot_id=?1 AND revision=?4",
            params![
                capture.bot.to_storage(),
                revision,
                capture.now,
                current.revision,
                back.token()
            ],
        )
        .map_err(bad)?;
        tx.execute(
            "DELETE FROM bot_chat_trash WHERE user_scope_id=?1 AND operation_id=?2",
            params![capture.owner.user_scope.to_storage(), source.to_storage()],
        )
        .map_err(bad)?;
        reverse_source(&tx, capture.owner.user_scope, source, capture.now)?;
        complete_operation(
            &tx,
            capture.owner.user_scope,
            capture.operation,
            BotLifecycleStage::Captured,
            capture.now,
        )?;
        let done = read_operation(&tx, capture.owner.user_scope, capture.operation)?
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }

    /// A conversation Trash's last step, once that conversation's work is
    /// settled: one transaction puts it into Trash under this operation and
    /// completes. The Bot is not paused and its revision does not move, so
    /// its other conversations are not disturbed.
    pub fn complete_bot_chat_trash(
        &mut self,
        scope: UserScopeId,
        operation: BotRequestId,
        now: i64,
    ) -> Result<LifecycleOperation, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let op = read_operation(&tx, scope, operation)?.ok_or(BotError::NotFound)?;
        if op.kind != BotLifecycleKind::TrashChat || op.stage != BotLifecycleStage::Settling {
            return Err(BotError::Conflict);
        }
        let (_, prior) = admission::guard_any_lifecycle(&tx, op.owner, op.bot)?;
        if prior != BotLifecycle::Active {
            return Err(BotError::Conflict);
        }
        insert_captured_chat_trash(&tx, scope, operation, now)?;
        complete_operation(&tx, scope, operation, BotLifecycleStage::Settling, now)?;
        let done = read_operation(&tx, scope, operation)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }

    /// Restore of one conversation's Trash, completed or parked, captured and
    /// applied in one transaction: exactly that operation's Trash row is
    /// removed, so the same conversation and history return. Nothing runs.
    pub fn restore_bot_chat(
        &mut self,
        capture: &LifecycleCapture,
    ) -> Result<LifecycleOperation, BotError> {
        if capture.kind != BotLifecycleKind::RestoreChat || !capture.members.is_empty() {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let chat = capture.chat.ok_or(BotError::Invalid)?;
        let source = capture.source.ok_or(BotError::Invalid)?;
        let (restore, fresh) = capture_on(&tx, capture)?;
        if !fresh {
            return Ok(restore);
        }
        tx.execute(
            "DELETE FROM bot_chat_trash WHERE chat_id=?1 AND user_scope_id=?2 AND operation_id=?3",
            params![
                chat.to_storage(),
                capture.owner.user_scope.to_storage(),
                source.to_storage()
            ],
        )
        .map_err(bad)?;
        reverse_source(&tx, capture.owner.user_scope, source, capture.now)?;
        complete_operation(
            &tx,
            capture.owner.user_scope,
            capture.operation,
            BotLifecycleStage::Captured,
            capture.now,
        )?;
        let done = read_operation(&tx, capture.owner.user_scope, capture.operation)?
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(done)
    }

    /// Whether a Bot conversation is in Trash now, by any operation.
    pub fn bot_chat_in_trash(&self, chat: ChatId) -> Result<bool, BotError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bot_chat_trash WHERE chat_id=?1)",
                [chat.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)
    }
}

/// Put each captured conversation still `pending` into Trash under this
/// operation. A conversation captured as already in Trash is `retained`
/// and keeps its own operation's row.
fn insert_captured_chat_trash(
    tx: &Connection,
    scope: UserScopeId,
    operation: BotRequestId,
    now: i64,
) -> Result<(), BotError> {
    tx.execute(
        "INSERT INTO bot_chat_trash(chat_id,user_scope_id,operation_id,trashed_at_ms)
         SELECT chat_id,user_scope_id,operation_id,?3 FROM bot_lifecycle_members
         WHERE user_scope_id=?1 AND operation_id=?2 AND kind='chat' AND stage='pending'",
        params![scope.to_storage(), operation.to_storage(), now],
    )
    .map_err(bad)?;
    Ok(())
}

/// Mark the operation a Restore reverses `reversed`, in the Restore's own
/// transaction: completed or parked, it then holds nothing, leaves every
/// index of unfinished operations, and is never resumed, with no read asking
/// whether a Restore names it. It keeps its refusal (why it had parked) or
/// its completion time (when it had completed). Capture already required
/// the source to be completed or parked and not yet reversed or purged
/// (`require_source`), so anything else rolls the transaction back.
fn reverse_source(
    tx: &Connection,
    scope: UserScopeId,
    source: BotRequestId,
    now: i64,
) -> Result<(), BotError> {
    let changed = tx
        .execute(
            "UPDATE bot_lifecycle_intents SET stage='reversed',updated_at_ms=?3
             WHERE user_scope_id=?1 AND operation_id=?2 AND stage IN ('completed','incomplete')",
            params![scope.to_storage(), source.to_storage(), now],
        )
        .map_err(bad)?;
    if changed != 1 {
        return Err(BotError::Conflict);
    }
    Ok(())
}

/// Acknowledge every pending member the AI owner itself settles and
/// complete the operation, which must still be at `from`; anything else rolls
/// the caller's transaction back. A Routine member is settled only by the
/// coordinator's answer (`settle_bot_lifecycle_routine`), never here.
fn complete_operation(
    tx: &Connection,
    scope: UserScopeId,
    operation: BotRequestId,
    from: BotLifecycleStage,
    now: i64,
) -> Result<(), BotError> {
    tx.execute(
        "UPDATE bot_lifecycle_members SET stage='acknowledged',updated_at_ms=?3
         WHERE user_scope_id=?1 AND operation_id=?2 AND stage='pending' AND kind<>'routine'",
        params![scope.to_storage(), operation.to_storage(), now],
    )
    .map_err(bad)?;
    let changed = tx
        .execute(
            "UPDATE bot_lifecycle_intents SET stage='completed',completed_at_ms=?3,updated_at_ms=?3
             WHERE user_scope_id=?1 AND operation_id=?2 AND stage=?4",
            params![
                scope.to_storage(),
                operation.to_storage(),
                now,
                from.token()
            ],
        )
        .map_err(bad)?;
    if changed != 1 {
        return Err(BotError::Conflict);
    }
    Ok(())
}

/// Why lifecycle holds a Bot or one of its conversations still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotHold {
    /// The Bot is in Trash, and so is everything of it: nothing but Stop may
    /// touch it until the Bot is restored.
    BotTrashed,
    /// This conversation is in Trash on its own: nothing but Stop may touch
    /// it until it is restored.
    Trashed,
    /// An Archive, Trash, Restore or Purge is moving.
    Pending,
    /// One stopped before it finished and is parked incomplete: it waits for
    /// the person to resume or reverse it, and holds the subject meanwhile.
    Parked,
}
impl BotHold {
    /// In Trash, by its own operation or its Bot's: hidden from every read.
    pub fn hides(self) -> bool {
        matches!(self, Self::BotTrashed | Self::Trashed)
    }
}

impl Repository {
    /// The Bot conversations aggregate owner reads may include: not in
    /// Trash and not of a trashed Bot. Named-ID guards use the raw
    /// association instead, so a hidden conversation never reads as ordinary.
    pub fn visible_bot_chats(&self) -> Result<Vec<(BotId, ChatId)>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT bc.bot_id,bc.chat_id FROM bot_chats bc JOIN bots b ON b.bot_id=bc.bot_id
                 WHERE b.lifecycle<>'trashed'
                   AND NOT EXISTS(SELECT 1 FROM bot_chat_trash t WHERE t.chat_id=bc.chat_id)",
            )
            .map_err(bad)?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (bot, chat) = row.map_err(bad)?;
            Ok((
                bot.parse().map_err(bad)?,
                ChatId::parse(&chat).ok_or(BotError::Storage)?,
            ))
        })
        .collect()
    }

    /// The hold on a whole Bot: trashed, or a whole-Bot operation unfinished.
    pub fn bot_hold(&self, bot: BotId) -> Result<Option<BotHold>, BotError> {
        let (trashed, moving, parked): (bool, bool, bool) = self
            .connection
            .query_row(&bot_hold_sql(), [bot.to_storage()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(bad)?;
        Ok(hold(trashed, moving, parked).map(|hold| match hold {
            // The Bot itself is in Trash.
            BotHold::Trashed => BotHold::BotTrashed,
            other => other,
        }))
    }

    /// A Bot's unfinished lifecycle operation, moving or parked and not
    /// reversed, whole-Bot or of one conversation: what its read reports so
    /// a client that lost the id can resume or reverse it. The newest wins
    /// when a moving reversal stands beside the parked operation it names.
    pub fn bot_open_lifecycle(&self, bot: BotId) -> Result<Option<BotOpenLifecycle>, BotError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(&bot_open_sql(), [bot.to_storage()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(bad)?;
        let Some((scope, operation)) = row else {
            return Ok(None);
        };
        let scope = UserScopeId::parse(&scope).ok_or(BotError::Storage)?;
        let op = read_operation(&self.connection, scope, operation.parse().map_err(bad)?)?
            .ok_or(BotError::Storage)?;
        Ok(Some(BotOpenLifecycle {
            request_id: op.operation,
            kind: op.kind,
            chat_id: op.chat,
            stage: op.stage,
            refusal: op.refusal,
        }))
    }

    /// The hold on one Bot conversation: its own or its whole Bot's.
    pub fn bot_chat_hold(&self, chat: ChatId) -> Result<Option<BotHold>, BotError> {
        let Some(bot) = bot_for_chat_on(&self.connection, chat)? else {
            return Ok(None);
        };
        let (trashed, moving, parked): (bool, bool, bool) = self
            .connection
            .query_row(&chat_hold_sql(), [chat.to_storage()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(bad)?;
        let whole = self.bot_hold(bot)?;
        // The strongest hold wins: the Bot's Trash (restoring the Bot is
        // what is needed), the conversation's own Trash, then a moving
        // operation, then a parked one.
        Ok([hold(trashed, moving, parked), whole]
            .into_iter()
            .flatten()
            .min_by_key(|hold| match hold {
                BotHold::BotTrashed => 0,
                BotHold::Trashed => 1,
                BotHold::Pending => 2,
                BotHold::Parked => 3,
            }))
    }

    /// A Bot's fixed owner in any lifecycle, for checking ownership before
    /// telling a caller that a hidden Bot or conversation is held.
    pub fn bot_owner(&self, bot: BotId) -> Result<Option<BotOwner>, BotError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT user_scope_id,owner_device_id FROM bots WHERE bot_id=?1",
                [bot.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(bad)?;
        row.map(|(scope, device)| {
            Ok(BotOwner {
                user_scope: UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                device: DeviceId::parse(&device).ok_or(BotError::Storage)?,
            })
        })
        .transpose()
    }

    /// The Bots of one owner's workspace in one lifecycle (Archived, Trash).
    pub fn bots_in_lifecycle(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        lifecycle: BotLifecycle,
    ) -> Result<Vec<Bot>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT bot_id FROM bots WHERE user_scope_id=?1 AND owner_device_id=?2 AND home_workspace_id=?3 AND lifecycle=?4 ORDER BY created_at_ms,bot_id",
            )
            .map_err(bad)?;
        let ids = query
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    workspace.to_storage(),
                    lifecycle.token()
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(bad)?;
        ids.map(|id| {
            let id: BotId = id.map_err(bad)?.parse().map_err(bad)?;
            self.bot_any_lifecycle(id)?
                .map(|(bot, _)| bot)
                .ok_or(BotError::Storage)
        })
        .collect()
    }

    /// Fixture only: the AI owner's Trash effect for one captured conversation,
    /// which S6's driver applies after settling it.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn fixture_trash_bot_chat(
        &mut self,
        operation: &LifecycleOperation,
        chat: ChatId,
        now: i64,
    ) -> Result<(), BotError> {
        self.connection
            .execute(
                "INSERT INTO bot_chat_trash(chat_id,user_scope_id,operation_id,trashed_at_ms) VALUES(?1,?2,?3,?4)",
                params![
                    chat.to_storage(),
                    operation.owner.user_scope.to_storage(),
                    operation.operation.to_storage(),
                    now
                ],
            )
            .map_err(bad)?;
        Ok(())
    }
}

impl Repository {
    /// Fixture only: mark a captured operation completed, as its driver will.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn fixture_complete_bot_lifecycle(
        &mut self,
        operation: &LifecycleOperation,
        now: i64,
    ) -> Result<(), BotError> {
        self.connection
            .execute(
                "UPDATE bot_lifecycle_intents SET stage='completed',completed_at_ms=?3,updated_at_ms=?3 WHERE user_scope_id=?1 AND operation_id=?2",
                params![
                    operation.owner.user_scope.to_storage(),
                    operation.operation.to_storage(),
                    now
                ],
            )
            .map_err(bad)?;
        Ok(())
    }

    /// Fixture only: a Bot in another lifecycle, as Archive/Trash will leave it.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn fixture_set_bot_lifecycle(
        &mut self,
        bot: BotId,
        lifecycle: BotLifecycle,
    ) -> Result<(), BotError> {
        self.connection
            .execute(
                "UPDATE bots SET lifecycle=?2 WHERE bot_id=?1",
                params![bot.to_storage(), lifecycle.token()],
            )
            .map_err(bad)?;
        Ok(())
    }
}

fn hold(trashed: bool, moving: bool, parked: bool) -> Option<BotHold> {
    if trashed {
        Some(BotHold::Trashed)
    } else if moving {
        Some(BotHold::Pending)
    } else if parked {
        Some(BotHold::Parked)
    } else {
        None
    }
}

fn validate_capture(capture: &LifecycleCapture) -> Result<(), BotError> {
    let kinds = allowed_members(capture.kind);
    if capture.kind.names_chat() != capture.chat.is_some()
        || capture.kind.needs_source() != capture.source.is_some()
        || capture.fingerprint.len() != 64
        || !capture.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
        || capture.expected_revision == 0
        || capture.expected_revision >= i64::MAX as u64
        || capture.members.len() > 4096
    {
        return Err(BotError::Invalid);
    }
    for member in &capture.members {
        let kind = member.subject.kind();
        if !kinds.contains(&kind)
            || (member.intended == BotLifecycleIntended::Retain) != member.retained.is_some()
            || (kind == BotLifecycleMemberKind::Routine)
                != (member.reason == BotLifecycleReason::OwnedRoutine)
        {
            return Err(BotError::Invalid);
        }
        // A conversation operation captures only that conversation's members.
        if let Some(chat) = capture.chat {
            let own = match &member.subject {
                BotLifecycleSubject::Chat { chat_id }
                | BotLifecycleSubject::AttachmentCopy { chat_id, .. } => *chat_id == chat,
                BotLifecycleSubject::RunReference { chat_id, .. } => *chat_id == Some(chat),
                _ => false,
            };
            if !own {
                return Err(BotError::Invalid);
            }
        }
    }
    Ok(())
}

fn require_no_open_operation(
    conn: &Connection,
    capture: &LifecycleCapture,
) -> Result<(), BotError> {
    let mut query = conn.prepare(&open_operations_sql()).map_err(bad)?;
    let rows = query
        .query_map([capture.bot.to_storage()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(bad)?;
    for row in rows {
        let (scope, operation, stage) = row.map_err(bad)?;
        let parked = BotLifecycleStage::parse(&stage).ok_or(BotError::Storage)?
            == BotLifecycleStage::Incomplete;
        // Only a reversal of the parked operation itself may start beside it.
        let reverses = parked
            && scope == capture.owner.user_scope.to_storage()
            && capture.source.map(BotRequestId::to_storage) == Some(operation);
        if !reverses {
            return Err(BotError::Conflict);
        }
    }
    Ok(())
}

fn require_source(
    conn: &Connection,
    capture: &LifecycleCapture,
    source: BotRequestId,
) -> Result<(), BotError> {
    let original =
        read_operation(conn, capture.owner.user_scope, source)?.ok_or(BotError::NotFound)?;
    let (kinds, incomplete) = source_rule(capture.kind).ok_or(BotError::Invalid)?;
    let stage_ok = original.stage == BotLifecycleStage::Completed
        || (incomplete && original.stage == BotLifecycleStage::Incomplete);
    if original.bot != capture.bot
        || original.chat != capture.chat
        || !kinds.contains(&original.kind)
        || !stage_ok
    {
        return Err(BotError::Conflict);
    }
    let used: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_lifecycle_intents WHERE user_scope_id=?1 AND source_operation_id=?2)",
            params![capture.owner.user_scope.to_storage(), source.to_storage()],
            |row| row.get(0),
        )
        .map_err(bad)?;
    if used {
        return Err(BotError::Conflict);
    }
    Ok(())
}

fn insert_member(
    conn: &Connection,
    capture: &LifecycleCapture,
    ordinal: usize,
    member: &LifecycleMemberDraft,
) -> Result<(), BotError> {
    let (chat, run, workspace, document, attachment) = match &member.subject {
        BotLifecycleSubject::Bot => (None, None, None, None, None),
        BotLifecycleSubject::Chat { chat_id } => {
            (Some(chat_id.to_storage()), None, None, None, None)
        }
        BotLifecycleSubject::RunReference {
            run_id,
            workspace_id,
            document_id,
            chat_id,
        } => (
            chat_id.map(ChatId::to_storage),
            Some(run_id.to_storage()),
            Some(workspace_id.to_storage()),
            document_id.map(|d| d.to_storage()),
            None,
        ),
        BotLifecycleSubject::OwnedRole {
            workspace_id,
            document_id,
        } => (
            None,
            None,
            Some(workspace_id.to_storage()),
            Some(document_id.to_storage()),
            None,
        ),
        BotLifecycleSubject::AttachmentCopy {
            chat_id,
            attachment_id,
        } => (
            Some(chat_id.to_storage()),
            None,
            None,
            None,
            Some(attachment_id.to_storage()),
        ),
        BotLifecycleSubject::Routine {
            workspace_id,
            document_id,
        } => {
            // The member names one of this Bot's own Routines, in its home.
            let owned: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM bot_routines WHERE schedule_id=?1 AND user_scope_id=?2
                     AND owner_device_id=?3 AND bot_id=?4 AND workspace_id=?5)",
                    params![
                        document_id.to_storage(),
                        capture.owner.user_scope.to_storage(),
                        capture.owner.device.to_storage(),
                        capture.bot.to_storage(),
                        workspace_id.to_storage()
                    ],
                    |r| r.get(0),
                )
                .map_err(bad)?;
            if !owned {
                return Err(BotError::Invalid);
            }
            (
                None,
                None,
                Some(workspace_id.to_storage()),
                Some(document_id.to_storage()),
                None,
            )
        }
    };
    let stage = if member.retained.is_some() {
        BotLifecycleMemberStage::Retained
    } else {
        BotLifecycleMemberStage::Pending
    };
    let child = (member.subject.owner() != BotLifecycleOwner::Ai)
        .then(|| Uuid::new_v4().hyphenated().to_string());
    conn.execute(
        "INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,chat_id,run_id,workspace_id,document_id,attachment_id,reason,prior_state,intended,stage,retained_reason,child_request_id,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            capture.owner.user_scope.to_storage(),
            capture.operation.to_storage(),
            ordinal as i64,
            member.subject.kind().token(),
            member.subject.owner().token(),
            chat,
            run,
            workspace,
            document,
            attachment,
            member.reason.token(),
            member.prior_state.token(),
            member.intended.token(),
            stage.token(),
            member.retained.map(BotLifecycleRetained::token),
            child,
            capture.now,
        ],
    )
    .map_err(bad)?;
    Ok(())
}

fn token<T>(value: String, parse: fn(&str) -> Option<T>) -> Result<T, BotError> {
    parse(&value).ok_or(BotError::Storage)
}

type IntentRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    u64,
    Option<u64>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    i64,
    i64,
    Option<i64>,
);

fn read_operation(
    conn: &Connection,
    scope: UserScopeId,
    operation: BotRequestId,
) -> Result<Option<LifecycleOperation>, BotError> {
    let row: Option<IntentRow> = conn
        .query_row(
            "SELECT bot_id,owner_device_id,kind,chat_id,fingerprint,expected_revision,barrier_revision,source_operation_id,prior_lifecycle,pause_request_id,policy_request_id,policy_barrier_state,stage,refusal,requested_at_ms,updated_at_ms,completed_at_ms
             FROM bot_lifecycle_intents WHERE user_scope_id=?1 AND operation_id=?2",
            params![scope.to_storage(), operation.to_storage()],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                    r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?, r.get(13)?,
                    r.get(14)?, r.get(15)?, r.get(16)?,
                ))
            },
        )
        .optional()
        .map_err(bad)?;
    let Some(r) = row else {
        return Ok(None);
    };
    let chat = |value: Option<String>| -> Result<Option<ChatId>, BotError> {
        value
            .map(|v| ChatId::parse(&v).ok_or(BotError::Storage))
            .transpose()
    };
    Ok(Some(LifecycleOperation {
        operation,
        owner: BotOwner {
            user_scope: scope,
            device: DeviceId::parse(&r.1).ok_or(BotError::Storage)?,
        },
        bot: r.0.parse().map_err(bad)?,
        kind: token(r.2, BotLifecycleKind::parse)?,
        chat: chat(r.3)?,
        fingerprint: r.4,
        expected_revision: r.5,
        barrier_revision: r.6,
        source: r.7.map(|v| v.parse().map_err(bad)).transpose()?,
        prior_lifecycle: token(r.8, BotLifecycle::parse)?,
        pause_request: r.9.map(|v| v.parse().map_err(bad)).transpose()?,
        policy_request: r.10.map(|v| v.parse().map_err(bad)).transpose()?,
        policy_barrier: r
            .11
            .map(|v| token(v, BotLifecycleBarrierState::parse))
            .transpose()?,
        stage: token(r.12, BotLifecycleStage::parse)?,
        refusal: r
            .13
            .map(|v| token(v, BotLifecycleRefusal::parse))
            .transpose()?,
        requested_at_ms: r.14,
        updated_at_ms: r.15,
        completed_at_ms: r.16,
        members: read_members(conn, scope, operation)?,
    }))
}

type MemberRow = (
    u32,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
);

fn read_members(
    conn: &Connection,
    scope: UserScopeId,
    operation: BotRequestId,
) -> Result<Vec<LifecycleMember>, BotError> {
    let mut query = conn
        .prepare(
            "SELECT ordinal,kind,chat_id,run_id,workspace_id,document_id,attachment_id,reason,prior_state,intended,stage,retained_reason,child_request_id,updated_at_ms
             FROM bot_lifecycle_members WHERE user_scope_id=?1 AND operation_id=?2 ORDER BY ordinal",
        )
        .map_err(bad)?;
    let rows = query
        .query_map(params![scope.to_storage(), operation.to_storage()], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
            ))
        })
        .map_err(bad)?;
    rows.map(|row| {
        let r: MemberRow = row.map_err(bad)?;
        let need = |v: Option<String>| v.ok_or(BotError::Storage);
        let chat = |v: String| ChatId::parse(&v).ok_or(BotError::Storage);
        let workspace = |v: String| WorkspaceId::parse(&v).ok_or(BotError::Storage);
        let document = |v: String| DocumentId::parse(&v).ok_or(BotError::Storage);
        let subject = match token(r.1, BotLifecycleMemberKind::parse)? {
            BotLifecycleMemberKind::Bot => BotLifecycleSubject::Bot,
            BotLifecycleMemberKind::Chat => BotLifecycleSubject::Chat {
                chat_id: chat(need(r.2)?)?,
            },
            BotLifecycleMemberKind::RunReference => BotLifecycleSubject::RunReference {
                run_id: need(r.3)?.parse().map_err(bad)?,
                workspace_id: workspace(need(r.4)?)?,
                document_id: r.5.map(document).transpose()?,
                chat_id: r.2.map(chat).transpose()?,
            },
            BotLifecycleMemberKind::OwnedRole => BotLifecycleSubject::OwnedRole {
                workspace_id: workspace(need(r.4)?)?,
                document_id: document(need(r.5)?)?,
            },
            BotLifecycleMemberKind::AttachmentCopy => BotLifecycleSubject::AttachmentCopy {
                chat_id: chat(need(r.2)?)?,
                attachment_id: ChatAttachmentId::parse(&need(r.6)?).ok_or(BotError::Storage)?,
            },
            BotLifecycleMemberKind::Routine => BotLifecycleSubject::Routine {
                workspace_id: workspace(need(r.4)?)?,
                document_id: document(need(r.5)?)?,
            },
        };
        Ok(LifecycleMember {
            ordinal: r.0,
            subject,
            reason: token(r.7, BotLifecycleReason::parse)?,
            prior_state: token(r.8, BotLifecyclePriorState::parse)?,
            intended: token(r.9, BotLifecycleIntended::parse)?,
            stage: token(r.10, BotLifecycleMemberStage::parse)?,
            retained: r
                .11
                .map(|v| token(v, BotLifecycleRetained::parse))
                .transpose()?,
            child_request: r.12.map(|v| Uuid::parse_str(&v).map_err(bad)).transpose()?,
            updated_at_ms: r.13,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests;
