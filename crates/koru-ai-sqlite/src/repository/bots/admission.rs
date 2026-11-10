// SPDX-License-Identifier: MPL-2.0

//! Typed ai16 coordination, on the sole existing AI connection.
use super::*;
use koru_delegate_core::{BotAdmissionBarrier, BotAdmissionView, WorkspaceScope};
use rusqlite::{Transaction, TransactionBehavior};
#[cfg(test)]
mod tests;

struct Raw {
    bot: String,
    fingerprint: String,
    kind: String,
    stage: String,
    run: Option<String>,
    config: Option<u64>,
    policy: Option<u64>,
    version: Option<u32>,
    submission: Option<String>,
    desired: Option<bool>,
    expected: Option<u64>,
    ack: Option<String>,
    outcome: Option<String>,
}
fn raw(
    conn: &Connection,
    scope: UserScopeId,
    request: BotRequestId,
) -> Result<Option<Raw>, BotError> {
    conn.query_row("SELECT bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,desired_pause,expected_revision,pause_ack_stage,outcome_json FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
        params![scope.to_storage(),request.to_storage()],|r|Ok(Raw {bot:r.get(0)?,fingerprint:r.get(1)?,kind:r.get(2)?,stage:r.get(3)?,run:r.get(4)?,config:r.get(5)?,policy:r.get(6)?,version:r.get(7)?,submission:r.get(8)?,desired:r.get(9)?,expected:r.get(10)?,ack:r.get(11)?,outcome:r.get(12)?})).optional().map_err(bad)
}
fn decode_submission(
    r: Raw,
    scope: UserScopeId,
    request: BotRequestId,
) -> Result<BotSubmissionReceipt, BotError> {
    let kind = BotOperationKind::parse(&r.kind)
        .filter(|k| matches!(k, BotOperationKind::Start | BotOperationKind::Continue))
        .ok_or(BotError::Conflict)?;
    let data = r.submission.ok_or(BotError::Storage)?;
    if data.len() > 4 * 1024 * 1024 {
        return Err(BotError::Storage);
    }
    let intent: BotSubmissionIntent = serde_json::from_str(&data).map_err(bad)?;
    intent.validate()?;
    let s = &intent.submission;
    let p = &s.provenance;
    if r.version != Some(1)
        || p.owner.user_scope != scope
        || p.submission != request
        || p.bot.to_storage() != r.bot
        || r.run.as_deref() != Some(s.request.run.to_storage().as_str())
        || r.config != Some(p.config_revision)
        || r.policy != Some(p.policy_revision)
        || (kind == BotOperationKind::Continue) != p.continued_from.is_some()
        || r.desired.is_some()
        || r.ack.is_some()
    {
        return Err(BotError::Storage);
    }
    let outcome = r
        .outcome
        .map(|v| {
            if v.len() > 1024 * 1024 {
                return Err(BotError::Storage);
            }
            let out: BotExecutionOutcome = serde_json::from_str(&v).map_err(bad)?;
            out.validate(&intent)?;
            Ok(out)
        })
        .transpose()?;
    let stage = BotReceiptStage::parse(&r.stage).ok_or(BotError::Storage)?;
    let valid = match (&stage, &outcome) {
        (BotReceiptStage::Prepared, None | Some(BotExecutionOutcome::RefusalFence { .. })) => true,
        (
            BotReceiptStage::Accepted | BotReceiptStage::Completed,
            Some(BotExecutionOutcome::Accepted { .. }),
        ) => true,
        (BotReceiptStage::Rejected, Some(BotExecutionOutcome::Rejected { .. })) => true,
        _ => false,
    };
    if !valid {
        return Err(BotError::Storage);
    }
    Ok(BotSubmissionReceipt {
        intent,
        fingerprint: r.fingerprint,
        kind,
        stage,
        outcome,
    })
}
pub(crate) fn read_bot_submission_on(
    conn: &Connection,
    scope: UserScopeId,
    request: BotRequestId,
) -> Result<Option<BotSubmissionReceipt>, BotError> {
    let receipt = raw(conn, scope, request)?
        .map(|r| decode_submission(r, scope, request))
        .transpose()?;
    if let Some(r) = &receipt {
        let p = &r.intent.submission.provenance;
        guard(
            conn,
            BotOwner {
                user_scope: p.owner.user_scope,
                device: p.owner.device,
            },
            p.bot,
        )?;
    }
    Ok(receipt)
}
fn pause_on(
    conn: &Connection,
    scope: UserScopeId,
    request: BotRequestId,
) -> Result<Option<BotPauseReceipt>, BotError> {
    let Some(r) = raw(conn, scope, request)? else {
        return Ok(None);
    };
    if !matches!(r.kind.as_str(), "pause" | "resume") {
        return Err(BotError::Conflict);
    }
    let data = r.outcome.ok_or(BotError::Storage)?;
    if data.len() > 1024 * 1024 {
        return Err(BotError::Storage);
    }
    let intent: BotPauseIntent = serde_json::from_str(&data).map_err(bad)?;
    intent.validate()?;
    let b = &intent.barrier;
    guard(
        conn,
        BotOwner {
            user_scope: b.owner.user_scope,
            device: b.owner.device,
        },
        b.bot,
    )?;
    if b.owner.user_scope != scope
        || b.request != request
        || b.bot.to_storage() != r.bot
        || r.desired != Some(b.paused)
        || r.expected != Some(intent.expected_revision)
        || (r.kind == "pause") != b.paused
        || r.run.is_some()
        || r.submission.is_some()
    {
        return Err(BotError::Storage);
    }
    let acknowledged = match r.ack.as_deref() {
        Some("pending") => false,
        Some("acknowledged") => true,
        _ => return Err(BotError::Storage),
    };
    let stage = BotReceiptStage::parse(&r.stage).ok_or(BotError::Storage)?;
    if (acknowledged && stage != BotReceiptStage::Completed)
        || (!acknowledged && stage != BotReceiptStage::Prepared)
    {
        return Err(BotError::Storage);
    }
    Ok(Some(BotPauseReceipt {
        intent,
        fingerprint: r.fingerprint,
        stage,
        acknowledged,
    }))
}
fn fingerprint(value: &str) -> Result<(), BotError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(BotError::Invalid);
    }
    Ok(())
}
pub(super) fn require_pause_settled(
    conn: &Connection,
    scope: UserScopeId,
    bot: BotId,
) -> Result<(), BotError> {
    let pending:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND command_kind IN ('pause','resume') AND stage='prepared')",params![scope.to_storage(),bot.to_storage()],|r|r.get(0)).map_err(bad)?;
    if pending {
        Err(BotError::Conflict)
    } else {
        Ok(())
    }
}
struct Guard {
    revision: u64,
    policy: u64,
    paused: bool,
    folder: String,
    lock: Option<String>,
    frozen: Option<i64>,
    home: WorkspaceId,
}
fn guard(conn: &Connection, owner: BotOwner, bot: BotId) -> Result<Guard, BotError> {
    let r:Option<(String,String,String,u64,u64,bool,String,Option<String>,Option<i64>,String)>=conn.query_row("SELECT user_scope_id,owner_device_id,lifecycle,revision,policy_revision,paused,working_folder,folder_lock_request_id,folder_frozen_at_ms,home_workspace_id FROM bots WHERE bot_id=?1",
        [bot.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).optional().map_err(bad)?;
    let r = r.ok_or(BotError::NotFound)?;
    if r.0 != owner.user_scope.to_storage() || r.1 != owner.device.to_storage() || r.2 != "active" {
        return Err(BotError::Forbidden);
    }
    Ok(Guard {
        revision: r.3,
        policy: r.4,
        paused: r.5,
        folder: r.6,
        lock: r.7,
        frozen: r.8,
        home: WorkspaceId::parse(&r.9).ok_or(BotError::Storage)?,
    })
}

/// P1.5 composes this with its PendingTurn/association insert in one transaction.
/// It does not open a connection or commit the caller's transaction.
pub(crate) fn prepare_bot_submission_tx(
    tx: &Transaction<'_>,
    intent: &BotSubmissionIntent,
    fingerprint_value: &str,
    expected_revision: u64,
    now: i64,
) -> Result<BotSubmissionReceipt, BotError> {
    intent.validate()?;
    fingerprint(fingerprint_value)?;
    let s = &intent.submission;
    let p = &s.provenance;
    let owner = BotOwner {
        user_scope: p.owner.user_scope,
        device: p.owner.device,
    };
    let g = guard(tx, owner, p.bot)?;
    if let Some(existing) = read_bot_submission_on(tx, owner.user_scope, p.submission)? {
        return if existing.intent == *intent && existing.fingerprint == fingerprint_value {
            Ok(existing)
        } else {
            Err(BotError::Conflict)
        };
    }
    super::policy::require_policy_settled(tx, p.bot)?;
    if expected_revision != g.revision
        || p.config_revision != g.revision
        || p.policy_revision != g.policy
    {
        return Err(BotError::RevisionConflict);
    }
    require_pause_settled(tx, owner.user_scope, p.bot)?;
    // This is an unacknowledged-intent bound, not a second runtime queue.
    // Exact durable retries above remain possible when the bound is full.
    let unresolved:u64=tx.query_row("SELECT COUNT(*) FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND command_kind IN ('start','continue') AND stage='prepared'",params![owner.user_scope.to_storage(),p.bot.to_storage()],|r|r.get(0)).map_err(bad)?;
    if unresolved >= 50 {
        return Err(BotError::Conflict);
    }
    if g.paused {
        return Err(BotError::Forbidden);
    }
    if s.workspace != g.home || s.policy.destination != g.home {
        return Err(BotError::Forbidden);
    }
    let access: String = tx
        .query_row(
            "SELECT access FROM bots WHERE bot_id=?1",
            [p.bot.to_storage()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    let captured_access = match s.policy.access {
        koru_delegate_core::BotAccess::Default => "default",
        koru_delegate_core::BotAccess::Workspace => "workspace",
    };
    if access != captured_access {
        return Err(BotError::Conflict);
    }
    let mut query = tx
        .prepare("SELECT workspace_id,document_id FROM bot_selected_pages WHERE bot_id=?1")
        .map_err(bad)?;
    let selected = query
        .query_map([p.bot.to_storage()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(bad)?
        .map(|r| {
            let (workspace, document) = r.map_err(bad)?;
            if workspace != g.home.to_storage() {
                return Err(BotError::Storage);
            }
            DocumentId::parse(&document).ok_or(BotError::Storage)
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    if selected != s.policy.selected_pages {
        return Err(BotError::Conflict);
    }
    if !matches!(&s.request.scope,WorkspaceScope::BotFolder{folder} if folder.as_str()==g.folder) {
        return Err(BotError::FolderLocked);
    }
    let associated: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_chats WHERE bot_id=?1 AND chat_id=?2)",
            params![p.bot.to_storage(), p.chat_id.to_string()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if !associated {
        return Err(BotError::Forbidden);
    }
    if g.frozen.is_none() {
        if g.lock
            .as_deref()
            .is_some_and(|key| key != p.submission.to_storage())
        {
            return Err(BotError::FolderLocked);
        }
        // Also refuse an inconsistent old pending receipt; absence of its lock
        // is not authority to submit a second first-task binding.
        let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE bot_id=?1 AND user_scope_id=?2 AND command_kind IN ('start','continue') AND stage IN ('prepared','accepted'))",params![p.bot.to_storage(),owner.user_scope.to_storage()],|r|r.get(0)).map_err(bad)?;
        if pending {
            return Err(BotError::FolderLocked);
        }
        tx.execute(
            "UPDATE bots SET folder_lock_request_id=?2 WHERE bot_id=?1",
            params![p.bot.to_storage(), p.submission.to_storage()],
        )
        .map_err(bad)?;
    }
    let kind = if p.continued_from.is_some() {
        BotOperationKind::Continue
    } else {
        BotOperationKind::Start
    };
    let payload = serde_json::to_string(intent).map_err(bad)?;
    tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'prepared',?6,?7,?8,1,?9,?10,?10)",
        params![owner.user_scope.to_storage(),p.submission.to_storage(),p.bot.to_storage(),fingerprint_value,kind.token(),s.request.run.to_storage(),p.config_revision,p.policy_revision,payload,now]).map_err(bad)?;
    read_bot_submission_on(tx, owner.user_scope, p.submission)?.ok_or(BotError::Storage)
}

impl Repository {
    pub fn bot_submission_for_run(
        &self,
        owner: BotOwner,
        bot: BotId,
        run: koru_delegate_core::RunId,
    ) -> Result<Option<BotSubmissionReceipt>, BotError> {
        guard(&self.connection, owner, bot)?;
        let request:Option<String>=self.connection.query_row("SELECT request_id FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND planned_run_id=?3",params![owner.user_scope.to_storage(),bot.to_storage(),run.to_storage()],|r|r.get(0)).optional().map_err(bad)?;
        request
            .map(|r| {
                read_bot_submission_on(&self.connection, owner.user_scope, r.parse().map_err(bad)?)
            })
            .transpose()
            .map(Option::flatten)
    }
    pub fn prepare_bot_submission(
        &mut self,
        intent: &BotSubmissionIntent,
        fingerprint: &str,
        expected: u64,
        now: i64,
    ) -> Result<BotSubmissionReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let result = prepare_bot_submission_tx(&tx, intent, fingerprint, expected, now)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }
    pub fn bot_admission_receipt(
        &self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
    ) -> Result<Option<BotAdmissionReceipt>, BotError> {
        guard(&self.connection, owner, bot)?;
        let Some(row) = raw(&self.connection, owner.user_scope, request)? else {
            return Ok(None);
        };
        if row.bot != bot.to_storage() {
            return Err(BotError::Forbidden);
        }
        if matches!(row.kind.as_str(), "start" | "continue") {
            Ok(
                read_bot_submission_on(&self.connection, owner.user_scope, request)?
                    .map(BotAdmissionReceipt::Submission),
            )
        } else {
            Ok(pause_on(&self.connection, owner.user_scope, request)?
                .map(BotAdmissionReceipt::Pause))
        }
    }
    pub fn acknowledge_bot_submission(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        admission: &BotAdmissionView,
        now: i64,
    ) -> Result<BotSubmissionReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let g = guard(&tx, owner, bot)?;
        let r =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::NotFound)?;
        if r.intent.submission.provenance.bot != bot || r.stage == BotReceiptStage::Rejected {
            return Err(BotError::Conflict);
        }
        let outcome = BotExecutionOutcome::Accepted {
            version: 1,
            admission: admission.clone(),
        };
        outcome.validate(&r.intent)?;
        if g.frozen.is_none() && g.lock.as_deref() != Some(request.to_storage().as_str()) {
            return Err(BotError::FolderLocked);
        }
        tx.execute("UPDATE bots SET folder_frozen_at_ms=COALESCE(folder_frozen_at_ms,?2),folder_lock_request_id=NULL WHERE bot_id=?1",params![bot.to_storage(),now]).map_err(bad)?;
        tx.execute("UPDATE bot_command_receipts SET stage='completed',outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2",params![owner.user_scope.to_storage(),request.to_storage(),serde_json::to_string(&outcome).map_err(bad)?,now]).map_err(bad)?;
        let result =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }
    pub fn prepare_bot_pause(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        expected: u64,
        paused: bool,
        fingerprint_value: &str,
        now: i64,
    ) -> Result<BotPauseReceipt, BotError> {
        fingerprint(fingerprint_value)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let g = guard(&tx, owner, bot)?;
        if let Some(r) = pause_on(&tx, owner.user_scope, request)? {
            return if r.intent.barrier.bot == bot
                && r.intent.expected_revision == expected
                && r.intent.barrier.paused == paused
                && r.fingerprint == fingerprint_value
            {
                Ok(r)
            } else {
                Err(BotError::Conflict)
            };
        }
        super::policy::require_policy_settled(&tx, bot)?;
        if expected != g.revision || expected == 0 || expected >= i64::MAX as u64 {
            return Err(BotError::RevisionConflict);
        }
        require_pause_settled(&tx, owner.user_scope, bot)?;
        let intent = BotPauseIntent {
            version: 1,
            expected_revision: expected,
            barrier: BotAdmissionBarrier {
                owner: koru_delegate_core::BotOwner {
                    user_scope: owner.user_scope,
                    device: owner.device,
                },
                bot,
                request,
                epoch: expected + 1,
                paused,
            },
        };
        tx.execute(
            "UPDATE bots SET paused=?2,revision=?3,updated_at_ms=?4 WHERE bot_id=?1",
            params![bot.to_storage(), paused, intent.barrier.epoch, now],
        )
        .map_err(bad)?;
        tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,desired_pause,expected_revision,pause_ack_stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'prepared',?6,?7,'pending',?8,?9,?9)",params![owner.user_scope.to_storage(),request.to_storage(),bot.to_storage(),fingerprint_value,if paused {"pause"}else{"resume"},paused,expected,serde_json::to_string(&intent).map_err(bad)?,now]).map_err(bad)?;
        let result = pause_on(&tx, owner.user_scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }
    pub fn acknowledge_bot_pause(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        ack: &BotAdmissionBarrier,
        now: i64,
    ) -> Result<BotPauseReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        guard(&tx, owner, bot)?;
        let r = pause_on(&tx, owner.user_scope, request)?.ok_or(BotError::NotFound)?;
        if r.intent.barrier != *ack || r.intent.barrier.bot != bot {
            return Err(BotError::Conflict);
        }
        tx.execute("UPDATE bot_command_receipts SET stage='completed',pause_ack_stage='acknowledged',updated_at_ms=?3 WHERE user_scope_id=?1 AND request_id=?2",params![owner.user_scope.to_storage(),request.to_storage(),now]).map_err(bad)?;
        let result = pause_on(&tx, owner.user_scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }
    pub fn prepare_bot_refusal_fence(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        reason: BotAdmissionRefusal,
        renew: bool,
        now: i64,
    ) -> Result<BotSubmissionReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let g = guard(&tx, owner, bot)?;
        let r =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::NotFound)?;
        if r.intent.submission.provenance.bot != bot {
            return Err(BotError::Forbidden);
        }
        if r.stage != BotReceiptStage::Prepared {
            return Ok(r);
        }
        require_pause_settled(&tx, owner.user_scope, bot)?;
        let round = match &r.outcome {
            Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) if renew => {
                if g.revision <= barrier.epoch {
                    return Err(BotError::Conflict);
                }
                round.checked_add(1).ok_or(BotError::Conflict)?
            }
            Some(BotExecutionOutcome::RefusalFence { .. }) => return Ok(r),
            _ => 1,
        };
        super::policy::require_policy_settled(&tx, bot)?;
        if g.revision >= i64::MAX as u64 {
            return Err(BotError::RevisionConflict);
        }
        let barrier = BotAdmissionBarrier {
            owner: koru_delegate_core::BotOwner {
                user_scope: owner.user_scope,
                device: owner.device,
            },
            bot,
            request: BotRequestId::mint(),
            epoch: g.revision + 1,
            paused: g.paused,
        };
        let outcome = BotExecutionOutcome::RefusalFence {
            version: 1,
            round,
            barrier,
            acknowledged: false,
            reason,
        };
        outcome.validate(&r.intent)?;
        tx.execute(
            "UPDATE bots SET revision=?2,updated_at_ms=?3 WHERE bot_id=?1",
            params![bot.to_storage(), g.revision + 1, now],
        )
        .map_err(bad)?;
        tx.execute("UPDATE bot_command_receipts SET outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2",params![owner.user_scope.to_storage(),request.to_storage(),serde_json::to_string(&outcome).map_err(bad)?,now]).map_err(bad)?;
        let result =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }
    pub fn acknowledge_bot_refusal_fence(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        round: u32,
        ack: &BotAdmissionBarrier,
        now: i64,
    ) -> Result<BotSubmissionReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        guard(&tx, owner, bot)?;
        let mut r =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::NotFound)?;
        if r.stage != BotReceiptStage::Prepared {
            return Err(BotError::Conflict);
        }
        match r.outcome.as_mut() {
            Some(BotExecutionOutcome::RefusalFence {
                round: stored,
                barrier,
                acknowledged,
                ..
            }) if *stored == round && *barrier == *ack && barrier.bot == bot => {
                *acknowledged = true
            }
            _ => return Err(BotError::Conflict),
        }
        tx.execute("UPDATE bot_command_receipts SET outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2",params![owner.user_scope.to_storage(),request.to_storage(),serde_json::to_string(&r.outcome).map_err(bad)?,now]).map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(r)
    }
    pub fn reject_bot_submission_after_fence(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        round: u32,
        ack: &BotAdmissionBarrier,
        now: i64,
    ) -> Result<BotSubmissionReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let g = guard(&tx, owner, bot)?;
        let r =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::NotFound)?;
        let reason = match &r.outcome {
            Some(BotExecutionOutcome::RefusalFence {
                round: stored,
                barrier,
                acknowledged: true,
                reason,
                ..
            }) if *stored == round
                && barrier == ack
                && barrier.bot == bot
                && r.stage == BotReceiptStage::Prepared =>
            {
                *reason
            }
            _ => return Err(BotError::Conflict),
        };
        if g.frozen.is_none() {
            if g.lock.as_deref() != Some(request.to_storage().as_str()) {
                return Err(BotError::FolderLocked);
            }
            let other:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE bot_id=?1 AND user_scope_id=?2 AND request_id<>?3 AND command_kind IN ('start','continue') AND stage IN ('prepared','accepted','completed'))",params![bot.to_storage(),owner.user_scope.to_storage(),request.to_storage()],|r|r.get(0)).map_err(bad)?;
            if other {
                return Err(BotError::FolderLocked);
            }
            tx.execute("UPDATE bots SET folder_lock_request_id=NULL WHERE bot_id=?1 AND folder_lock_request_id=?2 AND folder_frozen_at_ms IS NULL",params![bot.to_storage(),request.to_storage()]).map_err(bad)?;
        }
        let outcome = BotExecutionOutcome::Rejected { version: 1, reason };
        tx.execute("UPDATE bot_command_receipts SET stage='rejected',outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2",params![owner.user_scope.to_storage(),request.to_storage(),serde_json::to_string(&outcome).map_err(bad)?,now]).map_err(bad)?;
        let result =
            read_bot_submission_on(&tx, owner.user_scope, request)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }
    /// Content-free, bounded recovery pages; no outcome or task blobs are read.
    pub fn pending_bot_operations(
        &self,
        after: i64,
        limit: u16,
    ) -> Result<Vec<(i64, BotId, UserScopeId, BotRequestId)>, BotError> {
        let mut q=self.connection.prepare("SELECT rowid,bot_id,user_scope_id,request_id FROM bot_command_receipts WHERE rowid>?1 AND stage IN ('prepared','accepted') AND command_kind IN ('start','continue','pause','resume') ORDER BY rowid LIMIT ?2").map_err(bad)?;
        q.query_map(params![after, limit.clamp(1, 64)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(bad)?
        .map(|r| {
            let (row, bot, scope, request) = r.map_err(bad)?;
            Ok((
                row,
                bot.parse().map_err(bad)?,
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                request.parse().map_err(bad)?,
            ))
        })
        .collect()
    }
}
