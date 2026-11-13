// SPDX-License-Identifier: MPL-2.0

//! Owner Library commands. Store authority is prepared on the bounded lane;
//! the AI writer only validates that evidence and commits canonical references.
use super::*;
#[cfg(test)]
mod tests;

/// Source identity is retained independently of captured versions. Knowledge
/// must never be inferred back into user task selection or write authority.
#[derive(Clone)]
pub(in crate::worker) struct LibrarySources {
    pub config_revision: u64,
    pub policy_revision: u64,
    pub role: BotPage,
    pub knowledge: Vec<BotPage>,
    pub task_documents: Vec<BotPage>,
}

impl LibrarySources {
    pub(super) fn read(
        repository: &Repository,
        bot: &Bot,
        task_documents: Vec<BotPage>,
    ) -> ApiResult<Self> {
        let library = repository
            .bot_library(
                BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                bot.id,
            )
            .map_err(bot_failure)?;
        if library.binding.is_some() {
            return Err(failure(
                "unsupported_capability",
                "This Bot's Memory cannot yet be captured for a new task.",
                false,
            ));
        }
        validate_knowledge(&task_documents, bot.home_workspace).map_err(bot_failure)?;
        Ok(Self {
            config_revision: bot.revision,
            policy_revision: bot.policy_revision,
            role: bot.config.role.clone(),
            knowledge: library.knowledge,
            task_documents,
        })
    }

    pub(super) fn pages(&self) -> Vec<BotPage> {
        let mut pages = Vec::new();
        for page in std::iter::once(&self.role)
            .chain(&self.knowledge)
            .chain(&self.task_documents)
        {
            if !pages.contains(page) {
                pages.push(page.clone());
            }
        }
        pages
    }

    pub(super) fn validate(
        &self,
        bot: &Bot,
        task: &BotTask,
        pins: &[koru_delegate_core::RunContextItem],
    ) -> ApiResult<()> {
        if self.config_revision != bot.revision
            || self.policy_revision != bot.policy_revision
            || self.role != bot.config.role
        {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        if self.task_documents != task.documents {
            return Err(bot_failure(BotError::Conflict));
        }
        validate_knowledge(&self.knowledge, bot.home_workspace).map_err(bot_failure)?;
        task.validate(bot.home_workspace).map_err(bot_failure)?;
        let required = self
            .pages()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let captured = pins
            .iter()
            .filter_map(|pin| match pin {
                koru_delegate_core::RunContextItem::Document {
                    workspace,
                    document,
                    ..
                } => Some(BotPage {
                    workspace: *workspace,
                    document: *document,
                }),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        if required != captured || pins.len() != captured.len() || pins.len() > 33 {
            return Err(bot_failure(BotError::Forbidden));
        }
        Ok(())
    }

    pub(super) fn capture(&self, intent: &BotSubmissionIntent) -> ApiResult<BotLibraryCapture> {
        let p = &intent.submission.provenance;
        let knowledge = self
            .knowledge
            .iter()
            .map(|page| {
                let version = intent
                    .submission
                    .request
                    .context
                    .iter()
                    .find_map(|pin| match pin {
                        koru_delegate_core::RunContextItem::Document {
                            workspace,
                            document,
                            version,
                        } if *workspace == page.workspace && *document == page.document => {
                            Some(*version)
                        }
                        _ => None,
                    })
                    .ok_or_else(|| bot_failure(BotError::Forbidden))?;
                Ok(BotKnowledgeCapture {
                    page: page.clone(),
                    version,
                })
            })
            .collect::<ApiResult<Vec<_>>>()?;
        let capture = BotLibraryCapture {
            version: 1,
            owner: BotOwner {
                user_scope: p.owner.user_scope,
                device: p.owner.device,
            },
            bot: p.bot,
            submission: p.submission,
            config_revision: p.config_revision,
            knowledge,
            memory: BotCapturedMemory::Absent {},
            index_digest: *blake3::hash(&[]).as_bytes(),
        };
        capture
            .validate(intent.submission.workspace)
            .map_err(bot_failure)?;
        Ok(capture)
    }
}

pub(super) fn command_fingerprint(command: &BotCommand) -> ApiResult<String> {
    Ok(
        blake3::hash(&serde_json::to_vec(command).map_err(|_| bot_failure(BotError::Invalid))?)
            .to_hex()
            .to_string(),
    )
}

impl WorkerState {
    pub(super) fn bot_library_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        let id = match command {
            BotCommand::ReadLibrary { bot_id } | BotCommand::SetKnowledge { bot_id, .. } => *bot_id,
            _ => return None,
        };
        Some((|| {
            authorize(context, AiScope::ManageBots)?;
            let proof = context
                .prepared
                .as_ref()
                .ok_or_else(|| bot_failure(BotError::Unavailable))?;
            let bot = lifecycle::readable_bot(&self.repository, id)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            validate_owner(&proof.authority, &bot)?;
            let owner = BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            };
            match command {
                BotCommand::ReadLibrary { .. } => {
                    let library = self
                        .repository
                        .bot_library(owner, id)
                        .map_err(bot_failure)?;
                    Ok(BotResponse::Library {
                        library: BotLibraryView {
                            bot_id: id,
                            revision: bot.revision,
                            role: bot.config.role,
                            knowledge: library.knowledge,
                            memory: library.binding.map(|binding| binding.page),
                        },
                    })
                }
                BotCommand::SetKnowledge {
                    request_id,
                    expected_revision,
                    pages,
                    ..
                } => {
                    let input = PreparedBotKnowledge {
                        owner,
                        bot: id,
                        request: *request_id,
                        expected_revision: *expected_revision,
                        pages: pages.clone(),
                    };
                    let fingerprint = command_fingerprint(command)?;
                    let prior = self
                        .repository
                        .bot_knowledge_receipt(&input, &fingerprint)
                        .map_err(bot_failure)?;
                    if prior.is_none() {
                        for page in pages {
                            proof.authority.page(page).map_err(bot_failure)?;
                        }
                    }
                    let outcome = self
                        .repository
                        .set_bot_knowledge(&input, &fingerprint, Timestamp::now().millis())
                        .map_err(bot_failure)?;
                    let BotLibraryReceiptV1::SetKnowledge {
                        revision, pages, ..
                    } = outcome
                    else {
                        return Err(bot_failure(BotError::Storage));
                    };
                    // The enclosing bots() route coalesces a boot refresh after
                    // this successful mutation returns; reads never schedule it.
                    Ok(BotResponse::KnowledgeSaved {
                        receipt: BotKnowledgeReceipt {
                            bot_id: id,
                            request_id: *request_id,
                            revision,
                            pages,
                        },
                    })
                }
                _ => unreachable!(),
            }
        })())
    }
}
