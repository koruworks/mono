// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::bots::*;
pub(crate) mod admission;
pub(crate) mod policy;

mod branch;
pub(crate) mod lifecycle;
pub use branch::HistoricalBotRun;
pub use lifecycle::{
    BotHold, LifecycleCapture, LifecycleMember, LifecycleMemberDraft, LifecycleOperation,
    ParkedCursor, ParkedForOwner,
};
mod creation;
pub use creation::BotCreationScan;
mod reads;

fn bad(_: impl std::fmt::Display) -> BotError {
    BotError::Storage
}

/// The raw canonical association, which ordinary-safe Chat reads hide.
fn bot_for_chat_on(conn: &Connection, chat: ChatId) -> Result<Option<BotId>, BotError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT bot_id FROM bot_chats WHERE chat_id=?1",
            [chat.to_storage()],
            |row| row.get(0),
        )
        .optional()
        .map_err(bad)?;
    value.map(|value| value.parse().map_err(bad)).transpose()
}

/// Inside a Bot conversation write: foreign or inactive refuses, and a changed
/// configuration/policy since the caller's snapshot retries visibly.
fn fence_bot(conn: &Connection, bot: &Bot) -> Result<(), BotError> {
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let current = admission::guard(conn, owner, bot.id)?;
    if (current.revision, current.policy, current.home)
        != (bot.revision, bot.policy_revision, bot.home_workspace)
    {
        return Err(BotError::RevisionConflict);
    }
    Ok(())
}

/// Another conversation of the Bot; the unique main Chat is written only at creation.
fn insert_additional_chat(
    conn: &Connection,
    chat: ChatId,
    bot: BotId,
    now: Timestamp,
) -> Result<(), BotError> {
    conn.execute(
        "INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES(?1,?2,'additional',?3)",
        params![chat.to_storage(), bot.to_storage(), now.millis()],
    )
    .map_err(bad)?;
    Ok(())
}

impl Repository {
    /// Resolve both identities without reading the selected block's contents.
    pub fn bot_chat_for_block(
        &self,
        block: koru_ai_core::MessageBlockId,
    ) -> Result<Option<(BotId, ChatId)>, BotError> {
        let value: Option<(String, String)> = self.connection.query_row(
            "SELECT bc.bot_id,bc.chat_id FROM message_blocks b JOIN messages m ON m.id=b.message_id JOIN bot_chats bc ON bc.chat_id=m.chat_id WHERE b.id=?1",
            [block.to_storage()], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional().map_err(bad)?;
        value
            .map(|(bot, chat)| {
                Ok((
                    bot.parse().map_err(bad)?,
                    ChatId::parse(&chat).ok_or(BotError::Storage)?,
                ))
            })
            .transpose()
    }

    /// Additional conversations use the same writer and existing ai16 receipt.
    /// The caller supplies fresh authority; the transaction fences its Bot snapshot.
    pub fn create_bot_chat(
        &mut self,
        bot: &Bot,
        profile: ProfileId,
        request: BotRequestId,
        fingerprint: &str,
        now: Timestamp,
    ) -> Result<BotChatReference, BotError> {
        let tx = self.connection.transaction().map_err(bad)?;
        fence_bot(&tx, bot)?;
        if let Some(prior) = admission::raw(&tx, bot.user_scope, request)? {
            if prior.bot != bot.id.to_storage()
                || prior.fingerprint != fingerprint
                || prior.kind != "create_chat"
            {
                return Err(BotError::Conflict);
            }
            let json = prior.outcome.ok_or(BotError::Storage)?;
            if prior.stage != "completed" || json.len() > 1024 * 1024 {
                return Err(BotError::Storage);
            }
            let outcome: BotReceiptOutcome = serde_json::from_str(&json).map_err(bad)?;
            outcome.validate()?;
            let BotReceiptOutcome::ChatCreated { reference, .. } = outcome else {
                return Err(BotError::Storage);
            };
            let associated: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM bot_chats bc JOIN chats c ON c.id=bc.chat_id WHERE bc.chat_id=?1 AND bc.bot_id=?2 AND bc.purpose='additional' AND c.profile_id=?3)",
                params![reference.chat_id.to_storage(), bot.id.to_storage(), profile.to_storage()],
                |row| row.get(0)).map_err(bad)?;
            if reference.bot_id != bot.id || !associated {
                return Err(BotError::NotFound);
            }
            return Ok(reference);
        }
        validate_config(&bot.config, bot.home_workspace)?;
        // Titled as an ordinary new chat is, until its first message names
        // it: the Bot's name is on the Bot label, and two conversations of
        // one Bot named alike could not be told apart. The main
        // conversation keeps the Bot's name it was made with.
        let chat = Chat {
            id: ChatId::new(),
            title: koru_ai_core::NEW_CHAT_TITLE.to_owned(),
            private: false,
            current_model: bot.config.model.clone(),
            branched_from: None,
            agent_working_folder: Some(bot.config.working_folder.clone()),
            agent_mode: bot.config.mode.clone(),
            agent_fast: bot.config.fast,
            pinned_at: None,
            profile_id: Some(profile),
            bot: None,
            created_at: now,
            updated_at: now,
        };
        insert_chat(&tx, &chat).map_err(|_| BotError::Storage)?;
        insert_additional_chat(&tx, chat.id, bot.id, now)?;
        let reference = BotChatReference {
            bot_id: bot.id,
            chat_id: chat.id,
        };
        let outcome = BotReceiptOutcome::ChatCreated {
            version: 1,
            reference: reference.clone(),
        };
        tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'create_chat','completed',?5,?6,?6)",
            params![bot.user_scope.to_storage(), request.to_storage(),bot.id.to_storage(),fingerprint,
                serde_json::to_string(&outcome).map_err(bad)?,now.millis()]).map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(reference)
    }

    /// Resolve a selected context block's Bot association without exposing its content.
    pub fn bot_for_block(
        &self,
        block: koru_ai_core::MessageBlockId,
    ) -> Result<Option<BotId>, BotError> {
        Ok(self.bot_chat_for_block(block)?.map(|(bot, _)| bot))
    }

    pub fn bot_for_chat(&self, chat: ChatId) -> Result<Option<BotId>, BotError> {
        bot_for_chat_on(&self.connection, chat)
    }

    pub fn all_bot_chats(&self) -> Result<Vec<(BotId, ChatId)>, BotError> {
        let mut query = self
            .connection
            .prepare("SELECT bot_id,chat_id FROM bot_chats")
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

    // Fixture only: production admission must pair this fence with its typed receipt (P1.3).
    #[cfg(test)]
    fn lock_bot_folder(
        &mut self,
        bot: BotId,
        expected_revision: u64,
        request: BotRequestId,
    ) -> Result<(), BotError> {
        let changed = self
            .connection
            .execute(
                "UPDATE bots SET folder_lock_request_id=?3
          WHERE bot_id=?1 AND revision=?2 AND folder_frozen_at_ms IS NULL
          AND (folder_lock_request_id IS NULL OR folder_lock_request_id=?3)",
                params![bot.to_storage(), expected_revision, request.to_storage()],
            )
            .map_err(bad)?;
        if changed == 0 {
            return Err(BotError::FolderLocked);
        }
        Ok(())
    }

    /// Only a definite owner outcome can settle a provisional fence; unknown outcomes do nothing.
    #[cfg(test)]
    fn settle_bot_folder(
        &mut self,
        bot: BotId,
        request: BotRequestId,
        accepted_at: Option<i64>,
    ) -> Result<(), BotError> {
        self.connection
            .execute(
                "UPDATE bots SET folder_frozen_at_ms=?3,folder_lock_request_id=NULL
          WHERE bot_id=?1 AND folder_lock_request_id=?2 AND folder_frozen_at_ms IS NULL",
                params![bot.to_storage(), request.to_storage(), accepted_at],
            )
            .map_err(bad)?;
        Ok(())
    }
}

/// The designated main Chat: its live association, or after an explicit
/// purge of that conversation its retained tombstone, which never reads as a
/// Chat and is never replaced (O7).
fn designated_main(conn: &Connection, bot: BotId) -> Result<ChatId, BotError> {
    let live: Option<String> = conn
        .query_row(
            "SELECT chat_id FROM bot_chats WHERE bot_id=?1 AND purpose='main'",
            [bot.to_storage()],
            |r| r.get(0),
        )
        .optional()
        .map_err(bad)?;
    let main = match live {
        Some(main) => main,
        None => conn
            .query_row(
                "SELECT subject_id FROM bot_purge_tombstones WHERE bot_id=?1 AND subject_kind='chat' AND chat_purpose='main'",
                [bot.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?,
    };
    ChatId::parse(&main).ok_or(BotError::Storage)
}

impl Repository {
    /// A Bot in any lifecycle, with that lifecycle. Ordinary Bot commands use
    /// [`BotRepository::bot`], which refuses an inactive Bot; lifecycle reads
    /// (the Archived and Trash filters, restore) use this.
    pub fn bot_any_lifecycle(&self, id: BotId) -> Result<Option<(Bot, BotLifecycle)>, BotError> {
        let result = self
            .connection
            .query_row(
                "SELECT user_scope_id,home_workspace_id,owner_device_id,name,
          role_workspace_id,role_document_id,working_folder,provider,model,reasoning,mode,fast,
          revision,policy_revision,access,paused,folder_lock_request_id,folder_frozen_at_ms,
          created_at_ms,updated_at_ms,lifecycle FROM bots WHERE bot_id=?1",
                [id.to_storage()],
                |r| {
                    let get = |i| r.get::<_, String>(i);
                    Ok((
                        get(0)?,
                        get(1)?,
                        get(2)?,
                        get(3)?,
                        get(4)?,
                        get(5)?,
                        get(6)?,
                        get(7)?,
                        get(8)?,
                        get(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<bool>>(11)?,
                        r.get::<_, u64>(12)?,
                        r.get::<_, u64>(13)?,
                        get(14)?,
                        r.get::<_, bool>(15)?,
                        r.get::<_, Option<String>>(16)?,
                        r.get::<_, Option<i64>>(17)?,
                        r.get::<_, i64>(18)?,
                        r.get::<_, i64>(19)?,
                        get(20)?,
                    ))
                },
            )
            .optional()
            .map_err(bad)?;
        let Some(r) = result else { return Ok(None) };
        let lifecycle = BotLifecycle::parse(&r.20).ok_or(BotError::Storage)?;
        let main = designated_main(&self.connection, id)?;
        let mut query=self.connection.prepare("SELECT workspace_id,document_id FROM bot_selected_pages WHERE bot_id=?1 ORDER BY workspace_id,document_id").map_err(bad)?;
        let rows = query
            .query_map([id.to_storage()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(bad)?;
        let selected_pages = rows
            .map(|r| {
                let (w, d) = r.map_err(bad)?;
                Ok(BotPage {
                    workspace: WorkspaceId::parse(&w).ok_or(BotError::Storage)?,
                    document: DocumentId::parse(&d).ok_or(BotError::Storage)?,
                })
            })
            .collect::<Result<Vec<_>, BotError>>()?;
        Ok(Some((
            Bot {
                id,
                user_scope: UserScopeId::parse(&r.0).ok_or(BotError::Storage)?,
                home_workspace: WorkspaceId::parse(&r.1).ok_or(BotError::Storage)?,
                owner_device: DeviceId::parse(&r.2).ok_or(BotError::Storage)?,
                config: BotConfig {
                    name: r.3,
                    role: BotPage {
                        workspace: WorkspaceId::parse(&r.4).ok_or(BotError::Storage)?,
                        document: DocumentId::parse(&r.5).ok_or(BotError::Storage)?,
                    },
                    working_folder: r.6,
                    model: ModelSelection {
                        provider: AiProviderId::parse(r.7).map_err(bad)?,
                        requested_model: r.8,
                        default_reasoning: ReasoningChoice::parse(&r.9).ok_or(BotError::Storage)?,
                    },
                    mode: r.10,
                    fast: r.11,
                },
                revision: r.12,
                policy_revision: r.13,
                access: match r.14.as_str() {
                    "default" => BotAccess::Default,
                    "workspace" => BotAccess::Workspace,
                    _ => return Err(BotError::Storage),
                },
                paused: r.15,
                folder_lock_request: r.16.map(|v| v.parse().map_err(bad)).transpose()?,
                folder_frozen_at_ms: r.17,
                created_at_ms: r.18,
                updated_at_ms: r.19,
                lifecycle,
                open_operation: None,
                held_by: None,
                pending_change: None,
                main_chat: main,
                selected_pages,
            },
            lifecycle,
        )))
    }
}

impl BotRepository for Repository {
    fn bot(&self, id: BotId) -> Result<Option<Bot>, BotError> {
        match self.bot_any_lifecycle(id)? {
            None => Ok(None),
            Some((bot, BotLifecycle::Active)) => Ok(Some(bot)),
            Some(_) => Err(BotError::Forbidden),
        }
    }
    fn bots(&self, scope: UserScopeId, workspace: WorkspaceId) -> Result<Vec<Bot>, BotError> {
        let mut query=self.connection.prepare("SELECT bot_id FROM bots WHERE user_scope_id=?1 AND home_workspace_id=?2 AND lifecycle='active' ORDER BY created_at_ms,bot_id").map_err(bad)?;
        let ids = query
            .query_map(params![scope.to_storage(), workspace.to_storage()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(bad)?;
        ids.map(|id| {
            self.bot(id.map_err(bad)?.parse().map_err(bad)?)?
                .ok_or(BotError::Storage)
        })
        .collect()
    }

    fn bot_receipt(
        &self,
        scope: UserScopeId,
        request: BotRequestId,
        fingerprint: &str,
    ) -> Result<Option<Bot>, BotError> {
        let row:Option<(String,String)>=self.connection.query_row("SELECT fingerprint,outcome_json FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
            params![scope.to_storage(),request.to_storage()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(bad)?;
        match row {
            None => Ok(None),
            Some((f, b)) if f == fingerprint => {
                let outcome: BotReceiptOutcome = serde_json::from_str(&b).map_err(bad)?;
                outcome.validate()?;
                match outcome {
                    BotReceiptOutcome::Configuration { bot, .. } => Ok(Some(bot)),
                    _ => Err(BotError::Unavailable),
                }
            }
            Some(_) => Err(BotError::Conflict),
        }
    }

    fn save_bot(
        &mut self,
        bot: &Bot,
        profile: ProfileId,
        request: BotRequestId,
        fingerprint: &str,
        expected: Option<u64>,
    ) -> Result<Bot, BotError> {
        self.save_bot_record(bot, profile, request, fingerprint, expected, None)
    }
}

impl Repository {
    fn save_bot_record(
        &mut self,
        bot: &Bot,
        profile: ProfileId,
        request: BotRequestId,
        fingerprint: &str,
        expected: Option<u64>,
        barrier: Option<&koru_delegate_core::BotPolicyBarrier>,
    ) -> Result<Bot, BotError> {
        if let Some(existing) = self.bot_receipt(bot.user_scope, request, fingerprint)? {
            return Ok(existing);
        }
        validate_config(&bot.config, bot.home_workspace)?;
        policy::require_policy_settled(&self.connection, bot.id)?;
        admission::require_pause_settled(&self.connection, bot.user_scope, bot.id)?;
        if let Some(barrier) = barrier {
            validate_access_policy(bot, barrier)?;
        }
        let kind = match expected {
            None => "create",
            Some(_) => {
                let previous = self.bot(bot.id)?.ok_or(BotError::NotFound)?;
                if previous.policy_revision != bot.policy_revision {
                    "update_access"
                } else {
                    "update"
                }
            }
        };
        let tx = self.connection.transaction().map_err(bad)?;
        if let Some(revision) = expected {
            let current:Option<(u64,String,Option<String>,Option<i64>)>=tx.query_row("SELECT revision,working_folder,folder_lock_request_id,folder_frozen_at_ms FROM bots WHERE bot_id=?1 AND user_scope_id=?2 AND owner_device_id=?3",
                params![bot.id.to_storage(),bot.user_scope.to_storage(),bot.owner_device.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(bad)?;
            let Some((actual, folder, lock, frozen)) = current else {
                return Err(BotError::NotFound);
            };
            if actual != revision {
                return Err(BotError::RevisionConflict);
            }
            // An unfinished Archive or Trash holds the Bot still: its
            // completion relies on nobody else moving the revision (P1.7).
            if lifecycle::whole_bot_operation_open(&tx, bot.id)? {
                return Err(BotError::Conflict);
            }
            if folder != bot.config.working_folder && (lock.is_some() || frozen.is_some()) {
                return Err(BotError::FolderLocked);
            }
        } else {
            // The one BotId reservation, shared with creation (P1.8): a live
            // Bot, a purged one's tombstone or another unfinished creation.
            creation::reserve_bot_id(&tx, bot.user_scope, bot.id)?;
            tx.execute("INSERT INTO chats(id,title,current_provider,current_model,current_reasoning,created_at_ms,updated_at_ms,profile_id,agent_working_folder,agent_mode,agent_fast)
              VALUES(?1,?2,?3,?4,?5,?6,?6,?7,?8,?9,?10)",params![bot.main_chat.to_storage(),bot.config.name,bot.config.model.provider.as_str(),bot.config.model.requested_model,bot.config.model.default_reasoning.as_token(),bot.created_at_ms,profile.to_storage(),bot.config.working_folder,bot.config.mode,bot.config.fast]).map_err(bad)?;
        }
        tx.execute("INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,role_workspace_id,role_document_id,working_folder,provider,model,reasoning,mode,fast,revision,policy_revision,access,created_at_ms,updated_at_ms)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
          ON CONFLICT(bot_id) DO UPDATE SET
          role_owned=CASE WHEN bots.role_workspace_id=excluded.role_workspace_id AND bots.role_document_id=excluded.role_document_id THEN bots.role_owned ELSE 0 END,
          role_creation_request_id=CASE WHEN bots.role_workspace_id=excluded.role_workspace_id AND bots.role_document_id=excluded.role_document_id THEN bots.role_creation_request_id ELSE NULL END,
          name=excluded.name,role_workspace_id=excluded.role_workspace_id,role_document_id=excluded.role_document_id,working_folder=excluded.working_folder,provider=excluded.provider,model=excluded.model,reasoning=excluded.reasoning,mode=excluded.mode,fast=excluded.fast,revision=excluded.revision,policy_revision=excluded.policy_revision,access=excluded.access,updated_at_ms=excluded.updated_at_ms",
          params![bot.id.to_storage(),bot.user_scope.to_storage(),bot.home_workspace.to_storage(),bot.owner_device.to_storage(),bot.config.name,bot.config.role.workspace.to_storage(),bot.config.role.document.to_storage(),bot.config.working_folder,bot.config.model.provider.as_str(),bot.config.model.requested_model,bot.config.model.default_reasoning.as_token(),bot.config.mode,bot.config.fast,bot.revision,bot.policy_revision,match bot.access{BotAccess::Default=>"default",BotAccess::Workspace=>"workspace"},bot.created_at_ms,bot.updated_at_ms]).map_err(bad)?;
        if expected.is_none() {
            tx.execute(
                "INSERT INTO bot_chats VALUES(?1,?2,'main',?3)",
                params![
                    bot.main_chat.to_storage(),
                    bot.id.to_storage(),
                    bot.created_at_ms
                ],
            )
            .map_err(bad)?;
        }
        // A Bot's defaults seed new conversations only (O23): creation wrote
        // the main Chat above from them, and an edit writes no conversation.
        // The main conversation keeps its own title, model, reasoning, mode
        // and Fast setting like every other; a model change is made in the
        // conversation it is for. A task runs in the Bot's folder, never the
        // one a Chat row recorded when it was made.
        tx.execute(
            "DELETE FROM bot_selected_pages WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .map_err(bad)?;
        for page in &bot.selected_pages {
            tx.execute(
                "INSERT INTO bot_selected_pages VALUES(?1,?2,?3,?4)",
                params![
                    bot.id.to_storage(),
                    page.workspace.to_storage(),
                    page.document.to_storage(),
                    bot.updated_at_ms
                ],
            )
            .map_err(bad)?;
        }
        let (stage, outcome) = match barrier {
            Some(barrier) if kind == "update_access" => (
                "prepared",
                BotReceiptOutcome::AccessPolicy {
                    version: 1,
                    bot: bot.clone(),
                    barrier: barrier.clone(),
                },
            ),
            Some(_) => return Err(BotError::Invalid),
            None => (
                "completed",
                BotReceiptOutcome::Configuration {
                    version: 1,
                    bot: bot.clone(),
                },
            ),
        };
        tx.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?7,?8,?5,?6,?6)",params![bot.user_scope.to_storage(),request.to_storage(),bot.id.to_storage(),fingerprint,serde_json::to_string(&outcome).map_err(bad)?,bot.updated_at_ms,kind,stage]).map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(bot.clone())
    }
}

#[cfg(test)]
mod tests;
