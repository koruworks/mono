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
        }
    }
}

/// Which member kinds an operation may capture. Restore acts on its source
/// operation's members and captures none of its own.
fn allowed_members(kind: BotLifecycleKind) -> &'static [BotLifecycleMemberKind] {
    use BotLifecycleMemberKind as M;
    match kind {
        BotLifecycleKind::Archive => &[M::Bot, M::Chat],
        BotLifecycleKind::Trash => &[M::Bot, M::Chat, M::RunReference, M::OwnedRole],
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
        validate_capture(capture)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        if let Some(existing) = read_operation(&tx, capture.owner.user_scope, capture.operation)? {
            return if existing.bot == capture.bot
                && existing.fingerprint == capture.fingerprint
                && existing.owner == capture.owner
            {
                Ok(existing)
            } else {
                Err(BotError::Conflict)
            };
        }
        let (current, prior) = admission::guard_any_lifecycle(&tx, capture.owner, capture.bot)?;
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
            admission::prepare_bot_pause_on(
                &tx,
                capture.owner,
                capture.bot,
                request,
                capture.expected_revision,
                true,
                &capture.fingerprint,
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
        let operation = read_operation(&tx, capture.owner.user_scope, capture.operation)?
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(operation)
    }

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
            .query_row(
                "SELECT user_scope_id,operation_id FROM bot_lifecycle_intents WHERE chat_id=?1 AND stage<>'completed'",
                [chat.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
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
        let mut query = self
            .connection
            .prepare(
                "SELECT updated_at_ms,user_scope_id,operation_id FROM bot_lifecycle_intents
                 WHERE stage NOT IN ('completed','incomplete')
                   AND (updated_at_ms>?1 OR (updated_at_ms=?1 AND operation_id>?2))
                 ORDER BY updated_at_ms,operation_id LIMIT ?3",
            )
            .map_err(bad)?;
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
}

/// Why lifecycle holds a Bot or one of its conversations still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotHold {
    /// In Trash, or its Bot is: nothing but Stop may touch it until restored.
    Trashed,
    /// An Archive, Trash, Restore or Purge is moving or parked incomplete.
    Pending,
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
        let (trashed, pending): (bool, bool) = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bots WHERE bot_id=?1 AND lifecycle='trashed'),
                        EXISTS(SELECT 1 FROM bot_lifecycle_intents WHERE bot_id=?1 AND stage<>'completed' AND chat_id IS NULL)",
                [bot.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(bad)?;
        Ok(hold(trashed, pending))
    }

    /// The hold on one Bot conversation: its own or its whole Bot's.
    pub fn bot_chat_hold(&self, chat: ChatId) -> Result<Option<BotHold>, BotError> {
        let Some(bot) = bot_for_chat_on(&self.connection, chat)? else {
            return Ok(None);
        };
        let (trashed, pending): (bool, bool) = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bot_chat_trash WHERE chat_id=?1),
                        EXISTS(SELECT 1 FROM bot_lifecycle_intents WHERE chat_id=?1 AND stage<>'completed')",
                [chat.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(bad)?;
        let whole = self.bot_hold(bot)?;
        Ok(match (hold(trashed, pending), whole) {
            (Some(BotHold::Trashed), _) | (_, Some(BotHold::Trashed)) => Some(BotHold::Trashed),
            (Some(BotHold::Pending), _) | (_, Some(BotHold::Pending)) => Some(BotHold::Pending),
            (None, None) => None,
        })
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

fn hold(trashed: bool, pending: bool) -> Option<BotHold> {
    if trashed {
        Some(BotHold::Trashed)
    } else if pending {
        Some(BotHold::Pending)
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
    let mut query = conn
        .prepare("SELECT user_scope_id,operation_id,stage FROM bot_lifecycle_intents WHERE bot_id=?1 AND stage<>'completed'")
        .map_err(bad)?;
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
