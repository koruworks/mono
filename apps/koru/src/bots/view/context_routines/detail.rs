// SPDX-License-Identifier: MPL-2.0
//! Existing context elements only; display data never supplies opening authority.
use super::*;
use crate::bots::routines::detail::Data;
use crate::schedule_words::{ScheduleProjection, schedule_words};
use koru_i18n::time::LocaleFormat;

pub(super) const REFRESH_DETAIL: Key = Key::surface(SURFACE, "detail-refresh");
pub(super) const NEXT_HISTORY: Key = Key::surface(SURFACE, "detail-history-next");
pub(super) fn heading(data: &Data, schedule: ScheduleId, lang: Lang) -> Option<(String, Words)> {
    let detail = data.held.as_ref().filter(|d| d.schedule_id == schedule)?;
    Some(match &detail.live {
        RoutineDetailLive::Current { name, state, .. } => (
            name.as_str().to_owned(),
            match state {
                ScheduleState::Active { .. } => Words::ScheduleNavigatorActive,
                ScheduleState::Paused { .. } => Words::ScheduleNavigatorPaused,
                ScheduleState::HandoffPending { .. } => Words::ScheduleNavigatorHandoffPending,
                ScheduleState::Retired => Words::BotsRoutineRetired,
            },
        ),
        RoutineDetailLive::NotLive {} => (
            t(lang, Words::BotsRoutine).into(),
            Words::BotsRoutineNotLive,
        ),
        RoutineDetailLive::Unavailable {} => (
            t(lang, Words::BotsRoutineSetup).into(),
            Words::BotsRoutineLiveUnavailable,
        ),
    })
}
pub(super) fn keys(data: &Data, enabled_only: bool) -> Vec<Key> {
    if data.selected.is_none() {
        return vec![];
    }
    let mut keys = Vec::new();
    if !enabled_only || !data.loading {
        keys.push(REFRESH_DETAIL);
    }
    if let Some(detail) = &data.held {
        if let RoutineLocalHistoryObservation::Available {
            items,
            next_before_occurrence_ms,
        } = &detail.local_history
        {
            if next_before_occurrence_ms.is_some() && (!enabled_only || !data.loading) {
                keys.push(NEXT_HISTORY);
            }
            keys.extend(
                items
                    .iter()
                    .filter(|r| {
                        !enabled_only
                            || (data.navigable
                                && r.conversation == BotRoutineConversationVisibility::Available)
                    })
                    .map(|r| chat_key(r.chat_id)),
            );
        }
    }
    keys
}
pub(super) fn action(data: &Data, key: Key) -> Option<Action> {
    if !keys(data, true).contains(&key) {
        return None;
    }
    if key == REFRESH_DETAIL {
        return Some(Action::RefreshDetail);
    }
    if key == NEXT_HISTORY {
        return Some(Action::NextHistory);
    }
    let RoutineLocalHistoryObservation::Available { items, .. } =
        &data.held.as_ref()?.local_history
    else {
        return None;
    };
    let chat = items.iter().find(|r| chat_key(r.chat_id) == key)?.chat_id;
    ChatId::parse(&chat.to_string()).map(Action::Open)
}
pub(super) fn content(data: &Data, surface: &Surface, engine: &mut TextEngine, lang: Lang) -> El {
    let mut content = col("Selected Routine detail").gap(Space::S1);
    if let Some(notice) = data.notice {
        content = content.child(line(t(lang, notice)));
    }
    if data.loading {
        content = content.child(line(t(lang, Words::BotsLoading)));
    }
    if data.stale && data.notice != Some(Words::BotsRoutinesStale) {
        content = content.child(line(t(lang, Words::BotsRoutinesStale)));
    }
    if let Some(detail) = &data.held {
        content = content.child(line(match &detail.configured_model {
            BotRoutineModelObservation::Configured {
                provider,
                requested_model,
            } => fill(
                t(lang, Words::BotsRoutineModel),
                &[provider.as_str(), requested_model],
            ),
            _ => t(lang, Words::BotsRoutineModelUnavailable).into(),
        }));
        let count = detail.access_at_setup.selected_page_count.to_string();
        content = content.child(line(match detail.access_at_setup.access {
            koru_ai_core::bots::BotAccess::Default => {
                fill(t(lang, Words::BotsRoutineAccessDefault), &[&count])
            }
            koru_ai_core::bots::BotAccess::Workspace => {
                t(lang, Words::BotsRoutineAccessWorkspace).into()
            }
        }));
        match &detail.live {
            RoutineDetailLive::Current {
                effective_at_ms,
                recurrence,
                next,
                ..
            } => {
                let exact = match next {
                    RoutineNextObservation::Exact { at_ms } => {
                        koru_recurrence::Occurrence::from_instant_ms(*at_ms).ok()
                    }
                    _ => None,
                };
                let projection = match recurrence {
                    koru_recurrence::RecurrenceRule::Floating { .. } => {
                        ScheduleProjection::owner_local(recurrence)
                    }
                    koru_recurrence::RecurrenceRule::Zoned { .. } => {
                        ScheduleProjection::zoned(recurrence, exact)
                    }
                };
                if let Some(words) = projection.ok().and_then(|p| {
                    schedule_words(&p, &LocaleFormat::for_system(Some(lang.tag()), None)).ok()
                }) {
                    content = content.child(line(words.rule));
                    if matches!(
                        next,
                        RoutineNextObservation::Exact { .. }
                            | RoutineNextObservation::OwnerLocal {}
                    ) {
                        if let Some(next) = words.next {
                            content = content.child(line(next));
                        }
                    }
                }
                if matches!(next, RoutineNextObservation::Exact { .. }) && exact.is_none() {
                    content = content.child(line(t(lang, Words::BotsRoutineDateUnavailable)));
                }
                if matches!(next, RoutineNextObservation::Inactive {}) {
                    content = content.child(line(t(lang, Words::BotsRoutineNextInactive)));
                }
                if matches!(next, RoutineNextObservation::Unavailable {}) {
                    content = content.child(line(t(lang, Words::BotsRoutineNextUnavailable)));
                }
                content = content.child(line(fill(
                    t(lang, Words::BotsRoutineEffectiveFrom),
                    &[&date(lang, *effective_at_ms)],
                )));
            }
            RoutineDetailLive::NotLive {} => {
                content = content.child(line(t(lang, Words::BotsRoutineNotLive)))
            }
            RoutineDetailLive::Unavailable {} => {
                content = content.child(line(t(lang, Words::BotsRoutineLiveUnavailable)))
            }
        }
        content = content.child(line(match &detail.destination {
            RoutineDestinationObservation::Current { working_folder, .. } => fill(
                t(lang, Words::BotsRoutineDestination),
                &[working_folder.as_str()],
            ),
            _ => t(lang, Words::BotsRoutineDestinationUnavailable).into(),
        }));
        match &detail.readiness {
            RoutineReadinessObservation::Blocked { reasons } => {
                content = content.child(line(t(lang, Words::BotsRoutineKnownBlockers)));
                for reason in reasons {
                    content = content.child(line(t(
                        lang,
                        match reason {
                            RoutineReadinessReason::BotPaused => Words::BotsPaused,
                            RoutineReadinessReason::BotArchived => Words::BotsArchived,
                            RoutineReadinessReason::CreationNotBound => Words::BotsRoutineSetup,
                            RoutineReadinessReason::SchedulePaused => {
                                Words::ScheduleNavigatorPaused
                            }
                            RoutineReadinessReason::HandoffPending => {
                                Words::ScheduleNavigatorHandoffPending
                            }
                            RoutineReadinessReason::ScheduleRetired => Words::BotsRoutineRetired,
                            RoutineReadinessReason::ScheduleNotLive => Words::BotsRoutineNotLive,
                        },
                    )));
                }
            }
            RoutineReadinessObservation::NotAssessed {} => {
                content = content.child(line(t(lang, Words::BotsRoutineReadinessNotAssessed)))
            }
            RoutineReadinessObservation::Unavailable {} => {
                content = content.child(line(t(lang, Words::BotsRoutineReadinessUnavailable)))
            }
        }
        content = content.child(line(t(lang, Words::BotsRoutineLocalConversations)));
        match &detail.local_history {
            RoutineLocalHistoryObservation::Unavailable {} => {
                content = content.child(line(t(lang, Words::BotsRoutineHistoryUnavailable)))
            }
            RoutineLocalHistoryObservation::Available {
                items,
                next_before_occurrence_ms,
            } => {
                if items.is_empty() {
                    content = content.child(line(t(lang, Words::BotsRoutineHistoryEmpty)));
                }
                for reference in items {
                    let enabled = data.navigable
                        && reference.conversation == BotRoutineConversationVisibility::Available;
                    content = content.child(
                        row("Local Routine conversation").child(
                            surface
                                .button(
                                    engine,
                                    chat_key(reference.chat_id),
                                    date(lang, reference.occurrence_ms),
                                    !enabled,
                                )
                                .name_text(fill(
                                    t(lang, Words::BotsRoutineConversation),
                                    &[&date(lang, reference.occurrence_ms)],
                                )),
                        ),
                    );
                    if reference.conversation == BotRoutineConversationVisibility::Unavailable {
                        content = content.child(line(t(lang, Words::BotsConversationUnavailable)));
                    }
                }
                if next_before_occurrence_ms.is_some() {
                    content = content.child(row("Next local conversations").child(surface.button(
                        engine,
                        NEXT_HISTORY,
                        t(lang, Words::BotsRoutineMoreConversations).into(),
                        data.loading,
                    )));
                }
            }
        }
    }
    content.child(row("Refresh selected Routine").child(surface.button(
        engine,
        REFRESH_DETAIL,
        t(lang, Words::BotsRoutinesRefresh).into(),
        data.loading,
    )))
}
