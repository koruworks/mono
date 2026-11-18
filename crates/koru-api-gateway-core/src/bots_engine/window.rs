// SPDX-License-Identifier: MPL-2.0
//! Existing Bot-window contracts, backed by the engine when it owns the root.
//! No legacy Bot database is consulted and no request falls back to it.
use super::*;
use koru_ai_core::{ChatId, DocumentId, ModelSelection, ReasoningChoice, WorkspaceId, bots::*};
use koru_bots_engine::{
    configuration::Configuration,
    creation::{self as c, Phase, Record, Step},
};
use uuid::Uuid;
mod lifecycle;
mod lists;
mod management;

fn configured(config: &BotCreationConfig, folder: Option<String>) -> Result<Configuration, Error> {
    config.validate().map_err(authority_error)?;
    let provider = koru_ai_provider_agent::agent_kind_of(&config.model.provider)
        .ok_or(Error::InvalidRequest)?
        .kind;
    let model = if config.model.requested_model == koru_ai_core::AGENT_DEFAULT_MODEL {
        None
    } else {
        Some(
            config
                .model
                .requested_model
                .parse()
                .map_err(|_| Error::InvalidRequest)?,
        )
    };
    let reasoning = if config.model.default_reasoning == ReasoningChoice::Default {
        None
    } else {
        Some(
            serde_json::from_value(Value::String(
                config.model.default_reasoning.as_token().into(),
            ))
            .map_err(|_| Error::InvalidRequest)?,
        )
    };
    Ok(Configuration {
        provider,
        working_folder: folder,
        selection: koru_session_protocol::LaunchSelection {
            model,
            reasoning,
            mode: config
                .mode
                .as_ref()
                .map(|mode| mode.parse().map_err(|_| Error::InvalidRequest))
                .transpose()?,
            fast: config.fast,
        },
    })
}

impl Gateway {
    fn window_workspace(&self, owner: BotOwner, workspace: WorkspaceId) -> Result<(), Error> {
        self.window_profile(owner, workspace).map(|_| ())
    }
    fn window_profile(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
    ) -> Result<koru_ai_core::ProfileId, Error> {
        let snapshot = self
            .authority
            .inspect(&[workspace], &[])
            .map_err(authority_error)?;
        if snapshot.owner != owner {
            return Err(Error::NotAllowed);
        }
        snapshot.workspace(workspace).map_err(authority_error)
    }
    fn person_read<T: serde::de::DeserializeOwned>(
        &self,
        owner: BotOwner,
        query: Query,
    ) -> Result<T, Error> {
        let scope = owner.user_scope.to_storage();
        let reply = (self.call)(person::Request {
            owner: scope.clone(),
            sender: format!("person:{scope}"),
            command: Command::Read { query },
        })?;
        serde_json::from_value(reply.0).map_err(|_| Error::EngineUnavailable)
    }
    fn bot_view(&self, owner: BotOwner, bot: person::Bot) -> Result<Bot, Error> {
        let d = bot.definition;
        let provider = koru_ai_provider_agent::agent_provider_kinds()
            .into_iter()
            .find(|kind| kind.kind == d.configuration.provider)
            .ok_or(Error::InvalidRequest)?
            .id;
        let selection = d.configuration.selection;
        let working_folder = d.configuration.working_folder.unwrap_or_else(|| {
            koru_bots_engine::folders::managed_path(&self.root, bot.id)
                .to_string_lossy()
                .into_owned()
        });
        Ok(Bot {
            id: BotId::from_uuid(bot.id),
            home_workspace: d.access.workspace,
            user_scope: owner.user_scope,
            owner_device: owner.device,
            config: BotConfig {
                name: d.name,
                avatar: d.avatar,
                role: BotPage {
                    workspace: d.access.workspace,
                    document: DocumentId::from_uuid(d.role.ok_or(Error::InvalidRequest)?),
                },
                working_folder,
                model: ModelSelection {
                    provider,
                    requested_model: selection
                        .model
                        .map(|model| model.as_str().to_owned())
                        .unwrap_or_else(|| koru_ai_core::AGENT_DEFAULT_MODEL.into()),
                    default_reasoning: selection
                        .reasoning
                        .map(|reasoning| {
                            ReasoningChoice::parse(reasoning.as_str()).ok_or(Error::InvalidRequest)
                        })
                        .transpose()?
                        .unwrap_or_default(),
                },
                mode: selection.mode.map(|mode| mode.as_str().to_owned()),
                fast: selection.fast,
            },
            access: match d.access.access {
                koru_delegate_core::BotAccess::Default => BotAccess::Default,
                koru_delegate_core::BotAccess::Workspace => BotAccess::Workspace,
            },
            selected_pages: d
                .access
                .selected_pages
                .into_iter()
                .map(|document| BotPage {
                    workspace: d.access.workspace,
                    document,
                })
                .collect(),
            revision: bot.revision,
            policy_revision: bot.policy_revision,
            main_chat: ChatId::from_uuid(bot.main_thread),
            paused: bot.paused,
            folder_lock_request: None,
            folder_frozen_at_ms: bot.folder_frozen_at_ms,
            created_at_ms: bot.created_at_ms,
            updated_at_ms: bot.updated_at_ms,
            lifecycle: match bot.lifecycle {
                person::Lifecycle::Active => BotLifecycle::Active,
                person::Lifecycle::Archived => BotLifecycle::Archived,
                person::Lifecycle::Trashed => BotLifecycle::Trashed,
            },
            open_operation: None,
            held_by: bot.held_by.map(Self::holder_view).transpose()?,
            pending_change: None,
            connections: d.access.connections,
            may_ask: bot.may_ask,
        })
    }
    fn creation_view(&self, owner: BotOwner, record: Record) -> Result<BotCreation, Error> {
        let previous = record.abandoned_from.unwrap_or(record.phase);
        let authored = record.seed.is_some();
        let phase = match previous {
            Phase::Intent => BotCreationPhase::Intent,
            Phase::FolderPending => BotCreationPhase::FolderPending,
            Phase::FolderReady => BotCreationPhase::FolderReady,
            Phase::RolePending => BotCreationPhase::RolePending,
            _ if authored => BotCreationPhase::RoleVerified,
            _ => BotCreationPhase::FolderReady,
        };
        let reason = record.failure.map(|reason| match reason {
            c::Failure::FolderUnavailable => BotCreationReason::FolderUnavailable,
            c::Failure::FolderOccupied => BotCreationReason::FolderOccupied,
            c::Failure::FolderMismatch => BotCreationReason::FolderMismatch,
            c::Failure::FolderInvalid => BotCreationReason::FolderInvalid,
            c::Failure::RoleUnavailable => BotCreationReason::RoleUnavailable,
            c::Failure::RoleMissing => BotCreationReason::RoleMissing,
            c::Failure::RoleConflict => BotCreationReason::RoleConflict,
            c::Failure::StoreRefused => BotCreationReason::StoreRefused,
            c::Failure::AuthorityUnavailable => BotCreationReason::StoreUnavailable,
        });
        let stage = match record.phase {
            Phase::Completed => BotCreationStage::Completed,
            Phase::Abandoned => BotCreationStage::Abandoned,
            _ if reason.is_some() => BotCreationStage::Incomplete,
            _ => BotCreationStage::Pending,
        };
        let role_started = matches!(
            phase,
            BotCreationPhase::RolePending | BotCreationPhase::RoleVerified
        );
        let role_page = if (!authored || role_started)
            && !matches!(
                reason,
                Some(BotCreationReason::RoleMissing | BotCreationReason::StoreRefused)
            ) {
            record.definition.role.map(|id| BotPage {
                workspace: record.definition.access.workspace,
                document: DocumentId::from_uuid(id),
            })
        } else {
            None
        };
        let left_behind = if stage == BotCreationStage::Abandoned {
            let folder = if record.definition.configuration.working_folder.is_none()
                && phase != BotCreationPhase::Intent
            {
                let path = koru_bots_engine::folders::managed_path(&self.root, record.bot);
                if phase == BotCreationPhase::FolderPending
                    || self.folders.occupied(&path).unwrap_or(true)
                {
                    Some(path.to_string_lossy().into_owned())
                } else {
                    None
                }
            } else {
                None
            };
            Some(BotCreationLeftovers {
                role_page: if authored { role_page.clone() } else { None },
                folder,
            })
        } else {
            None
        };
        let bot = if record.phase == Phase::Completed {
            Some(self.bot_view(
                owner,
                self.person_read(owner, Query::Bot { bot: record.bot })?,
            )?)
        } else {
            None
        };
        Ok(BotCreation {
            request_id: BotRequestId::from_uuid(record.request),
            bot_id: BotId::from_uuid(record.bot),
            home_workspace: record.definition.access.workspace,
            name: record.definition.name,
            stage,
            phase,
            reason,
            retryable: stage == BotCreationStage::Incomplete
                && record.failure.is_some_and(c::Failure::retryable),
            role_page_unconfirmed: role_page.is_some()
                && authored
                && phase == BotCreationPhase::RolePending,
            role_page,
            folder_binding: record.folder_binding,
            left_behind,
            bot,
        })
    }
    pub(super) fn window_command(&self, command: BotCommand) -> Result<BotResponse, Error> {
        let owner = self.authority.owner().map_err(authority_error)?;
        match command {
            BotCommand::CreateBotWithRole {
                request_id,
                bot_id,
                home_workspace,
                role,
                folder,
                config,
                access,
                selected_pages,
            } => {
                let source = BotCommand::CreateBotWithRole {
                    request_id,
                    bot_id,
                    home_workspace,
                    role: role.clone(),
                    folder: folder.clone(),
                    config: config.clone(),
                    access,
                    selected_pages: selected_pages.clone(),
                };
                let source_hash =
                    blake3::hash(&serde_json::to_vec(&source).map_err(|_| Error::InvalidRequest)?)
                        .to_hex()
                        .to_string();
                let existing = match self.creation_call(
                    owner,
                    c::Command::Check {
                        request: request_id.as_uuid(),
                        source_hash: source_hash.clone(),
                    },
                )? {
                    c::Reply::Record(record) => record,
                    _ => return Err(Error::EngineUnavailable),
                };
                let record = if let Some(record) = existing {
                    record
                } else {
                    let folder = match folder {
                        BotFolderChoice::Managed {} => None,
                        BotFolderChoice::Selected { path } => Some(
                            koru_bots_engine::folders::selected(
                                self.folders.as_ref(),
                                std::path::Path::new(&path),
                                std::slice::from_ref(&self.root),
                            )
                            .map_err(|_| Error::InvalidRequest)?
                            .to_str()
                            .ok_or(Error::InvalidRequest)?
                            .to_owned(),
                        ),
                    };
                    let configuration = configured(&config, folder)?;
                    self.configuration(&configuration, config.avatar)?;
                    let mut pages = selected_pages;
                    normalize_pages(&mut pages, home_workspace).map_err(authority_error)?;
                    let (role, authored, seed) = match role {
                        BotRoleInput::Existing { page } => {
                            if page.workspace != home_workspace {
                                return Err(Error::InvalidRequest);
                            }
                            (Some(page.document.as_uuid()), None, None)
                        }
                        BotRoleInput::Authored { title, markdown } => {
                            validate_role_text(&title, &markdown).map_err(authority_error)?;
                            let seed = koru_import_markdown::document_snapshot(
                                koru_core::WorkspaceId::from_uuid(home_workspace.as_uuid()),
                                &title,
                                &markdown,
                            )
                            .map_err(|_| Error::InvalidRequest)?;
                            (
                                None,
                                Some(c::AuthoredRole {
                                    title,
                                    source_hash: blake3::hash(markdown.as_bytes())
                                        .to_hex()
                                        .to_string(),
                                }),
                                Some(seed),
                            )
                        }
                    };
                    let definition = person::Definition {
                        name: config.name,
                        avatar: config.avatar,
                        configuration,
                        role,
                        knowledge: vec![],
                        memory: None,
                        access: koru_delegate_core::CapturedBotPolicy {
                            workspace: home_workspace,
                            destination: home_workspace,
                            access: match access {
                                BotAccess::Default => koru_delegate_core::BotAccess::Default,
                                BotAccess::Workspace => koru_delegate_core::BotAccess::Workspace,
                            },
                            selected_pages: pages.into_iter().map(|page| page.document).collect(),
                            connections: Default::default(),
                        },
                    };
                    // Prove current placement before reserving the creation.
                    let mut refs: Vec<_> =
                        definition.access.selected_pages.iter().copied().collect();
                    refs.extend(definition.role.map(DocumentId::from_uuid));
                    let refs: Vec<_> = refs
                        .into_iter()
                        .map(|document| BotPage {
                            workspace: home_workspace,
                            document,
                        })
                        .collect();
                    let snapshot = self
                        .authority
                        .inspect(&[home_workspace], &refs)
                        .map_err(authority_error)?;
                    if snapshot.owner != owner {
                        return Err(Error::NotAllowed);
                    }
                    snapshot
                        .workspace(home_workspace)
                        .map_err(authority_error)?;
                    for page in refs {
                        snapshot
                            .pages
                            .get(&page)
                            .ok_or(Error::NotAllowed)?
                            .as_ref()
                            .map_err(|e| authority_error(*e))?;
                    }
                    match self.creation_call(
                        owner,
                        c::Command::Begin {
                            request: request_id.as_uuid(),
                            source_hash,
                            bot: bot_id.as_uuid(),
                            definition,
                            authored,
                            seed,
                        },
                    )? {
                        c::Reply::Record(Some(record)) => record,
                        _ => return Err(Error::EngineUnavailable),
                    }
                };
                let record = self.creation_progress(owner, record)?;
                Ok(BotResponse::Creation {
                    creation: self.creation_view(owner, record)?,
                })
            }
            BotCommand::ReadBotCreation { request_id }
            | BotCommand::RetryBotCreation { request_id }
            | BotCommand::AbandonBotCreation { request_id } => {
                let mut record = self
                    .creation_read(owner, request_id.as_uuid())?
                    .ok_or(Error::NotAllowed)?;
                for _ in 0..4 {
                    let step = match command {
                        BotCommand::AbandonBotCreation { .. }
                            if record.phase == Phase::Completed =>
                        {
                            return Err(Error::Conflict);
                        }
                        BotCommand::RetryBotCreation { .. }
                            if record.failure.is_some_and(c::Failure::retryable)
                                && !matches!(record.phase, Phase::Completed | Phase::Abandoned) =>
                        {
                            Some(Step::Retry)
                        }
                        BotCommand::AbandonBotCreation { .. }
                            if record.phase != Phase::Abandoned =>
                        {
                            Some(Step::Abandon)
                        }
                        _ => None,
                    };
                    let Some(step) = step else {
                        break;
                    };
                    match self.creation_advance(owner, &record, step) {
                        Ok(next) => {
                            record = next;
                            break;
                        }
                        Err(Error::Conflict) => {
                            record = self
                                .creation_read(owner, request_id.as_uuid())?
                                .ok_or(Error::NotAllowed)?
                        }
                        Err(error) => return Err(error),
                    }
                }
                if matches!(command, BotCommand::AbandonBotCreation { .. })
                    && record.phase != Phase::Abandoned
                {
                    return Err(Error::Conflict);
                }
                record = self.creation_progress(owner, record)?;
                Ok(BotResponse::Creation {
                    creation: self.creation_view(owner, record)?,
                })
            }
            BotCommand::Read { bot_id } => Ok(BotResponse::Bot {
                bot: self.bot_view(
                    owner,
                    self.person_read(
                        owner,
                        Query::Bot {
                            bot: bot_id.as_uuid(),
                        },
                    )?,
                )?,
            }),
            BotCommand::List { workspace } | BotCommand::ListLifecycle { workspace, .. } => {
                self.window_workspace(owner, workspace)?;
                let lifecycle = match command {
                    BotCommand::ListLifecycle {
                        lifecycle: BotLifecycle::Archived,
                        ..
                    } => person::Lifecycle::Archived,
                    BotCommand::ListLifecycle {
                        lifecycle: BotLifecycle::Trashed,
                        ..
                    } => person::Lifecycle::Trashed,
                    _ => person::Lifecycle::Active,
                };
                #[derive(serde::Deserialize)]
                struct Page {
                    items: Vec<person::Bot>,
                    next: Option<Uuid>,
                }
                let mut after = None;
                let mut bots = vec![];
                let mut bytes = 0;
                for _ in 0..16 {
                    let page: Page = self.person_read(
                        owner,
                        Query::Bots {
                            workspace: workspace.as_uuid(),
                            after,
                            lifecycle,
                        },
                    )?;
                    for bot in page.items {
                        let bot = self.bot_view(owner, bot)?;
                        bytes += serde_json::to_vec(&bot)
                            .map_err(|_| Error::EngineUnavailable)?
                            .len();
                        if bytes > c::MAX_RECORD_BYTES {
                            return Err(Error::InputTooSmall);
                        }
                        bots.push(bot);
                    }
                    if page.next.is_none() {
                        return Ok(BotResponse::List { bots });
                    }
                    if page.next == after {
                        return Err(Error::EngineUnavailable);
                    }
                    after = page.next;
                }
                Err(Error::InputTooSmall)
            }
            BotCommand::ListBotCreations {
                workspace,
                cursor,
                limit,
            } => {
                if limit == 0 || limit > 100 {
                    return Err(Error::InvalidRequest);
                }
                self.window_workspace(owner, workspace)?;
                let mut after = None;
                let mut records = vec![];
                // The byte bound can shorten a page below its row limit.
                // Follow every cursor, bounded by the owner's pending limit.
                for _ in 0..=c::MAX_PENDING_PER_OWNER {
                    let c::Reply::Pending(page) =
                        self.creation_call(owner, c::Command::Pending { after })?
                    else {
                        return Err(Error::EngineUnavailable);
                    };
                    if page.is_empty() {
                        break;
                    }
                    let next = page.last().map(|record| record.request);
                    if next == after {
                        return Err(Error::EngineUnavailable);
                    }
                    after = next;
                    records.extend(
                        page.into_iter()
                            .filter(|record| record.definition.access.workspace == workspace),
                    );
                }
                records.sort_by_key(|record| (record.created_at_ms, record.request));
                if let Some(cursor) = cursor {
                    records.retain(|record| {
                        (record.created_at_ms, record.request)
                            > (cursor.created_at_ms, cursor.request_id.as_uuid())
                    });
                }
                let more = records.len() > usize::from(limit);
                records.truncate(usize::from(limit));
                let next_cursor = if more {
                    records.last().map(|record| BotCreationCursor {
                        created_at_ms: record.created_at_ms,
                        request_id: BotRequestId::from_uuid(record.request),
                    })
                } else {
                    None
                };
                let creations = records
                    .into_iter()
                    .map(|record| self.creation_view(owner, record))
                    .collect::<Result<_, _>>()?;
                Ok(BotResponse::Creations {
                    page: BotCreationPage {
                        creations,
                        next_cursor,
                    },
                })
            }
            command => self.window_management(owner, command),
        }
    }
}
