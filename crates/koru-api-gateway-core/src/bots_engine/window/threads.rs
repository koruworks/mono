// SPDX-License-Identifier: MPL-2.0
//! Typed shared-chat projection over the same person commands and receipts.
use super::*;
use koru_ai_wire::bot_threads as wire;
use koru_bots_engine::messages::{self, Receipt};
use readiness::Refusal;

fn convert<T: serde::de::DeserializeOwned>(value: impl serde::Serialize) -> Result<T, Error> {
    serde_json::from_value(serde_json::to_value(value).map_err(|_| Error::EngineUnavailable)?)
        .map_err(|_| Error::EngineUnavailable)
}

impl Gateway {
    pub(in crate::bots_engine) fn thread_command(
        &self,
        principal: &GatewayPrincipal,
        command: wire::Command,
        validate: impl Fn(&Configuration) -> Result<(), koru_ai_wire::Failure>,
    ) -> Result<wire::Response, Refusal> {
        if !permitted(principal) {
            return Err(Error::NotAllowed.into());
        }
        let owner = self.authority.owner().map_err(authority_error)?;
        let read = |query| self.execute_owner(owner, Command::Read { query });
        let execute = |command| self.validated_execute(principal, command, &validate);
        Ok(match command {
            wire::Command::Cards { thread, after } => {
                let page = read(Query::Cards {
                    thread: thread.as_uuid(),
                    after,
                })?;
                wire::Response::Cards {
                    items: convert(page["items"].clone())?,
                    next: convert(page["next"].clone())?,
                }
            }
            wire::Command::Join { id, thread } => {
                let result: Receipt<Value> = convert(execute(Command::Change {
                    id,
                    action: Change::Join {
                        thread: thread.as_uuid(),
                    },
                })?)?;
                wire::Response::Changed {
                    request: id,
                    replayed: result.replayed,
                }
            }
            wire::Command::Rename {
                id,
                thread,
                revision,
                title,
            } => {
                let result: Receipt<Value> = convert(execute(Command::Change {
                    id,
                    action: Change::RenameThread {
                        thread: thread.as_uuid(),
                        revision,
                        title,
                    },
                })?)?;
                wire::Response::Changed {
                    request: id,
                    replayed: result.replayed,
                }
            }
            wire::Command::Branch {
                id,
                thread,
                branch,
                through,
                title,
            } => {
                let result: Receipt<Value> = convert(execute(Command::Change {
                    id,
                    action: Change::Branch {
                        thread: thread.as_uuid(),
                        branch: branch.as_uuid(),
                        through,
                        title,
                    },
                })?)?;
                wire::Response::Branched {
                    request: id,
                    thread: branch,
                    replayed: result.replayed,
                }
            }
            wire::Command::Attention { thread, after } => {
                let page: koru_bots_engine::operations::AttentionPage =
                    convert(read(Query::OperationAttention {
                        thread: thread.as_uuid(),
                        after: convert(after)?,
                    })?)?;
                wire::Response::Attention {
                    items: convert(page.items)?,
                    next: convert(page.next)?,
                }
            }
            wire::Command::Continue {
                id,
                thread,
                operations,
            } => {
                let result: Receipt<koru_bots_engine::operations::Continued> =
                    convert(execute(Command::Continue {
                        id,
                        thread: thread.as_uuid(),
                        operations: operations
                            .into_iter()
                            .map(|key| koru_bots_engine::operations::Key {
                                scope: owner.user_scope.to_storage(),
                                bot: key.bot,
                                thread: thread.as_uuid(),
                                operation: key.operation,
                            })
                            .collect(),
                    })?)?;
                wire::Response::Continued {
                    request: id,
                    messages: result.answer.messages,
                    replayed: result.replayed,
                }
            }
            wire::Command::Read { thread } => {
                let view: person::ThreadView = convert(read(Query::Thread {
                    thread: thread.as_uuid(),
                })?)?;
                let bot = self.bot_view(
                    owner,
                    convert(read(Query::Bot {
                        bot: view.thread.bot,
                    })?)?,
                )?;
                let profile = self.window_profile(owner, bot.home_workspace)?;
                let team: person::Team = convert(read(Query::Team)?)?;
                wire::Response::Read {
                    snapshot: wire::Snapshot {
                        conversation: Self::conversation_view(&bot, &view.thread, profile)?,
                        revision: view.thread.revision,
                        settings_revision: view.thread.settings.revision,
                        paused: bot.paused,
                        team_paused: team.paused,
                        waiting_messages: view.waiting_messages,
                        head_sequence: view.head_sequence,
                        turns: convert(view.turns)?,
                        joined: view.joined,
                        participants: convert(view.participants)?,
                        origin: convert(view.origin)?,
                    },
                }
            }
            wire::Command::History { thread, before } => {
                let messages: Vec<wire::Message> = convert(read(Query::RecentHistory {
                    thread: thread.as_uuid(),
                    before,
                })?)?;
                let before = messages.first().map(|m| m.sequence).filter(|s| *s > 1);
                let after = messages.last().map_or(0, |m| m.sequence);
                wire::Response::History {
                    messages,
                    before,
                    after,
                }
            }
            wire::Command::Since { thread, after } => {
                let messages: Vec<wire::Message> = convert(read(Query::History {
                    thread: thread.as_uuid(),
                    after,
                })?)?;
                let after = messages.last().map_or(after, |m| m.sequence);
                wire::Response::History {
                    messages,
                    before: None,
                    after,
                }
            }
            wire::Command::Send {
                id,
                bot,
                thread,
                body,
                in_reply_to,
                attachments,
                references,
            } => {
                let result: Receipt<messages::Accepted> = convert(execute(Command::Send {
                    envelope: messages::Envelope {
                        id,
                        to: bot.as_uuid(),
                        thread: Some(thread.as_uuid()),
                        body,
                        in_reply_to,
                        attachments,
                        references,
                        deadline_ms: None,
                        reply_to: messages::ReplyTo::Thread,
                    },
                })?)?;
                wire::Response::Accepted {
                    request: id,
                    message: result.answer.message,
                    thread: result.answer.thread,
                    sequence: result.answer.sequence,
                    replayed: result.replayed,
                }
            }
            wire::Command::Stop { id, turn } => {
                let result: Receipt<messages::Stopped> = convert(execute(Command::Stop {
                    id,
                    turn: convert(turn)?,
                })?)?;
                wire::Response::Stopped {
                    request: id,
                    cancelled: result.answer.cancelled,
                    replayed: result.replayed,
                }
            }
            wire::Command::Observe {
                thread,
                boot,
                after,
            } => {
                let page: koru_bots_engine::observations::Page =
                    convert(execute(Command::Observe {
                        thread: thread.as_uuid(),
                        boot,
                        after,
                    })?)?;
                wire::Response::Observed {
                    boot: page.boot,
                    cursor: page.cursor,
                    gap: page.gap,
                    entries: convert(page.entries)?,
                }
            }
            wire::Command::Settings {
                id,
                bot,
                thread,
                revision,
                model,
                mode,
                fast,
            } => {
                let current: person::Bot = convert(read(Query::Bot { bot: bot.as_uuid() })?)?;
                let configuration = configured(
                    &BotCreationConfig {
                        name: current.definition.name,
                        avatar: current.definition.avatar,
                        model,
                        mode,
                        fast,
                    },
                    None,
                )?;
                let result: Receipt<koru_bots_engine::configuration::ConversationSettings> =
                    convert(execute(Command::Change {
                        id,
                        action: Change::ConversationSettings {
                            bot: bot.as_uuid(),
                            thread: thread.as_uuid(),
                            revision,
                            provider: configuration.provider,
                            selection: configuration.selection,
                        },
                    })?)?;
                wire::Response::Settings {
                    request: id,
                    revision: result.answer.revision,
                    model: model_view(&result.answer.configuration)?,
                    mode: result
                        .answer
                        .configuration
                        .selection
                        .mode
                        .map(|m| m.as_str().to_owned()),
                    fast: result.answer.configuration.selection.fast,
                    replayed: result.replayed,
                }
            }
            wire::Command::Questions { thread } => wire::Response::Questions {
                questions: convert(read(Query::Questions {
                    thread: thread.as_uuid(),
                })?)?,
            },
            wire::Command::Answer {
                id,
                question,
                answer,
            } => {
                let result: Receipt<koru_bots_engine::questions::Accepted> =
                    convert(execute(Command::Answer {
                        id,
                        question,
                        answer: convert(answer)?,
                    })?)?;
                wire::Response::Answered {
                    request: id,
                    question: result.answer.question,
                    continued: result.answer.continued,
                    replayed: result.replayed,
                }
            }
            wire::Command::Reask {
                id,
                thread,
                question,
            } => {
                let result: Receipt<Value> = convert(execute(Command::Change {
                    id,
                    action: Change::Reask {
                        thread: thread.as_uuid(),
                        question,
                    },
                })?)?;
                wire::Response::Changed {
                    request: id,
                    replayed: result.replayed,
                }
            }
            wire::Command::Retry { id, thread, turn } => {
                let result: Receipt<Value> = convert(execute(Command::Change {
                    id,
                    action: Change::RetrySetup {
                        thread: thread.as_uuid(),
                        turn,
                    },
                })?)?;
                wire::Response::Changed {
                    request: id,
                    replayed: result.replayed,
                }
            }
            wire::Command::SaveReply {
                id,
                thread,
                message,
                sequence,
                title,
                document,
            } => {
                let view: person::ThreadView = convert(read(Query::Thread {
                    thread: thread.as_uuid(),
                })?)?;
                let bot = self.bot_view(
                    owner,
                    convert(read(Query::Bot {
                        bot: view.thread.bot,
                    })?)?,
                )?;
                // The Bot's own workspace, where its Role and Memory live.
                let workspace = bot.home_workspace;
                let profile = self.window_profile(owner, workspace)?;
                // The engine's copy of one published Bot reply, read under
                // the owner's authority: never words the window supplies.
                let page: Vec<wire::Message> = convert(read(Query::History {
                    thread: thread.as_uuid(),
                    after: sequence.saturating_sub(1),
                })?)?;
                let reply = page
                    .into_iter()
                    .find(|m| {
                        m.id == message
                            && m.sequence == sequence
                            && m.kind == "words"
                            && !m.sender.starts_with("person:")
                    })
                    .ok_or(Error::InvalidRequest)?;
                let (workspace, title) = self.save_words(
                    owner,
                    workspace,
                    profile,
                    id,
                    document,
                    &title,
                    &format!("{}\n{message}", thread.as_uuid()),
                    &reply.content.body,
                )?;
                wire::Response::Saved {
                    request: id,
                    document,
                    workspace: workspace.as_uuid(),
                    title,
                }
            }
        })
    }
}

impl Gateway {
    /// One Bot's published words saved as a document in `workspace`, under
    /// the strict reply creation: `request` and `document` are the person's
    /// save, `source` names what was saved for the request's hash. A retry
    /// finds the page it made; another request cannot claim it.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::bots_engine) fn save_words(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        profile: koru_ai_core::ProfileId,
        request: Uuid,
        document: Uuid,
        title: &str,
        source: &str,
        words: &str,
    ) -> Result<(WorkspaceId, String), Error> {
        use koru_ai_core::bots::{BotRolePlacement, BotRoleSeed, validate_role_text};
        let title = title.trim().to_owned();
        validate_role_text(&title, words).map_err(authority_error)?;
        let seed = BotRoleSeed {
            owner,
            workspace,
            profile,
            title: title.clone(),
            candidate: DocumentId::from_uuid(document),
            request_key: request.to_string(),
            request_hash: blake3::hash(format!("{source}\n{title}").as_bytes())
                .to_hex()
                .to_string(),
            seed: koru_import_markdown::document_snapshot(
                koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
                &title,
                words,
            )
            .map_err(|_| Error::InvalidRequest)?,
        };
        let proof = self
            .authority
            .create_reply_page(&seed)
            .map_err(|error| match error {
                BotError::Conflict => Error::Conflict,
                other => authority_error(other),
            })?;
        // Another request's page under this candidate, or this request's
        // page since removed, is not a saved reply.
        if !proof.saved_reply(&seed) {
            return Err(Error::Conflict);
        }
        if proof.placement != BotRolePlacement::Live {
            return Err(Error::NotAllowed);
        }
        Ok((workspace, title))
    }
}
