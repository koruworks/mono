// SPDX-License-Identifier: MPL-2.0

//! Library persistence on the existing AI writer. These methods perform no Store
//! effects. Prepared inputs and confirmation evidence require a trusted caller.
use super::*;
use rusqlite::Transaction;

/// Compose capture with admission on the caller's transaction. A legacy
/// receipt cannot be upgraded by supplying today's Library after the fact.
pub(crate) fn prepare_library_submission_tx(
    tx: &Transaction<'_>,
    intent: &BotSubmissionIntent,
    capture: &BotLibraryCapture,
    fingerprint: &str,
    expected: u64,
    now: i64,
) -> Result<BotSubmissionReceipt, BotError> {
    let p = &intent.submission.provenance;
    if capture.owner.user_scope != p.owner.user_scope
        || capture.owner.device != p.owner.device
        || capture.bot != p.bot
        || capture.submission != p.submission
        || capture.config_revision != p.config_revision
    {
        return Err(BotError::Conflict);
    }
    if admission::raw(tx, p.owner.user_scope, p.submission)?.is_some() {
        let has_capture: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_library_captures WHERE user_scope_id=?1 AND submission_request_id=?2)",
            params![p.owner.user_scope.to_storage(), p.submission.to_storage()], |r| r.get(0)).map_err(bad)?;
        if !has_capture {
            return Err(BotError::Conflict);
        }
    }
    let receipt = admission::prepare_bot_submission_tx(tx, intent, fingerprint, expected, now)?;
    insert_library_capture_tx(tx, capture, now)?;
    Ok(receipt)
}
mod capture;
mod memory;
pub(crate) use capture::insert_library_capture_tx;
#[cfg(test)]
mod tests;

fn visible(conn: &Connection, owner: BotOwner, bot: BotId) -> Result<admission::Guard, BotError> {
    let (guard, state) = admission::guard_any_lifecycle(conn, owner, bot)?;
    if state == BotLifecycle::Trashed {
        return Err(BotError::NotFound);
    }
    Ok(guard)
}

fn settled(conn: &Connection, owner: BotOwner, bot: BotId) -> Result<admission::Guard, BotError> {
    let guard = admission::guard(conn, owner, bot)?;
    policy::require_policy_settled(conn, bot)?;
    admission::require_pause_settled(conn, owner.user_scope, bot)?;
    if lifecycle::whole_bot_operation_open(conn, bot)? {
        return Err(BotError::Conflict);
    }
    if guard.revision >= i64::MAX as u64 {
        return Err(BotError::RevisionConflict);
    }
    Ok(guard)
}

fn knowledge_on(
    conn: &Connection,
    bot: BotId,
    home: WorkspaceId,
) -> Result<Vec<BotPage>, BotError> {
    let mut query = conn.prepare("SELECT ordinal,workspace_id,document_id FROM bot_knowledge_pages WHERE bot_id=?1 ORDER BY ordinal LIMIT 17").map_err(bad)?;
    let rows = query
        .query_map([bot.to_storage()], |r| {
            Ok((
                r.get::<_, usize>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(bad)?;
    let mut pages = Vec::new();
    for row in rows {
        let (ordinal, workspace, document) = row.map_err(bad)?;
        if ordinal != pages.len() {
            return Err(BotError::Storage);
        }
        pages.push(BotPage {
            workspace: WorkspaceId::parse(&workspace).ok_or(BotError::Storage)?,
            document: DocumentId::parse(&document).ok_or(BotError::Storage)?,
        });
    }
    validate_knowledge(&pages, home).map_err(|_| BotError::Storage)?;
    Ok(pages)
}

fn receipt_on(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
    fingerprint: &str,
    kind: &str,
) -> Result<Option<BotLibraryReceiptV1>, BotError> {
    let Some(raw) = admission::raw(conn, owner.user_scope, request)? else {
        return Ok(None);
    };
    if raw.kind != kind || raw.bot != bot.to_storage() || raw.fingerprint != fingerprint {
        return Err(BotError::Conflict);
    }
    let text = raw.outcome.ok_or(BotError::Storage)?;
    if text.len() > BOT_LIBRARY_RECEIPT_LIMIT {
        return Err(BotError::Storage);
    }
    let receipt: BotLibraryReceiptV1 = serde_json::from_str(&text).map_err(bad)?;
    receipt.validate().map_err(|_| BotError::Storage)?;
    match &receipt {
        BotLibraryReceiptV1::SetKnowledge { .. }
            if kind == "set_knowledge" && raw.stage == "completed" => {}
        BotLibraryReceiptV1::EnsureMemory { .. }
            if kind == "ensure_memory"
                && matches!(raw.stage.as_str(), "prepared" | "completed" | "rejected") => {}
        _ => return Err(BotError::Storage),
    }
    Ok(Some(receipt))
}

#[allow(clippy::too_many_arguments)]
fn insert_receipt(
    tx: &Connection,
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
    fingerprint: &str,
    expected: u64,
    outcome: &BotLibraryReceiptV1,
    stage: &str,
    now: i64,
) -> Result<(), BotError> {
    outcome.validate()?;
    let kind = match outcome {
        BotLibraryReceiptV1::SetKnowledge { .. } => "set_knowledge",
        BotLibraryReceiptV1::EnsureMemory { .. } => "ensure_memory",
    };
    tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)", params![owner.user_scope.to_storage(),request.to_storage(),bot.to_storage(),fingerprint,kind,stage,expected,serde_json::to_string(outcome).map_err(bad)?,now]).map_err(bad)?;
    Ok(())
}

impl Repository {
    /// API task admission, folder reservation and exact Library capture commit
    /// together. Existing legacy receipts retain their original replay path.
    pub fn prepare_bot_submission_with_library(
        &mut self,
        intent: &BotSubmissionIntent,
        capture: &BotLibraryCapture,
        fingerprint: &str,
        expected: u64,
        now: i64,
    ) -> Result<BotSubmissionReceipt, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(bad)?;
        let receipt =
            prepare_library_submission_tx(&tx, intent, capture, fingerprint, expected, now)?;
        tx.commit().map_err(bad)?;
        Ok(receipt)
    }

    /// Exact shared-journal lookup before a trusted caller inspects page
    /// availability. Current owner/lifecycle still apply; this grants no page
    /// authority and never admits a new mutation.
    pub fn bot_knowledge_receipt(
        &self,
        input: &PreparedBotKnowledge,
        fingerprint: &str,
    ) -> Result<Option<BotLibraryReceiptV1>, BotError> {
        validate_library_request(
            input.owner,
            input.bot,
            input.request,
            input.expected_revision,
        )?;
        admission::fingerprint(fingerprint)?;
        let g = admission::guard(&self.connection, input.owner, input.bot)?;
        validate_knowledge(&input.pages, g.home)?;
        let receipt = receipt_on(
            &self.connection,
            input.owner,
            input.bot,
            input.request,
            fingerprint,
            "set_knowledge",
        )?;
        if let Some(BotLibraryReceiptV1::SetKnowledge {
            revision, pages, ..
        }) = &receipt
        {
            if *revision
                != input
                    .expected_revision
                    .checked_add(1)
                    .ok_or(BotError::Invalid)?
                || *pages != input.pages
            {
                return Err(BotError::Conflict);
            }
        }
        Ok(receipt)
    }

    pub fn bot_library(&self, owner: BotOwner, bot: BotId) -> Result<BotLibrary, BotError> {
        let g = visible(&self.connection, owner, bot)?;
        let knowledge = knowledge_on(&self.connection, bot, g.home)?;
        let creation = memory::creation_on(&self.connection, owner, bot, g.home)?;
        let binding = memory::binding_on(&self.connection, owner, bot, g.home, creation.as_ref())?;
        if binding
            .as_ref()
            .is_some_and(|b| knowledge.contains(&b.page))
        {
            return Err(BotError::Storage);
        }
        Ok(BotLibrary {
            knowledge,
            creation,
            binding,
        })
    }

    /// The trusted preparation caller must have checked current Store read
    /// authority. This transaction checks all canonical AI facts again.
    pub fn set_bot_knowledge(
        &mut self,
        input: &PreparedBotKnowledge,
        fingerprint: &str,
        now: i64,
    ) -> Result<BotLibraryReceiptV1, BotError> {
        validate_library_request(
            input.owner,
            input.bot,
            input.request,
            input.expected_revision,
        )?;
        admission::fingerprint(fingerprint)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let g = admission::guard(&tx, input.owner, input.bot)?;
        validate_knowledge(&input.pages, g.home)?;
        if let Some(receipt) = receipt_on(
            &tx,
            input.owner,
            input.bot,
            input.request,
            fingerprint,
            "set_knowledge",
        )? {
            let BotLibraryReceiptV1::SetKnowledge {
                revision, pages, ..
            } = &receipt
            else {
                return Err(BotError::Storage);
            };
            if *revision
                != input
                    .expected_revision
                    .checked_add(1)
                    .ok_or(BotError::Invalid)?
                || *pages != input.pages
            {
                return Err(BotError::Conflict);
            }
            return Ok(receipt);
        }
        let g = settled(&tx, input.owner, input.bot)?;
        if input.expected_revision != g.revision {
            return Err(BotError::RevisionConflict);
        }
        let creation = memory::creation_on(&tx, input.owner, input.bot, g.home)?;
        let binding = memory::binding_on(&tx, input.owner, input.bot, g.home, creation.as_ref())?;
        if binding.is_some_and(|b| input.pages.contains(&b.page)) {
            return Err(BotError::Forbidden);
        }
        tx.execute(
            "DELETE FROM bot_knowledge_pages WHERE bot_id=?1",
            [input.bot.to_storage()],
        )
        .map_err(bad)?;
        for (ordinal, page) in input.pages.iter().enumerate() {
            tx.execute("INSERT INTO bot_knowledge_pages(bot_id,workspace_id,document_id,ordinal,added_at_ms) VALUES(?1,?2,?3,?4,?5)",params![input.bot.to_storage(),page.workspace.to_storage(),page.document.to_storage(),ordinal,now]).map_err(bad)?;
        }
        tx.execute(
            "UPDATE bots SET revision=revision+1,updated_at_ms=?2 WHERE bot_id=?1",
            params![input.bot.to_storage(), now],
        )
        .map_err(bad)?;
        let outcome = BotLibraryReceiptV1::SetKnowledge {
            version: 1,
            revision: g.revision + 1,
            pages: input.pages.clone(),
        };
        insert_receipt(
            &tx,
            input.owner,
            input.bot,
            input.request,
            fingerprint,
            input.expected_revision,
            &outcome,
            "completed",
            now,
        )?;
        tx.commit().map_err(bad)?;
        Ok(outcome)
    }
}
