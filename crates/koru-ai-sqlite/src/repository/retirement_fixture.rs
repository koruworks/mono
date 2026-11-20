// SPDX-License-Identifier: MPL-2.0

//! Synthetic retained ownership, available only to in-memory test fixtures.
use super::*;

impl Repository {
    /// Mark a fixture chat as retired without enabling the former Bot writer.
    /// Deliberately invalid old identity fields prove ordinary code never
    /// decodes the retained Bot payload just to keep its chat hidden.
    pub fn retire_chat_for_fixture(&mut self, chat: ChatId) -> Result<(), AiError> {
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        transaction
            .execute(
                "INSERT INTO bots(bot_id,user_scope_id,home_workspace_id,owner_device_id,name,
                role_workspace_id,role_document_id,working_folder,provider,model,reasoning,
                revision,policy_revision,access,created_at_ms,updated_at_ms)
             VALUES(?1,'retired','retired','retired','Retained Bot','retired','retired',
                'retired','retired','retired','retired',1,1,'default',1,1)",
                [chat.to_storage()],
            )
            .map_err(storage_failed)?;
        transaction.execute(
            "INSERT INTO bot_chats(chat_id,bot_id,purpose,created_at_ms) VALUES(?1,?1,'main',1)",
            [chat.to_storage()],
        ).map_err(storage_failed)?;
        transaction.commit().map_err(storage_failed)
    }
}
