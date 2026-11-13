// SPDX-License-Identifier: MPL-2.0
use super::*;

fn next_token(creation: &BotMemoryCreation, now: i64) -> Result<i64, BotError> {
    Ok(now.max(
        creation
            .updated_at_ms
            .checked_add(1)
            .ok_or(BotError::Storage)?,
    ))
}

pub(super) fn creation_on(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    home: WorkspaceId,
) -> Result<Option<BotMemoryCreation>, BotError> {
    let row = conn.query_row("SELECT user_scope_id,request_id,owner_device_id,workspace_id,candidate_document_id,store_request_id,seed_format,seed_title,seed_hash,state,origin_edit_revision,refusal,created_at_ms,updated_at_ms FROM bot_memory_creation_intents WHERE bot_id=?1", [bot.to_storage()], |r| {
        Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,u32>(6)?,r.get::<_,String>(7)?,r.get::<_,Vec<u8>>(8)?,r.get::<_,String>(9)?,r.get::<_,Option<Vec<u8>>>(10)?,r.get::<_,Option<String>>(11)?,r.get::<_,i64>(12)?,r.get::<_,i64>(13)?))
    }).optional().map_err(bad)?;
    let Some(r) = row else {
        return Ok(None);
    };
    if r.0 != owner.user_scope.to_storage()
        || r.2 != owner.device.to_storage()
        || r.3 != home.to_storage()
    {
        return Err(BotError::Storage);
    }
    let creation = BotMemoryCreation {
        identity: BotMemoryIdentity {
            owner,
            bot,
            workspace: home,
            creation_request: r.1.parse().map_err(bad)?,
            candidate: DocumentId::parse(&r.4).ok_or(BotError::Storage)?,
            store_request: r.5.parse().map_err(bad)?,
            seed_format: r.6,
            seed_title: r.7,
            seed_hash: r.8.try_into().map_err(|_| BotError::Storage)?,
        },
        state: BotMemoryState::parse(&r.9).ok_or(BotError::Storage)?,
        origin_edit_revision: r.10,
        refusal: r
            .11
            .map(|v| BotMemoryRefusal::parse(&v).ok_or(BotError::Storage))
            .transpose()?,
        created_at_ms: r.12,
        updated_at_ms: r.13,
    };
    creation.validate().map_err(|_| BotError::Storage)?;
    let original = admission::raw(conn, owner.user_scope, creation.identity.creation_request)?
        .ok_or(BotError::Storage)?;
    if original.stage != creation.state.receipt_stage() {
        return Err(BotError::Storage);
    }
    let receipt = receipt_on(
        conn,
        owner,
        bot,
        creation.identity.creation_request,
        &original.fingerprint,
        "ensure_memory",
    )
    .map_err(|_| BotError::Storage)?
    .ok_or(BotError::Storage)?;
    validate_pointer(&receipt, &creation)?;
    Ok(Some(creation))
}

fn validate_pointer(
    receipt: &BotLibraryReceiptV1,
    creation: &BotMemoryCreation,
) -> Result<(), BotError> {
    match receipt {
        BotLibraryReceiptV1::EnsureMemory {
            creation_request,
            candidate,
            ..
        } if *creation_request == creation.identity.creation_request
            && candidate.workspace == creation.identity.workspace
            && candidate.document == creation.identity.candidate =>
        {
            Ok(())
        }
        _ => Err(BotError::Storage),
    }
}

pub(super) fn binding_on(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    home: WorkspaceId,
    creation: Option<&BotMemoryCreation>,
) -> Result<Option<BotMemoryBinding>, BotError> {
    let row: Option<(String,String,String,String,i64)> = conn.query_row("SELECT user_scope_id,workspace_id,document_id,creation_request_id,bound_at_ms FROM bot_memory_bindings WHERE bot_id=?1",[bot.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(bad)?;
    let Some((scope, workspace, document, request, bound_at_ms)) = row else {
        if creation.is_some_and(|c| c.state == BotMemoryState::Bound) {
            return Err(BotError::Storage);
        }
        return Ok(None);
    };
    let c = creation.ok_or(BotError::Storage)?;
    if c.state != BotMemoryState::Bound
        || scope != owner.user_scope.to_storage()
        || workspace != home.to_storage()
        || document != c.identity.candidate.to_storage()
        || request != c.identity.creation_request.to_storage()
    {
        return Err(BotError::Storage);
    }
    Ok(Some(BotMemoryBinding {
        owner,
        bot,
        page: BotPage {
            workspace: home,
            document: c.identity.candidate,
        },
        creation_request: c.identity.creation_request,
        bound_at_ms,
    }))
}

fn original_stage(
    tx: &Connection,
    creation: &BotMemoryCreation,
    state: BotMemoryState,
    now: i64,
) -> Result<(), BotError> {
    let changed = tx.execute("UPDATE bot_command_receipts SET stage=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2 AND command_kind='ensure_memory' AND stage=?5",params![creation.identity.owner.user_scope.to_storage(),creation.identity.creation_request.to_storage(),state.receipt_stage(),now,creation.state.receipt_stage()]).map_err(bad)?;
    if changed != 1 {
        return Err(BotError::Storage);
    }
    Ok(())
}

impl Repository {
    /// First creation allocates identity inside this transaction. Later caller
    /// requests reserve aliases in the same shared namespace, never new candidates.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_bot_memory_creation(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        expected: u64,
        seed: &BotMemorySeed,
        fingerprint: &str,
        now: i64,
    ) -> Result<BotMemoryCreation, BotError> {
        validate_library_request(owner, bot, request, expected)?;
        admission::fingerprint(fingerprint)?;
        if seed.title.is_empty() || seed.title.len() > 256 {
            return Err(BotError::Invalid);
        }
        let tx = self.connection.transaction().map_err(bad)?;
        let g = admission::guard(&tx, owner, bot)?;
        let receipt = receipt_on(&tx, owner, bot, request, fingerprint, "ensure_memory")?;
        let existing = creation_on(&tx, owner, bot, g.home)?;
        bound_memory_on(&tx, owner, bot, g.home)?;
        if let Some(receipt) = receipt {
            let creation = existing.ok_or(BotError::Storage)?;
            validate_pointer(&receipt, &creation)?;
            // The original request's immutable body can be checked directly.
            // Alias fingerprints are computed by the trusted preparer from its
            // request body; an alias is intentionally only a resolution pointer.
            if request == creation.identity.creation_request
                && (seed.title != creation.identity.seed_title
                    || seed.hash != creation.identity.seed_hash)
            {
                return Err(BotError::Conflict);
            }
            let original_expected: u64 = tx.query_row("SELECT expected_revision FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",params![owner.user_scope.to_storage(),request.to_storage()],|r|r.get(0)).map_err(bad)?;
            if expected != original_expected {
                return Err(BotError::Conflict);
            }
            return Ok(creation);
        }
        let g = settled(&tx, owner, bot)?;
        if expected != g.revision {
            return Err(BotError::RevisionConflict);
        }
        if let Some(creation) = existing {
            let outcome = BotLibraryReceiptV1::EnsureMemory {
                version: 1,
                creation_request: creation.identity.creation_request,
                candidate: BotPage {
                    workspace: g.home,
                    document: creation.identity.candidate,
                },
            };
            insert_receipt(
                &tx,
                owner,
                bot,
                request,
                fingerprint,
                expected,
                &outcome,
                creation.state.receipt_stage(),
                now,
            )?;
            tx.commit().map_err(bad)?;
            return Ok(creation);
        }
        let identity = BotMemoryIdentity {
            owner,
            bot,
            workspace: g.home,
            creation_request: request,
            candidate: DocumentId::new(),
            store_request: BotRequestId::mint(),
            seed_format: 1,
            seed_title: seed.title.clone(),
            seed_hash: seed.hash,
        };
        identity.validate()?;
        let outcome = BotLibraryReceiptV1::EnsureMemory {
            version: 1,
            creation_request: request,
            candidate: BotPage {
                workspace: g.home,
                document: identity.candidate,
            },
        };
        insert_receipt(
            &tx,
            owner,
            bot,
            request,
            fingerprint,
            expected,
            &outcome,
            "prepared",
            now,
        )?;
        tx.execute("INSERT INTO bot_memory_creation_intents(user_scope_id,request_id,bot_id,owner_device_id,workspace_id,candidate_document_id,store_request_id,seed_format,seed_title,seed_hash,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9,'prepared',?10,?10)",params![owner.user_scope.to_storage(),request.to_storage(),bot.to_storage(),owner.device.to_storage(),g.home.to_storage(),identity.candidate.to_storage(),identity.store_request.to_storage(),seed.title,seed.hash.as_slice(),now]).map_err(bad)?;
        let result = creation_on(&tx, owner, bot, g.home)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }

    pub fn transition_bot_memory_creation(
        &mut self,
        identity: &BotMemoryIdentity,
        observed: BotMemoryObserved,
        transition: BotMemoryTransition,
        now: i64,
    ) -> Result<BotMemoryCreation, BotError> {
        identity.validate()?;
        // Unavailability is not proved disagreement with the original page.
        // Keep it owed through Unknown/ReadFailure instead of parking conflict.
        if matches!(
            transition,
            BotMemoryTransition::Disagreement(
                BotMemoryRefusal::OwnerUnavailable | BotMemoryRefusal::StoreUnavailable
            )
        ) {
            return Err(BotError::Invalid);
        }
        let tx = self.connection.transaction().map_err(bad)?;
        let g = settled(&tx, identity.owner, identity.bot)?;
        let c =
            creation_on(&tx, identity.owner, identity.bot, g.home)?.ok_or(BotError::NotFound)?;
        bound_memory_on(&tx, identity.owner, identity.bot, g.home)?;
        if c.identity != *identity || c.observed() != observed {
            return Err(BotError::Conflict);
        }
        use BotMemoryState as S;
        use BotMemoryTransition as T;
        let (state, refusal) = match (c.state, transition) {
            (S::Prepared | S::Unconfirmed, T::Unknown) => (S::Unconfirmed, None),
            (S::Prepared | S::Unconfirmed, T::ReadFailure) => return Ok(c),
            (S::Prepared | S::Unconfirmed, T::Disagreement(reason)) => (S::Conflict, Some(reason)),
            (S::Prepared | S::Unconfirmed | S::Conflict, T::Abandon) => (S::Abandoned, None),
            (S::Abandoned | S::Conflict, T::Retry) => (S::Prepared, None),
            _ => return Err(BotError::Conflict),
        };
        if state == c.state && refusal == c.refusal {
            return Ok(c);
        }
        let next = next_token(&c, now)?;
        let changed = tx.execute("UPDATE bot_memory_creation_intents SET state=?3,refusal=?4,updated_at_ms=?5 WHERE user_scope_id=?1 AND request_id=?2 AND state=?6 AND updated_at_ms=?7",params![identity.owner.user_scope.to_storage(),identity.creation_request.to_storage(),state.as_str(),refusal.map(BotMemoryRefusal::as_str),next,observed.state.as_str(),observed.updated_at_ms]).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        original_stage(&tx, &c, state, next)?;
        let result =
            creation_on(&tx, identity.owner, identity.bot, g.home)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(result)
    }

    pub fn bind_bot_memory(
        &mut self,
        evidence: &BotMemoryConfirmation,
        observed: BotMemoryObserved,
        now: i64,
    ) -> Result<BotMemoryBinding, BotError> {
        evidence.validate()?;
        let identity = &evidence.identity;
        let tx = self.connection.transaction().map_err(bad)?;
        let g = admission::guard(&tx, identity.owner, identity.bot)?;
        let c =
            creation_on(&tx, identity.owner, identity.bot, g.home)?.ok_or(BotError::NotFound)?;
        if c.identity != *identity || c.observed() != observed {
            return Err(BotError::Conflict);
        }
        if let Some(binding) = bound_memory_on(&tx, identity.owner, identity.bot, g.home)? {
            if c.origin_edit_revision.as_ref() != Some(&evidence.original_edit_revision) {
                return Err(BotError::Conflict);
            }
            return Ok(binding);
        }
        if !matches!(
            c.state,
            BotMemoryState::Prepared | BotMemoryState::Unconfirmed
        ) {
            return Err(BotError::Conflict);
        }
        let g = settled(&tx, identity.owner, identity.bot)?;
        let role: (String, String) = tx
            .query_row(
                "SELECT role_workspace_id,role_document_id FROM bots WHERE bot_id=?1",
                [identity.bot.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(bad)?;
        let page = BotPage {
            workspace: identity.workspace,
            document: identity.candidate,
        };
        if role == (page.workspace.to_storage(), page.document.to_storage())
            || knowledge_on(&tx, identity.bot, g.home)?.contains(&page)
        {
            return Err(BotError::Forbidden);
        }
        let next = next_token(&c, now)?;
        let changed = tx.execute("UPDATE bot_memory_creation_intents SET state='bound',origin_edit_revision=?3,refusal=NULL,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2 AND state=?5 AND updated_at_ms=?6",params![identity.owner.user_scope.to_storage(),identity.creation_request.to_storage(),evidence.original_edit_revision,next,observed.state.as_str(),observed.updated_at_ms]).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        tx.execute("INSERT INTO bot_memory_bindings(bot_id,user_scope_id,workspace_id,document_id,creation_request_id,bound_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",params![identity.bot.to_storage(),identity.owner.user_scope.to_storage(),identity.workspace.to_storage(),identity.candidate.to_storage(),identity.creation_request.to_storage(),next]).map_err(bad)?;
        tx.execute(
            "UPDATE bots SET revision=revision+1,updated_at_ms=max(updated_at_ms,?2) WHERE bot_id=?1",
            params![identity.bot.to_storage(), next],
        )
        .map_err(bad)?;
        original_stage(&tx, &c, BotMemoryState::Bound, next)?;
        let c = creation_on(&tx, identity.owner, identity.bot, g.home)?.ok_or(BotError::Storage)?;
        let binding = binding_on(&tx, identity.owner, identity.bot, g.home, Some(&c))?
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(binding)
    }

    /// Internal recovery inspection, including inactive Bots. This grants no
    /// mutation authority. A later transition must pass current admission again.
    pub fn pending_bot_memory(
        &self,
        owner: BotOwner,
        cursor: Option<&BotMemoryPendingCursor>,
        limit: u16,
    ) -> Result<BotMemoryPendingPage, BotError> {
        if limit == 0 || limit > 16 || cursor.is_some_and(|c| c.owner != owner) {
            return Err(BotError::Invalid);
        }
        let mut query = self.connection.prepare("SELECT i.bot_id,i.updated_at_ms,i.request_id FROM bot_memory_creation_intents i INDEXED BY bot_memory_creation_pending JOIN bots b ON b.bot_id=i.bot_id WHERE i.state IN ('prepared','unconfirmed') AND i.user_scope_id=?1 AND i.owner_device_id=?2 AND b.user_scope_id=?1 AND b.owner_device_id=?2 AND (?3 IS NULL OR (i.updated_at_ms,i.request_id)>(?3,?4)) ORDER BY i.updated_at_ms,i.request_id LIMIT ?5").map_err(bad)?;
        let rows = query
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    cursor.map(|c| c.updated_at_ms),
                    cursor.map(|c| c.request.to_storage()),
                    limit + 1
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        let more = rows.len() > usize::from(limit);
        let mut creations = Vec::new();
        for (bot, _, _) in rows.iter().take(usize::from(limit)) {
            let bot = bot.parse().map_err(bad)?;
            let (g, _) = admission::guard_any_lifecycle(&self.connection, owner, bot)?;
            let c = creation_on(&self.connection, owner, bot, g.home)?.ok_or(BotError::Storage)?;
            binding_on(&self.connection, owner, bot, g.home, Some(&c))?;
            creations.push(c);
        }
        let next = if more {
            creations.last().map(|c| BotMemoryPendingCursor {
                owner,
                updated_at_ms: c.updated_at_ms,
                request: c.identity.creation_request,
            })
        } else {
            None
        };
        Ok(BotMemoryPendingPage { creations, next })
    }
}
