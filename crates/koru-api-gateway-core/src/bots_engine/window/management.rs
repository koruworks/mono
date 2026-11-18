// SPDX-License-Identifier: MPL-2.0
//! Settings and pause controls reuse the same authority and receipt path as
//! engine person commands. No legacy database or derived mutation cache.
use super::*;

impl Gateway {
    pub(super) fn update_action(
        &self,
        owner: BotOwner,
        bot_id: BotId,
        expected_revision: u64,
        config: BotConfig,
    ) -> Result<Change, Error> {
        let current: person::Bot = self.person_read(
            owner,
            Query::Bot {
                bot: bot_id.as_uuid(),
            },
        )?;
        if config.role.workspace != current.definition.access.workspace {
            return Err(Error::InvalidRequest);
        }
        let managed = koru_bots_engine::folders::managed_path(&self.root, bot_id.as_uuid());
        let folder = if std::path::Path::new(&config.working_folder) == managed {
            None
        } else {
            Some(config.working_folder.clone())
        };
        let creation = BotCreationConfig {
            name: config.name,
            avatar: config.avatar,
            model: config.model,
            mode: config.mode,
            fast: config.fast,
        };
        let configuration = configured(&creation, folder)?;
        Ok(Change::Configure {
            bot: bot_id.as_uuid(),
            revision: expected_revision,
            name: creation.name,
            avatar: creation.avatar,
            configuration,
            role: Some(config.role.document.as_uuid()),
        })
    }
    fn control_response(
        bot_id: BotId,
        request_id: BotRequestId,
        kind: person::ControlKind,
    ) -> BotResponse {
        BotResponse::Operation {
            operation: BotOperation {
                bot_id,
                request_id,
                run_id: None,
                status: BotOperationStatus::Accepted,
                kind: match kind {
                    person::ControlKind::Pause => BotOperationKind::Pause,
                    person::ControlKind::Resume => BotOperationKind::Resume,
                    person::ControlKind::Access => BotOperationKind::UpdateAccess,
                },
            },
        }
    }
    pub(super) fn window_change<T: serde::de::DeserializeOwned>(
        &self,
        owner: BotOwner,
        id: BotRequestId,
        action: Change,
    ) -> Result<T, Error> {
        let reply = self.execute_owner(
            owner,
            Command::Change {
                id: id.as_uuid(),
                action,
            },
        )?;
        let receipt: koru_bots_engine::messages::Receipt<T> =
            serde_json::from_value(reply).map_err(|_| Error::EngineUnavailable)?;
        Ok(receipt.answer)
    }

    fn library_view(bot: person::Bot) -> Result<BotLibraryView, Error> {
        let home = bot.definition.access.workspace;
        let page = |id| BotPage {
            workspace: home,
            document: DocumentId::from_uuid(id),
        };
        Ok(BotLibraryView {
            bot_id: BotId::from_uuid(bot.id),
            revision: bot.revision,
            role: page(bot.definition.role.ok_or(Error::InvalidRequest)?),
            knowledge: bot.definition.knowledge.into_iter().map(page).collect(),
            memory: bot.definition.memory.map(page),
        })
    }

    fn team_view(owner: BotOwner, team: person::Team) -> BotResponse {
        BotResponse::TeamPause {
            team: TeamPauseView {
                owner_device: owner.device,
                paused: team.paused,
                revision: team.revision,
                stage: TeamPauseStage::Acknowledged,
                pending_since_ms: None,
            },
        }
    }

    pub(super) fn window_management(
        &self,
        owner: BotOwner,
        command: BotCommand,
    ) -> Result<BotResponse, Error> {
        match command {
            BotCommand::ReadOperation { request_id, bot_id }
            | BotCommand::ReconcileOperation { request_id, bot_id } => {
                let receipt: person::ControlReceipt = self.person_read(
                    owner,
                    Query::ControlReceipt {
                        bot: bot_id.as_uuid(),
                        request: request_id.as_uuid(),
                    },
                )?;
                Ok(Self::control_response(bot_id, request_id, receipt.kind))
            }
            BotCommand::UpdateAccess {
                request_id,
                bot_id,
                expected_revision,
                expected_policy_revision,
                access,
                selected_pages,
                connections,
                may_ask,
            } => {
                let bot: person::Bot = self.person_read(
                    owner,
                    Query::Bot {
                        bot: bot_id.as_uuid(),
                    },
                )?;
                let mut normalized = selected_pages.clone();
                normalize_pages(&mut normalized, bot.definition.access.workspace)
                    .map_err(authority_error)?;
                let _: person::Bot = self.window_change(
                    owner,
                    request_id,
                    Change::Permissions {
                        bot: bot_id.as_uuid(),
                        revision: expected_revision,
                        policy_revision: expected_policy_revision,
                        access: match access {
                            BotAccess::Default => koru_delegate_core::BotAccess::Default,
                            BotAccess::Workspace => koru_delegate_core::BotAccess::Workspace,
                        },
                        selected_pages: selected_pages
                            .into_iter()
                            .map(|page| page.document.as_uuid())
                            .collect(),
                        connections,
                        may_ask,
                    },
                )?;
                Ok(Self::control_response(
                    bot_id,
                    request_id,
                    person::ControlKind::Access,
                ))
            }
            BotCommand::CreateChat { request_id, bot_id } => {
                let thread: person::Thread = self.window_change(
                    owner,
                    request_id,
                    Change::Conversation {
                        bot: bot_id.as_uuid(),
                        title: String::new(),
                    },
                )?;
                Ok(BotResponse::Chat {
                    reference: BotChatReference {
                        bot_id,
                        chat_id: ChatId::from_uuid(thread.id),
                    },
                })
            }
            BotCommand::Update {
                request_id,
                bot_id,
                expected_revision,
                config,
            } => {
                let action = self.update_action(owner, bot_id, expected_revision, config)?;
                let bot = self.window_change(owner, request_id, action)?;
                Ok(BotResponse::Bot {
                    bot: self.bot_view(owner, bot)?,
                })
            }
            BotCommand::ReadLibrary { bot_id } => {
                let bot = self.person_read(
                    owner,
                    Query::Bot {
                        bot: bot_id.as_uuid(),
                    },
                )?;
                Ok(BotResponse::Library {
                    library: Self::library_view(bot)?,
                })
            }
            BotCommand::SetKnowledge {
                request_id,
                bot_id,
                expected_revision,
                pages,
            } => {
                let current: person::Bot = self.person_read(
                    owner,
                    Query::Bot {
                        bot: bot_id.as_uuid(),
                    },
                )?;
                validate_knowledge(&pages, current.definition.access.workspace)
                    .map_err(authority_error)?;
                let bot: person::Bot = self.window_change(
                    owner,
                    request_id,
                    Change::Knowledge {
                        bot: bot_id.as_uuid(),
                        revision: expected_revision,
                        knowledge: pages.iter().map(|page| page.document.as_uuid()).collect(),
                    },
                )?;
                Ok(BotResponse::KnowledgeSaved {
                    receipt: BotKnowledgeReceipt {
                        bot_id,
                        request_id,
                        revision: bot.revision,
                        pages: Self::library_view(bot)?.knowledge,
                    },
                })
            }
            BotCommand::Pause {
                request_id,
                bot_id,
                expected_revision,
            }
            | BotCommand::Resume {
                request_id,
                bot_id,
                expected_revision,
            } => {
                let paused = matches!(command, BotCommand::Pause { .. });
                let _: person::Bot = self.window_change(
                    owner,
                    request_id,
                    Change::Pause {
                        bot: bot_id.as_uuid(),
                        revision: expected_revision,
                        paused,
                    },
                )?;
                Ok(Self::control_response(
                    bot_id,
                    request_id,
                    if paused {
                        person::ControlKind::Pause
                    } else {
                        person::ControlKind::Resume
                    },
                ))
            }
            BotCommand::PauseTeam {
                request_id,
                expected_team_revision,
            }
            | BotCommand::ResumeTeam {
                request_id,
                expected_team_revision,
            } => {
                let paused = matches!(command, BotCommand::PauseTeam { .. });
                Ok(Self::team_view(
                    owner,
                    self.window_change(
                        owner,
                        request_id,
                        Change::Team {
                            revision: expected_team_revision,
                            paused,
                        },
                    )?,
                ))
            }
            BotCommand::ReadTeamPause {} => Ok(Self::team_view(
                owner,
                self.person_read(owner, Query::Team)?,
            )),
            // Remaining controls are composed in subsequent P1.7 increments.
            command => self.window_lifecycle(owner, command),
        }
    }
}
