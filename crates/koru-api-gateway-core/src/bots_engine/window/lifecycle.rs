// SPDX-License-Identifier: MPL-2.0
//! Existing lifecycle forms project the engine's exact reversal identities.
use super::*;

impl Gateway {
    pub(super) fn holder_view(
        holder: person::LifecycleHolder,
    ) -> Result<BotLifecycleHolder, Error> {
        Ok(BotLifecycleHolder {
            request_id: BotRequestId::from_uuid(holder.request),
            kind: match holder.lifecycle {
                person::Lifecycle::Archived => BotLifecycleKind::Archive,
                person::Lifecycle::Trashed => BotLifecycleKind::Trash,
                _ => return Err(Error::StorageUnavailable),
            },
            completed_at_ms: holder.completed_at_ms,
        })
    }

    fn lifecycle_view(operation: person::LifecycleOperation) -> Result<BotResponse, Error> {
        let kind = match (
            operation.source.is_some(),
            operation.thread.is_some(),
            operation.lifecycle,
        ) {
            (true, false, _) => BotLifecycleKind::Restore,
            (true, true, _) => BotLifecycleKind::RestoreChat,
            (false, false, person::Lifecycle::Archived) => BotLifecycleKind::Archive,
            (false, false, person::Lifecycle::Trashed) => BotLifecycleKind::Trash,
            (false, true, person::Lifecycle::Trashed) => BotLifecycleKind::TrashChat,
            _ => return Err(Error::InvalidRequest),
        };
        let chat_id = operation.thread.map(ChatId::from_uuid);
        Ok(BotResponse::Lifecycle {
            operation: BotLifecycleProgress {
                request_id: BotRequestId::from_uuid(operation.request),
                bot_id: BotId::from_uuid(operation.bot),
                kind,
                chat_id,
                source_request_id: operation.source.map(BotRequestId::from_uuid),
                stage: if operation.reversed_by.is_some() {
                    BotLifecycleStage::Reversed
                } else {
                    BotLifecycleStage::Completed
                },
                refusal: None,
                members: vec![BotLifecycleMemberOutcome {
                    subject: chat_id
                        .map(|chat_id| BotLifecycleSubject::Chat { chat_id })
                        .unwrap_or(BotLifecycleSubject::Bot),
                    intended: if operation.source.is_some() {
                        BotLifecycleIntended::Restore
                    } else {
                        BotLifecycleIntended::Hide
                    },
                    stage: BotLifecycleMemberStage::Acknowledged,
                    retained: None,
                }],
                left_in_place: BotLeftInPlace::of(kind),
                routines_needing_access: vec![],
                routines_still_on: vec![],
            },
        })
    }

    pub(super) fn window_lifecycle(
        &self,
        owner: BotOwner,
        command: BotCommand,
    ) -> Result<BotResponse, Error> {
        let (bot_id, request_id) = match command {
            BotCommand::Archive {
                request_id,
                bot_id,
                expected_revision,
            }
            | BotCommand::Trash {
                request_id,
                bot_id,
                expected_revision,
            }
            | BotCommand::Restore {
                request_id,
                bot_id,
                expected_revision,
                ..
            } => {
                let (lifecycle, source) = match command {
                    BotCommand::Archive { .. } => (person::Lifecycle::Archived, None),
                    BotCommand::Trash { .. } => (person::Lifecycle::Trashed, None),
                    BotCommand::Restore {
                        source_request_id, ..
                    } => (person::Lifecycle::Active, Some(source_request_id.as_uuid())),
                    _ => unreachable!(),
                };
                let _: person::Bot = self.window_change(
                    owner,
                    request_id,
                    Change::BotLifecycle {
                        bot: bot_id.as_uuid(),
                        revision: expected_revision,
                        lifecycle,
                        source,
                    },
                )?;
                (bot_id, request_id)
            }
            BotCommand::TrashChat {
                request_id,
                bot_id,
                chat_id,
                expected_revision,
            }
            | BotCommand::RestoreChat {
                request_id,
                bot_id,
                chat_id,
                expected_revision,
                ..
            } => {
                let (lifecycle, source) = match command {
                    BotCommand::RestoreChat {
                        source_request_id, ..
                    } => (person::Lifecycle::Active, Some(source_request_id.as_uuid())),
                    _ => (person::Lifecycle::Trashed, None),
                };
                let _: person::Thread = self.window_change(
                    owner,
                    request_id,
                    Change::ConversationLifecycle {
                        bot: bot_id.as_uuid(),
                        revision: expected_revision,
                        thread: chat_id.as_uuid(),
                        lifecycle,
                        source,
                    },
                )?;
                (bot_id, request_id)
            }
            BotCommand::ReadLifecycleOperation { request_id, bot_id } => (bot_id, request_id),
            _ => return Err(Error::EngineUnavailable),
        };
        Self::lifecycle_view(self.person_read(
            owner,
            Query::LifecycleOperation {
                bot: bot_id.as_uuid(),
                request: request_id.as_uuid(),
            },
        )?)
    }
}
