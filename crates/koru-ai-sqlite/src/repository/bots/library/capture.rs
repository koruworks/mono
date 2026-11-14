// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::{DocumentVersionId, RunContextItem};

fn validate_links(
    conn: &Connection,
    capture: &BotLibraryCapture,
    home: WorkspaceId,
) -> Result<(), BotError> {
    capture.validate(home).map_err(|_| BotError::Storage)?;
    let raw = admission::raw(conn, capture.owner.user_scope, capture.submission)?
        .ok_or(BotError::Storage)?;
    let receipt = admission::decode_submission(raw, capture.owner.user_scope, capture.submission)
        .map_err(|_| BotError::Storage)?;
    let submission = receipt.intent.submission;
    let p = &submission.provenance;
    if p.owner.user_scope != capture.owner.user_scope
        || p.owner.device != capture.owner.device
        || p.bot != capture.bot
        || p.config_revision != capture.config_revision
        || submission.workspace != home
    {
        return Err(BotError::Storage);
    }
    for page in &capture.knowledge {
        if !submission.request.context.iter().any(|pin|matches!(pin,RunContextItem::Document {workspace,document,version} if *workspace == page.page.workspace && *document == page.page.document && *version == page.version)) { return Err(BotError::Storage); }
    }
    if let BotCapturedMemory::Captured {
        page,
        creation_request,
        ..
    } = &capture.memory
    {
        let creation = memory::creation_on(conn, capture.owner, capture.bot, home)?;
        let binding =
            memory::binding_on(conn, capture.owner, capture.bot, home, creation.as_ref())?
                .ok_or(BotError::Storage)?;
        if binding.page != *page || binding.creation_request != *creation_request {
            return Err(BotError::Storage);
        }
    }
    Ok(())
}

pub(crate) fn capture_on(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    submission: BotRequestId,
    home: WorkspaceId,
) -> Result<Option<BotLibraryCapture>, BotError> {
    let row = conn.query_row("SELECT bot_id,capture_version,config_revision,memory_state,memory_workspace_id,memory_document_id,memory_history_version_id,memory_binding_request_id,index_digest,indexed_count,omitted_count,invalid_count,knowledge_count FROM bot_library_captures WHERE user_scope_id=?1 AND submission_request_id=?2",params![owner.user_scope.to_storage(),submission.to_storage()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u32>(1)?,r.get::<_,u64>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Vec<u8>>(8)?,r.get::<_,usize>(9)?,r.get::<_,u64>(10)?,r.get::<_,u64>(11)?,r.get::<_,usize>(12)?))).optional().map_err(bad)?;
    let Some(r) = row else {
        return Ok(None);
    };
    if r.0 != bot.to_storage() {
        return Err(BotError::Storage);
    }
    let mut query = conn.prepare("SELECT ordinal,workspace_id,document_id,history_version_id FROM bot_knowledge_captures WHERE user_scope_id=?1 AND submission_request_id=?2 ORDER BY ordinal LIMIT 17").map_err(bad)?;
    let rows = query
        .query_map(
            params![owner.user_scope.to_storage(), submission.to_storage()],
            |r| {
                Ok((
                    r.get::<_, usize>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(bad)?;
    let mut knowledge = Vec::new();
    for row in rows {
        let (ordinal, workspace, document, version) = row.map_err(bad)?;
        if ordinal != knowledge.len() {
            return Err(BotError::Storage);
        }
        knowledge.push(BotKnowledgeCapture {
            page: BotPage {
                workspace: WorkspaceId::parse(&workspace).ok_or(BotError::Storage)?,
                document: DocumentId::parse(&document).ok_or(BotError::Storage)?,
            },
            version: DocumentVersionId::parse(&version).ok_or(BotError::Storage)?,
        });
    }
    if knowledge.len() != r.12 {
        return Err(BotError::Storage);
    }
    let mut query = conn.prepare("SELECT ordinal,node_id FROM bot_memory_index_nodes WHERE user_scope_id=?1 AND submission_request_id=?2 ORDER BY ordinal LIMIT 257").map_err(bad)?;
    let rows = query
        .query_map(
            params![owner.user_scope.to_storage(), submission.to_storage()],
            |r| Ok((r.get::<_, usize>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(bad)?;
    let mut nodes = Vec::new();
    for row in rows {
        let (ordinal, node) = row.map_err(bad)?;
        if ordinal != nodes.len() {
            return Err(BotError::Storage);
        }
        nodes.push(node.parse::<BotMemoryNodeId>().map_err(bad)?);
    }
    if r.9 != nodes.len() {
        return Err(BotError::Storage);
    }
    let memory = match (r.3.as_str(), r.4, r.5, r.6, r.7) {
        ("absent", None, None, None, None) if nodes.is_empty() && r.10 == 0 && r.11 == 0 => {
            BotCapturedMemory::Absent {}
        }
        ("captured", Some(workspace), Some(document), Some(version), Some(request)) => {
            BotCapturedMemory::Captured {
                page: BotPage {
                    workspace: WorkspaceId::parse(&workspace).ok_or(BotError::Storage)?,
                    document: DocumentId::parse(&document).ok_or(BotError::Storage)?,
                },
                version: DocumentVersionId::parse(&version).ok_or(BotError::Storage)?,
                creation_request: request.parse().map_err(bad)?,
                nodes,
                omitted_count: r.10,
                invalid_count: r.11,
            }
        }
        _ => return Err(BotError::Storage),
    };
    let capture = BotLibraryCapture {
        version: r.1,
        owner,
        bot,
        submission,
        config_revision: r.2,
        knowledge,
        memory,
        index_digest: r.8.try_into().map_err(|_| BotError::Storage)?,
    };
    validate_links(conn, &capture, home)?;
    Ok(Some(capture))
}

/// Compose with submission/PendingTurn admission on its existing transaction.
/// There is intentionally no separately committing public insertion method.
pub(crate) fn insert_library_capture_tx(
    tx: &Transaction<'_>,
    capture: &BotLibraryCapture,
    now: i64,
) -> Result<BotLibraryCapture, BotError> {
    let g = admission::guard(tx, capture.owner, capture.bot)?;
    capture.validate(g.home)?;
    if let Some(existing) = capture_on(tx, capture.owner, capture.bot, capture.submission, g.home)?
    {
        return if existing == *capture {
            Ok(existing)
        } else {
            Err(BotError::Conflict)
        };
    }
    if capture.config_revision != g.revision {
        return Err(BotError::RevisionConflict);
    }
    // These are admission-only checks. Historical captures retain their exact
    // ordered references even after today's Library or binding changes.
    let current_knowledge = knowledge_on(tx, capture.bot, g.home)?;
    if current_knowledge
        != capture
            .knowledge
            .iter()
            .map(|p| p.page.clone())
            .collect::<Vec<_>>()
    {
        return Err(BotError::Conflict);
    }
    let current_creation = memory::creation_on(tx, capture.owner, capture.bot, g.home)?;
    let current_binding = memory::binding_on(
        tx,
        capture.owner,
        capture.bot,
        g.home,
        current_creation.as_ref(),
    )?;
    if matches!(capture.memory, BotCapturedMemory::Absent {}) != current_binding.is_none() {
        return Err(BotError::Conflict);
    }
    let binding = bound_memory_on(tx, capture.owner, capture.bot, g.home)?;
    let submission =
        admission::read_bot_submission_on(tx, capture.owner.user_scope, capture.submission)?
            .ok_or(BotError::Storage)?;
    refuse_memory_pins(
        binding.as_ref(),
        &submission.intent.submission.request.context,
    )?;
    validate_links(tx, capture, g.home)?;
    let (state, workspace, document, version, request, nodes, omitted, invalid) =
        match &capture.memory {
            BotCapturedMemory::Absent {} => ("absent", None, None, None, None, &[][..], 0, 0),
            BotCapturedMemory::Captured {
                page,
                version,
                creation_request,
                nodes,
                omitted_count,
                invalid_count,
            } => (
                "captured",
                Some(page.workspace.to_storage()),
                Some(page.document.to_storage()),
                Some(version.to_storage()),
                Some(creation_request.to_storage()),
                nodes.as_slice(),
                *omitted_count,
                *invalid_count,
            ),
        };
    tx.execute("INSERT INTO bot_library_captures(user_scope_id,submission_request_id,bot_id,capture_version,config_revision,memory_state,memory_workspace_id,memory_document_id,memory_history_version_id,memory_binding_request_id,index_digest,indexed_count,omitted_count,invalid_count,created_at_ms,knowledge_count) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",params![capture.owner.user_scope.to_storage(),capture.submission.to_storage(),capture.bot.to_storage(),capture.version,capture.config_revision,state,workspace,document,version,request,capture.index_digest.as_slice(),nodes.len(),omitted,invalid,now,capture.knowledge.len()]).map_err(bad)?;
    for (ordinal, pin) in capture.knowledge.iter().enumerate() {
        tx.execute("INSERT INTO bot_knowledge_captures(user_scope_id,submission_request_id,ordinal,workspace_id,document_id,history_version_id) VALUES(?1,?2,?3,?4,?5,?6)",params![capture.owner.user_scope.to_storage(),capture.submission.to_storage(),ordinal,pin.page.workspace.to_storage(),pin.page.document.to_storage(),pin.version.to_storage()]).map_err(bad)?;
    }
    for (ordinal, node) in nodes.iter().enumerate() {
        tx.execute("INSERT INTO bot_memory_index_nodes(user_scope_id,submission_request_id,ordinal,node_id) VALUES(?1,?2,?3,?4)",params![capture.owner.user_scope.to_storage(),capture.submission.to_storage(),ordinal,node.to_storage()]).map_err(bad)?;
    }
    Ok(capture.clone())
}

impl Repository {
    pub fn bot_library_capture(
        &self,
        owner: BotOwner,
        bot: BotId,
        submission: BotRequestId,
    ) -> Result<Option<BotLibraryCapture>, BotError> {
        let g = visible(&self.connection, owner, bot)?;
        capture_on(&self.connection, owner, bot, submission, g.home)
    }
}
