// SPDX-License-Identifier: MPL-2.0
//! Bounded current policy identity; never reads execution configuration/content.
use super::*;

impl Repository {
    pub fn bot_policy_metadata(
        &self,
        owner: BotOwner,
        id: BotId,
    ) -> Result<BotPolicyMetadata, BotError> {
        let row = self.connection.query_row("SELECT
            CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
            CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
            CASE WHEN typeof(home_workspace_id)='text' AND length(CAST(home_workspace_id AS BLOB))=36 THEN home_workspace_id END,
            CASE WHEN typeof(revision)='integer' AND revision>0 THEN revision END,
            CASE WHEN typeof(policy_revision)='integer' AND policy_revision>0 THEN policy_revision END,
            CASE WHEN typeof(paused)='integer' AND paused IN (0,1) THEN paused END,
            CASE WHEN typeof(lifecycle)='text' AND length(CAST(lifecycle AS BLOB))<=8 THEN lifecycle END,
            CASE WHEN typeof(access)='text' AND length(CAST(access AS BLOB))<=9 THEN access END,
            CASE WHEN typeof(may_ask)='integer' AND may_ask IN (0,1) THEN may_ask END
            FROM bots WHERE bot_id=?1", [id.to_storage()], |r| Ok((
                r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                r.get::<_,u64>(3)?,r.get::<_,u64>(4)?,r.get::<_,bool>(5)?,
                r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,bool>(8)?
            ))).optional().map_err(bad)?.ok_or(BotError::NotFound)?;
        let actual_owner = BotOwner {
            user_scope: UserScopeId::parse(&row.0).ok_or(BotError::Storage)?,
            device: DeviceId::parse(&row.1).ok_or(BotError::Storage)?,
        };
        if actual_owner != owner {
            return Err(BotError::Forbidden);
        }
        let home = WorkspaceId::parse(&row.2).ok_or(BotError::Storage)?;
        let main: String = self
            .connection
            .query_row(
                "SELECT
            CASE WHEN typeof(chat_id)='text' AND length(CAST(chat_id AS BLOB))=36 THEN chat_id END
            FROM bot_chats INDEXED BY bot_main_chat WHERE bot_id=?1 AND purpose='main'",
                [id.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?;
        let mut pages = self.connection.prepare_cached("SELECT
            CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,
            CASE WHEN typeof(document_id)='text' AND length(CAST(document_id AS BLOB))=36 THEN document_id END
            FROM bot_selected_pages WHERE bot_id=?1 ORDER BY workspace_id,document_id LIMIT 1025").map_err(bad)?;
        let mut rows = pages.query([id.to_storage()]).map_err(bad)?;
        let mut selected = Vec::<BotPage>::new();
        while let Some(r) = rows.next().map_err(bad)? {
            // Detect the sentinel without allocating a 1025th page/identity.
            if selected.len() == 1024 {
                return Err(BotError::Storage);
            }
            let workspace: String = r.get(0).map_err(bad)?;
            let document: String = r.get(1).map_err(bad)?;
            let page = BotPage {
                workspace: WorkspaceId::parse(&workspace).ok_or(BotError::Storage)?,
                document: DocumentId::parse(&document).ok_or(BotError::Storage)?,
            };
            if page.workspace != home || selected.last().is_some_and(|last| last >= &page) {
                return Err(BotError::Storage);
            }
            selected.push(page);
        }
        let metadata = BotPolicyMetadata {
            owner,
            id,
            home_workspace: home,
            main_chat: ChatId::parse(&main).ok_or(BotError::Storage)?,
            revision: row.3,
            policy_revision: row.4,
            paused: row.5,
            lifecycle: BotLifecycle::parse(&row.6).ok_or(BotError::Storage)?,
            access: match row.7.as_str() {
                "default" => BotAccess::Default,
                "workspace" => BotAccess::Workspace,
                _ => return Err(BotError::Storage),
            },
            selected_pages: selected,
            connections: crate::repository::connections::bot_grants_on(&self.connection, id)?,
            may_ask: row.8,
            pending_lifecycle: pending_lifecycle(&self.connection, owner, id)?,
            pending_change: pending_change(&self.connection, owner, id)?,
        };
        if !metadata.policy().valid() {
            return Err(BotError::Storage);
        }
        Ok(metadata)
    }
}

fn pending_lifecycle(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
) -> Result<Option<BotRequestId>, BotError> {
    let sql = format!("SELECT
        CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
        CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
        CASE WHEN typeof(operation_id)='text' AND length(CAST(operation_id AS BLOB))=36 THEN operation_id END
        FROM bot_lifecycle_intents i INDEXED BY bot_lifecycle_open
        WHERE bot_id=?1 AND chat_id IS NULL AND {} ORDER BY requested_at_ms DESC,operation_id DESC LIMIT 1", lifecycle::OPEN);
    let row: Option<(String, String, String)> = conn
        .query_row(&sql, [bot.to_storage()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .map_err(bad)?;
    row.map(|(scope, device, request)| {
        if UserScopeId::parse(&scope).ok_or(BotError::Storage)? != owner.user_scope
            || DeviceId::parse(&device).ok_or(BotError::Storage)? != owner.device
        {
            return Err(BotError::Forbidden);
        }
        request.parse().map_err(bad)
    })
    .transpose()
}

fn pending_change(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
) -> Result<Option<(BotRequestId, BotOperationKind)>, BotError> {
    let row: Option<(String,String,String)> = conn.query_row("SELECT
        CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
        CASE WHEN typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36 THEN request_id END,
        command_kind FROM bot_command_receipts INDEXED BY bot_command_guard
        WHERE bot_id=?1 AND command_kind IN ('pause','resume','update_access') AND stage='prepared'
        ORDER BY created_at_ms,request_id LIMIT 1", [bot.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(bad)?;
    row.map(|(scope, request, kind)| {
        if UserScopeId::parse(&scope).ok_or(BotError::Storage)? != owner.user_scope {
            return Err(BotError::Forbidden);
        }
        Ok((
            request.parse().map_err(bad)?,
            BotOperationKind::parse(&kind).ok_or(BotError::Storage)?,
        ))
    })
    .transpose()
}

/// Same whole-Bot barrier selection as boot, without allocating its members.
pub(super) fn lifecycle_pause(
    conn: &Connection,
    metadata: &BotPolicyMetadata,
) -> Result<Option<(Option<u64>, Option<BotRequestId>)>, BotError> {
    let sql = format!("SELECT
        CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
        CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
        CASE WHEN typeof(barrier_revision)='integer' AND barrier_revision>0 THEN barrier_revision END,
        CASE WHEN typeof(pause_request_id)='text' AND length(CAST(pause_request_id AS BLOB))=36 THEN pause_request_id END,
        (barrier_revision IS NULL OR (typeof(barrier_revision)='integer' AND barrier_revision>0))
        AND (pause_request_id IS NULL OR (typeof(pause_request_id)='text' AND length(CAST(pause_request_id AS BLOB))=36))
        FROM bot_lifecycle_intents i INDEXED BY bot_lifecycle_open
        WHERE bot_id=?1 AND chat_id IS NULL AND policy_request_id IS NOT NULL AND {}
        ORDER BY requested_at_ms DESC LIMIT 1", lifecycle::OPEN);
    let row: Option<(String, String, Option<u64>, Option<String>, bool)> = conn
        .query_row(&sql, [metadata.id.to_storage()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()
        .map_err(bad)?;
    row.map(|(scope, device, revision, pause, valid)| {
        if !valid {
            return Err(BotError::Storage);
        }
        if UserScopeId::parse(&scope).ok_or(BotError::Storage)? != metadata.owner.user_scope
            || DeviceId::parse(&device).ok_or(BotError::Storage)? != metadata.owner.device
        {
            return Err(BotError::Forbidden);
        }
        Ok((revision, pause.map(|p| p.parse().map_err(bad)).transpose()?))
    })
    .transpose()
}

#[cfg(test)]
mod tests;
