// SPDX-License-Identifier: MPL-2.0
//! Existing Routine controls project the native engine ledger. No Schedule
//! document, legacy AI database or Run identity is constructed here.
use super::*;
use koru_bots_engine::{messages::Receipt, routines as native};
use koru_delegate_core::{RoutineControlLifecycle, RoutineControlOutcome, ScheduleId};

pub(super) fn handles(command: &BotCommand) -> bool {
    matches!(
        command,
        BotCommand::ReadRoutines { .. }
            | BotCommand::ReadRoutineDetail { .. }
            | BotCommand::CreateRoutine { .. }
            | BotCommand::ReadRoutineCreation { .. }
            | BotCommand::RetryRoutineCreation { .. }
            | BotCommand::SetRoutineLifecycle { .. }
            | BotCommand::EditRoutine { .. }
            | BotCommand::UpdateRoutineAccess { .. }
    )
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn creation(row: &native::Routine) -> BotRoutineCreationObservation {
    BotRoutineCreationObservation {
        request_id: BotRequestId::from_uuid(row.created_by),
        state: BotRoutineCreationState::Bound,
        refusal: None,
        updated_at_ms: row.created_at_ms,
    }
}
fn access(row: &native::Routine, initial: bool) -> BotRoutineGrantSummary {
    let policy = if initial {
        &row.initial_access
    } else {
        &row.access
    };
    BotRoutineGrantSummary {
        source_bot_policy_revision: row.source_policy_revision,
        grant_revision: if initial { 1 } else { row.grant_revision },
        access: match policy.access {
            koru_delegate_core::BotAccess::Default => BotAccess::Default,
            koru_delegate_core::BotAccess::Workspace => BotAccess::Workspace,
        },
        selected_page_count: policy.selected_pages.len() as u16,
    }
}
fn state(row: &native::Routine, owner: BotOwner) -> RoutineScheduleState {
    routine_control_state(
        if row.enabled {
            RoutineControlLifecycle::Active
        } else {
            RoutineControlLifecycle::Paused
        },
        owner.device,
    )
}
fn next(row: &native::Routine) -> RoutineNextObservation {
    if !row.enabled {
        RoutineNextObservation::Inactive {}
    } else {
        row.next_due_ms
            .map(|at_ms| RoutineNextObservation::Exact { at_ms })
            .unwrap_or(RoutineNextObservation::Inactive {})
    }
}
fn model(bot: &person::Bot) -> Result<BotRoutineModelObservation, Error> {
    let model = model_view(&bot.definition.configuration)?;
    Ok(BotRoutineModelObservation::Configured {
        provider: model.provider,
        requested_model: model.requested_model,
    })
}
fn list_item(row: native::Routine, owner: BotOwner) -> Result<BotRoutineListItem, Error> {
    Ok(BotRoutineListItem {
        schedule_id: ScheduleId::from_uuid(row.id),
        creation: creation(&row),
        grant: access(&row, false),
        live: BotRoutineLiveObservation::Current {
            name: RoutineScheduleName::new(row.name.clone())
                .map_err(|_| Error::StorageUnavailable)?,
            definition_revision: row.revision,
            state: state(&row, owner),
            next: next(&row),
        },
        local_conversations: BotRoutineLocalConversations::Available {
            items: vec![],
            more: false,
        },
    })
}
// Old definition controls have revision identity instead of request identity.
// Their stable digest bridges exact retries into the engine's receipt table.
fn control_id(owner: BotOwner, command: &BotCommand) -> Result<Uuid, Error> {
    let bytes = serde_json::to_vec(&(owner.user_scope, owner.device, command))
        .map_err(|_| Error::InvalidRequest)?;
    let digest = blake3::hash(&bytes);
    let mut id = [0; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id[6] = (id[6] & 15) | 64;
    id[8] = (id[8] & 63) | 128;
    Ok(Uuid::from_bytes(id))
}
impl Gateway {
    pub(in crate::bots_engine) fn routine_command(
        &self,
        principal: &GatewayPrincipal,
        command: koru_ai_wire::bot_routines::Command,
    ) -> Result<koru_ai_wire::bot_routines::Response, Error> {
        use koru_ai_wire::bot_routines as wire;
        fn convert<T: serde::de::DeserializeOwned>(
            value: impl serde::Serialize,
        ) -> Result<T, Error> {
            serde_json::from_value(
                serde_json::to_value(value).map_err(|_| Error::EngineUnavailable)?,
            )
            .map_err(|_| Error::EngineUnavailable)
        }
        if !permitted(principal) {
            return Err(Error::NotAllowed);
        }
        let owner = self.authority.owner().map_err(authority_error)?;
        match command {
            wire::Command::Create {
                id,
                bot,
                expected_bot_revision,
                name,
                objective,
                recurrence,
            } => {
                let result = self.routine_change(
                    owner,
                    id,
                    native::Change::Create {
                        bot: bot.as_uuid(),
                        expected_bot_revision,
                        name,
                        objective,
                        recurrence,
                        documents: vec![],
                    },
                )?;
                Ok(wire::Response::Created {
                    request: id,
                    item: list_item(result.answer.routine, owner)?,
                    first_enabled: result.answer.first_enabled,
                    replayed: result.replayed,
                })
            }
            wire::Command::Reviews { bot, before } => {
                let page: native::review::Page = self.routine_read(
                    owner,
                    Query::RoutineReviews {
                        bot: bot.as_uuid(),
                        before: before.map(|v| native::review::Cursor {
                            at_ms: v.at_ms,
                            id: v.id,
                        }),
                    },
                )?;
                Ok(wire::Response::Reviews {
                    page: convert(page)?,
                })
            }
            wire::Command::Review {
                bot,
                occurrence,
                offset,
            } => {
                let detail: native::review::Detail = self.routine_read(
                    owner,
                    Query::RoutineReview {
                        bot: bot.as_uuid(),
                        occurrence,
                        offset,
                    },
                )?;
                Ok(wire::Response::Review {
                    detail: convert(detail)?,
                })
            }
            wire::Command::SkipReview {
                id,
                bot,
                occurrence,
            } => {
                let result: Receipt<native::review::Item> =
                    serde_json::from_value(self.execute_owner(
                        owner,
                        Command::SkipRoutineReview {
                            id,
                            bot: bot.as_uuid(),
                            occurrence,
                        },
                    )?)
                    .map_err(|_| Error::EngineUnavailable)?;
                Ok(wire::Response::SkippedReview {
                    request: id,
                    item: convert(result.answer)?,
                    replayed: result.replayed,
                })
            }
            wire::Command::SaveReview {
                id,
                bot,
                occurrence,
                title,
                document,
            } => {
                let view = self.bot_view(
                    owner,
                    self.routine_read(owner, Query::Bot { bot: bot.as_uuid() })?,
                )?;
                let workspace = view.home_workspace;
                let profile = self.window_profile(owner, workspace)?;
                // The whole retained result, chunk by chunk, read under the
                // owner's authority; past a document's limit it is refused.
                let mut words = String::new();
                let mut offset = Some(0);
                while let Some(at) = offset {
                    let detail: native::review::Detail = self.routine_read(
                        owner,
                        Query::RoutineReview {
                            bot: bot.as_uuid(),
                            occurrence,
                            offset: at,
                        },
                    )?;
                    words.push_str(&detail.text);
                    if words.len() > koru_ai_core::bots::BOT_ROLE_MARKDOWN_LIMIT {
                        return Err(Error::InvalidRequest);
                    }
                    offset = detail.next.filter(|next| *next > at);
                }
                let (workspace, title) = self.save_words(
                    owner,
                    workspace,
                    profile,
                    id,
                    document,
                    &title,
                    &format!("{}\n{occurrence}", bot.as_uuid()),
                    &words,
                )?;
                Ok(wire::Response::SavedReview {
                    request: id,
                    document,
                    workspace: workspace.as_uuid(),
                    title,
                })
            }
            wire::Command::ClaimStartup { id, bot } => {
                let result: Receipt<bool> = serde_json::from_value(self.execute_owner(
                    owner,
                    Command::ClaimRoutineStartup {
                        id,
                        bot: bot.as_uuid(),
                    },
                )?)
                .map_err(|_| Error::EngineUnavailable)?;
                Ok(wire::Response::Startup {
                    request: id,
                    first_enabled: result.answer,
                    replayed: result.replayed,
                })
            }
            wire::Command::History {
                bot,
                routine,
                before,
            } => {
                let native: native::Detail = self.routine_read(
                    owner,
                    Query::Routine {
                        bot: bot.as_uuid(),
                        routine,
                        before: before.clone().map(|c| native::Cursor {
                            at_ms: c.at_ms,
                            id: c.id,
                        }),
                        limit: 16,
                    },
                )?;
                let BotResponse::RoutineDetail { reply } = self.window_routines(
                    owner,
                    BotCommand::ReadRoutineDetail {
                        bot_id: bot,
                        schedule_id: ScheduleId::from_uuid(routine),
                        before_occurrence_ms: before.map(|c| c.at_ms),
                        history_limit: 16,
                    },
                )?
                else {
                    return Err(Error::EngineUnavailable);
                };
                // Independent bounded reads may straddle a human edit. Do not
                // combine an old grant/history with newer controls.
                if !matches!(&reply.detail.live,RoutineDetailLive::Current {definition_revision,..} if *definition_revision==native.routine.revision)
                    || reply.detail.access.grant_revision != native.routine.grant_revision
                {
                    return Err(Error::RevisionConflict);
                }
                Ok(wire::Response::History {
                    snapshot: wire::Snapshot {
                        detail: reply.detail,
                        thread: ChatId::from_uuid(native.routine.thread),
                        selected_pages: native
                            .routine
                            .access
                            .selected_pages
                            .into_iter()
                            .map(|document| BotPage {
                                workspace: native.routine.access.workspace,
                                document,
                            })
                            .collect(),
                        occurrences: convert(native.occurrences)?,
                        next: convert(native.next)?,
                    },
                })
            }
            wire::Command::RunNow {
                id,
                bot,
                routine,
                revision,
            } => {
                let result = self.routine_change(
                    owner,
                    id,
                    native::Change::RunNow {
                        bot: bot.as_uuid(),
                        routine,
                        revision,
                    },
                )?;
                Ok(wire::Response::Started {
                    request: id,
                    occurrence: convert(result.answer.occurrence.ok_or(Error::EngineUnavailable)?)?,
                    replayed: result.replayed,
                })
            }
            wire::Command::Enable {
                id,
                bot,
                routine,
                revision,
                enabled,
            } => {
                let result = self.routine_change(
                    owner,
                    id,
                    native::Change::Enable {
                        bot: bot.as_uuid(),
                        routine,
                        revision,
                        enabled,
                    },
                )?;
                let row = result.answer.routine;
                Ok(wire::Response::Enabled {
                    request: id,
                    control: BotRoutineControl {
                        schedule_id: ScheduleId::from_uuid(routine),
                        action: if enabled {
                            BotRoutineLifecycleAction::Enable
                        } else {
                            BotRoutineLifecycleAction::Pause
                        }
                        .into(),
                        outcome: if result.replayed {
                            RoutineControlOutcome::AlreadyApplied
                        } else {
                            RoutineControlOutcome::Applied
                        },
                        definition_revision: row.revision,
                        state: state(&row, owner),
                        next: next(&row),
                    },
                    first_enabled: result.answer.first_enabled,
                    replayed: result.replayed,
                })
            }
        }
    }
    fn routine_read<T: serde::de::DeserializeOwned>(
        &self,
        owner: BotOwner,
        query: Query,
    ) -> Result<T, Error> {
        serde_json::from_value(self.execute_owner(owner, Command::Read { query })?)
            .map_err(|_| Error::EngineUnavailable)
    }
    fn routine_change(
        &self,
        owner: BotOwner,
        id: Uuid,
        action: native::Change,
    ) -> Result<Receipt<native::Changed>, Error> {
        serde_json::from_value(self.execute_owner(owner, Command::Routine { id, action })?)
            .map_err(|_| Error::EngineUnavailable)
    }
    pub(super) fn window_routines(
        &self,
        owner: BotOwner,
        command: BotCommand,
    ) -> Result<BotResponse, Error> {
        let control_id = control_id(owner, &command)?;
        match command {
            BotCommand::ReadRoutines {
                bot_id,
                after_schedule,
                limit,
            } => {
                let bot: person::Bot = self.person_read(
                    owner,
                    Query::Bot {
                        bot: bot_id.as_uuid(),
                    },
                )?;
                let page: native::Page = self.routine_read(
                    owner,
                    Query::Routines {
                        bot: bot_id.as_uuid(),
                        after: after_schedule.map(|id| id.as_uuid()),
                        limit,
                    },
                )?;
                Ok(BotResponse::Routines {
                    page: BotRoutineListPage {
                        bot_id,
                        bot_revision: bot.revision,
                        effective_model: model(&bot)?,
                        items: page
                            .items
                            .into_iter()
                            .map(|row| list_item(row, owner))
                            .collect::<Result<_, _>>()?,
                        next_schedule: page.next.map(ScheduleId::from_uuid),
                    },
                })
            }
            BotCommand::ReadRoutineDetail {
                bot_id,
                schedule_id,
                before_occurrence_ms,
                history_limit,
            } => {
                let bot: person::Bot = self.person_read(
                    owner,
                    Query::Bot {
                        bot: bot_id.as_uuid(),
                    },
                )?;
                let detail: native::Detail = self.routine_read(
                    owner,
                    Query::Routine {
                        bot: bot_id.as_uuid(),
                        routine: schedule_id.as_uuid(),
                        before: before_occurrence_ms.map(|at_ms| native::Cursor {
                            at_ms,
                            id: Uuid::nil(),
                        }),
                        limit: history_limit,
                    },
                )?;
                let row = detail.routine;
                let folder = bot
                    .definition
                    .configuration
                    .working_folder
                    .clone()
                    .unwrap_or_else(|| {
                        koru_bots_engine::folders::managed_path(&self.root, bot.id)
                            .to_string_lossy()
                            .into_owned()
                    });
                let destination = RoutineDestinationObservation::Current {
                    home: row.access.workspace,
                    working_folder: koru_delegate_core::BotWorkingFolder::new(folder)
                        .map_err(|_| Error::StorageUnavailable)?,
                };
                let owner_zone = match row.recurrence.zone_name() {
                    Some(iana) => RoutineOwnerZoneObservation::Fixed { iana: iana.into() },
                    None => RoutineOwnerZoneObservation::OwnerLocal {},
                };
                // The legacy history can express final timing facts, but has
                // no engine thread identity. Native Detail is authoritative
                // for queued/cancelled states and opening an occurrence.
                let items = detail
                    .occurrences
                    .into_iter()
                    .filter_map(|entry| {
                        let outcome = match entry.state.as_str() {
                            "running" | "answered" => RoutineOccurrenceOutcome::Ran,
                            "missed" => RoutineOccurrenceOutcome::Missed,
                            "skipped" => RoutineOccurrenceOutcome::SkippedBusy,
                            _ => return None,
                        };
                        Some(RoutineOccurrenceLine {
                            occurrence_ms: entry.occurrence_ms,
                            outcome,
                            test: entry.manual,
                            recorded_at_ms: entry.recorded_at_ms,
                            conversation: None,
                        })
                    })
                    .collect();
                let detail = BotRoutineDetail {
                    bot_id,
                    schedule_id,
                    bot_revision: bot.revision,
                    observed_at_ms: now(),
                    creation: creation(&row),
                    access: access(&row, false),
                    access_at_setup: access(&row, true),
                    configured_model: model(&bot)?,
                    destination,
                    live: RoutineDetailLive::Current {
                        name: RoutineScheduleName::new(row.name.clone())
                            .map_err(|_| Error::StorageUnavailable)?,
                        definition_revision: row.revision,
                        effective_at_ms: row.effective_at_ms,
                        recurrence: row.recurrence.clone(),
                        state: state(&row, owner),
                        owner_zone,
                        next: next(&row),
                    },
                    readiness: RoutineReadinessObservation::NotAssessed {},
                    local_history: RoutineLocalHistoryObservation::Available {
                        items: vec![],
                        next_before_occurrence_ms: None,
                    },
                    occurrences: RoutineOccurrenceHistoryObservation::Available {
                        items,
                        next_before_occurrence_ms: detail.next.map(|cursor| cursor.at_ms),
                    },
                };
                Ok(BotResponse::RoutineDetail {
                    reply: BotRoutineDetailEnvelope { detail },
                })
            }
            BotCommand::CreateRoutine {
                request_id,
                bot_id,
                expected_bot_revision,
                name,
                objective,
                documents,
                recurrence,
            } => {
                let result = self.routine_change(
                    owner,
                    request_id.as_uuid(),
                    native::Change::Create {
                        bot: bot_id.as_uuid(),
                        expected_bot_revision,
                        name: name.as_str().into(),
                        objective: objective.as_str().into(),
                        documents: documents.into_iter().map(|id| id.as_uuid()).collect(),
                        recurrence,
                    },
                )?;
                Ok(BotResponse::RoutineCreated {
                    receipt: BotRoutineCreateResult {
                        request_id,
                        bot_id,
                        schedule_id: ScheduleId::from_uuid(result.answer.routine.id),
                        state: BotRoutineCreationState::Bound,
                        refusal: None,
                    },
                })
            }
            BotCommand::ReadRoutineCreation { bot_id, request_id }
            | BotCommand::RetryRoutineCreation {
                bot_id,
                original_request_id: request_id,
                ..
            } => {
                let row: native::Routine = self.routine_read(
                    owner,
                    Query::RoutineCreation {
                        bot: bot_id.as_uuid(),
                        request: request_id.as_uuid(),
                    },
                )?;
                Ok(BotResponse::RoutineCreation {
                    creation: BotRoutineCreationStatus {
                        request_id,
                        bot_id,
                        schedule_id: ScheduleId::from_uuid(row.id),
                        observed_token: BotRoutineCreationObserved {
                            state: BotRoutineCreationState::Bound,
                            updated_at_ms: row.created_at_ms,
                        },
                        refusal: None,
                        can_retry: false,
                        source_bot_policy_revision: row.source_policy_revision,
                        grant_revision: row.grant_revision,
                    },
                })
            }
            BotCommand::SetRoutineLifecycle {
                bot_id,
                schedule_id,
                expected_definition_revision,
                action,
            } => {
                let result = self.routine_change(
                    owner,
                    control_id,
                    native::Change::Enable {
                        bot: bot_id.as_uuid(),
                        routine: schedule_id.as_uuid(),
                        revision: expected_definition_revision,
                        enabled: action == BotRoutineLifecycleAction::Enable,
                    },
                )?;
                let row = result.answer.routine;
                Ok(BotResponse::RoutineControl {
                    control: BotRoutineControl {
                        schedule_id,
                        action: action.into(),
                        outcome: if result.replayed {
                            RoutineControlOutcome::AlreadyApplied
                        } else {
                            RoutineControlOutcome::Applied
                        },
                        definition_revision: row.revision,
                        state: state(&row, owner),
                        next: next(&row),
                    },
                })
            }
            BotCommand::EditRoutine {
                bot_id,
                schedule_id,
                expected_definition_revision,
                expected_name,
                name,
                recurrence,
            } => {
                let detail: native::Detail = self.routine_read(
                    owner,
                    Query::Routine {
                        bot: bot_id.as_uuid(),
                        routine: schedule_id.as_uuid(),
                        before: None,
                        limit: 1,
                    },
                )?;
                if detail.routine.revision == expected_definition_revision
                    && detail.routine.name != expected_name.as_str()
                {
                    return Err(Error::RevisionConflict);
                }
                let result = self.routine_change(
                    owner,
                    control_id,
                    native::Change::Timing {
                        bot: bot_id.as_uuid(),
                        routine: schedule_id.as_uuid(),
                        revision: expected_definition_revision,
                        name: name.as_str().into(),
                        recurrence,
                    },
                )?;
                let row = result.answer.routine;
                Ok(BotResponse::RoutineControl {
                    control: BotRoutineControl {
                        schedule_id,
                        action: BotRoutineControlAction::Edit,
                        outcome: if result.replayed {
                            RoutineControlOutcome::AlreadyApplied
                        } else {
                            RoutineControlOutcome::Applied
                        },
                        definition_revision: row.revision,
                        state: state(&row, owner),
                        next: next(&row),
                    },
                })
            }
            BotCommand::UpdateRoutineAccess {
                request_id,
                bot_id,
                schedule_id,
                expected_grant_revision,
                access: choice,
                selected_pages,
            } => {
                let bot: person::Bot = self.person_read(
                    owner,
                    Query::Bot {
                        bot: bot_id.as_uuid(),
                    },
                )?;
                let home = bot.definition.access.workspace;
                if selected_pages.iter().any(|page| page.workspace != home) {
                    return Err(Error::NotAllowed);
                }
                let policy = koru_delegate_core::CapturedBotPolicy {
                    workspace: home,
                    destination: home,
                    access: match choice {
                        BotAccess::Default => koru_delegate_core::BotAccess::Default,
                        BotAccess::Workspace => koru_delegate_core::BotAccess::Workspace,
                    },
                    selected_pages: selected_pages
                        .into_iter()
                        .map(|page| page.document)
                        .collect(),
                    connections: Default::default(),
                };
                let result = self.routine_change(
                    owner,
                    request_id.as_uuid(),
                    native::Change::Access {
                        bot: bot_id.as_uuid(),
                        routine: schedule_id.as_uuid(),
                        revision: expected_grant_revision,
                        access: policy,
                    },
                )?;
                Ok(BotResponse::RoutineAccess {
                    access: BotRoutineAccess::of(
                        schedule_id,
                        &access(&result.answer.routine, false),
                    ),
                })
            }
            _ => Err(Error::InvalidRequest),
        }
    }
}
