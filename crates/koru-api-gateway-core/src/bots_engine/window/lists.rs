// SPDX-License-Identifier: MPL-2.0
//! Existing list cursors over bounded engine reads. Chat metadata is a view;
//! it is never copied into the ordinary AI database.
use super::*;
use koru_ai_core::{Chat, ProfileId, Timestamp};

#[derive(serde::Deserialize)]
struct Page<C> {
    items: Vec<person::Thread>,
    next: Option<C>,
}

fn cursor(thread: &person::Thread) -> BotChatCursor {
    BotChatCursor {
        main: thread.kind == "main",
        updated_at_ms: thread.updated_at_ms,
        chat_id: ChatId::from_uuid(thread.id),
    }
}

impl Gateway {
    pub(super) fn conversation_view(
        bot: &Bot,
        thread: &person::Thread,
        profile: ProfileId,
    ) -> BotConversation {
        let label = BotChatLabel {
            bot_id: bot.id,
            bot_name: bot.config.name.clone(),
            purpose: if thread.kind == "main" {
                BotChatPurpose::Main
            } else {
                BotChatPurpose::Additional
            },
        };
        BotConversation {
            label: label.clone(),
            chat: Chat {
                id: ChatId::from_uuid(thread.id),
                title: if thread.title.is_empty() {
                    koru_ai_core::NEW_CHAT_TITLE.into()
                } else {
                    thread.title.clone()
                },
                private: true,
                current_model: bot.config.model.clone(),
                branched_from: if thread.kind == "branch" {
                    thread
                        .origin_thread
                        .zip(thread.origin_message)
                        .map(|(chat, message)| koru_ai_core::BranchOrigin {
                            chat_id: ChatId::from_uuid(chat),
                            through_message_id: koru_ai_core::MessageId::from_uuid(message),
                        })
                } else {
                    None
                },
                agent_working_folder: Some(bot.config.working_folder.clone()),
                agent_mode: bot.config.mode.clone(),
                agent_fast: bot.config.fast,
                pinned_at: None,
                profile_id: Some(profile),
                bot: Some(label),
                created_at: Timestamp::from_millis(thread.created_at_ms),
                updated_at: Timestamp::from_millis(thread.updated_at_ms),
            },
        }
    }

    pub(super) fn window_lists(
        &self,
        owner: BotOwner,
        command: BotCommand,
    ) -> Result<BotResponse, Error> {
        match command {
            BotCommand::ListBotChats {
                bot_id,
                cursor: after,
                limit,
            } => {
                let bot = self.bot_view(
                    owner,
                    self.person_read(
                        owner,
                        Query::Bot {
                            bot: bot_id.as_uuid(),
                        },
                    )?,
                )?;
                let profile = self.window_profile(owner, bot.home_workspace)?;
                let page: Page<person::ConversationCursor> = self.person_read(
                    owner,
                    Query::Conversations {
                        bot: bot_id.as_uuid(),
                        after: after.map(|c| person::ConversationCursor {
                            main: c.main,
                            updated_at_ms: c.updated_at_ms,
                            thread: c.chat_id.as_uuid(),
                        }),
                        limit,
                    },
                )?;
                let mut chats = vec![];
                let mut bytes = 0;
                let mut last = None;
                let mut more = page.next.is_some();
                for thread in page.items {
                    let item = Self::conversation_view(&bot, &thread, profile);
                    let size = serde_json::to_vec(&item)
                        .map_err(|_| Error::EngineUnavailable)?
                        .len();
                    if bytes + size > person::MAX_COMMAND_BYTES {
                        more = true;
                        break;
                    }
                    bytes += size;
                    last = Some(cursor(&thread));
                    chats.push(item);
                }
                if more && last.is_none() {
                    return Err(Error::InputTooSmall);
                }
                Ok(BotResponse::Chats {
                    page: BotChatPage {
                        chats,
                        next_cursor: if more { last } else { None },
                    },
                })
            }
            BotCommand::ListTrashedBotChats {
                workspace,
                cursor: after,
                limit,
            } => {
                let profile = self.window_profile(owner, workspace)?;
                let page: Page<Uuid> = self.person_read(
                    owner,
                    Query::TrashedConversations {
                        workspace: workspace.as_uuid(),
                        after: after.map(|cursor| cursor.chat_id.as_uuid()),
                        limit,
                    },
                )?;
                let mut bots = std::collections::BTreeMap::new();
                let mut chats = vec![];
                let mut bytes = 0;
                let mut more = page.next.is_some();
                for thread in page.items {
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        bots.entry(thread.bot)
                    {
                        entry.insert(self.bot_view(
                            owner,
                            self.person_read(owner, Query::Bot { bot: thread.bot })?,
                        )?);
                    }
                    let bot = &bots[&thread.bot];
                    let (request_id, kind, at) = if let Some(holder) = thread
                        .held_by
                        .as_ref()
                        .filter(|holder| holder.lifecycle == person::Lifecycle::Trashed)
                    {
                        (
                            BotRequestId::from_uuid(holder.request),
                            BotLifecycleKind::TrashChat,
                            holder.completed_at_ms,
                        )
                    } else {
                        let holder = bot.held_by.as_ref().ok_or(Error::StorageUnavailable)?;
                        (holder.request_id, holder.kind, holder.completed_at_ms)
                    };
                    let item = BotTrashedChat {
                        conversation: Self::conversation_view(bot, &thread, profile),
                        trashed_by: BotTrashedBy { request_id, kind },
                        trashed_at_ms: at,
                        restore: if bot.lifecycle == BotLifecycle::Active {
                            BotChatRestore::Conversation
                        } else {
                            BotChatRestore::Bot
                        },
                        bot_held_by: bot.held_by.clone(),
                    };
                    let size = serde_json::to_vec(&item)
                        .map_err(|_| Error::EngineUnavailable)?
                        .len();
                    if bytes + size > person::MAX_COMMAND_BYTES {
                        more = true;
                        break;
                    }
                    bytes += size;
                    chats.push(item);
                }
                let next_cursor = if more {
                    chats.last().map(|chat| BotTrashedChatCursor {
                        chat_id: chat.conversation.chat.id,
                    })
                } else {
                    None
                };
                if more && next_cursor.is_none() {
                    return Err(Error::InputTooSmall);
                }
                Ok(BotResponse::TrashedChats {
                    page: BotTrashedChatPage { chats, next_cursor },
                })
            }
            BotCommand::ListActivity { workspace } => {
                self.window_workspace(owner, workspace)?;
                #[derive(serde::Deserialize)]
                struct Item {
                    bot: Uuid,
                    thread: Uuid,
                    needs_you: bool,
                }
                #[derive(serde::Deserialize)]
                struct Activity {
                    items: Vec<Item>,
                    truncated: bool,
                }
                let activity: Activity = self.person_read(
                    owner,
                    Query::Activity {
                        workspace: workspace.as_uuid(),
                    },
                )?;
                Ok(BotResponse::Activity {
                    activity: BotActivityPage {
                        truncated: activity.truncated,
                        entries: activity
                            .items
                            .into_iter()
                            .map(|item| BotActivity {
                                bot_id: BotId::from_uuid(item.bot),
                                chat_id: ChatId::from_uuid(item.thread),
                                state: if item.needs_you {
                                    BotActivityState::NeedsYou
                                } else {
                                    BotActivityState::Running
                                },
                                waiting: None,
                            })
                            .collect(),
                    },
                })
            }
            _ => Err(Error::EngineUnavailable),
        }
    }
}
