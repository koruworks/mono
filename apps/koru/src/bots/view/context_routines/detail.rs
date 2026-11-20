// SPDX-License-Identifier: MPL-2.0
//! Existing context elements only; display data never supplies opening authority.
//!
//! The selected Routine: its timing and time zone, its access now and at
//! setup, and what each past minute became (P4.3 piece 4).
use super::*;
use crate::schedule_words::{ScheduleProjection, schedule_words};
use koru_i18n::time::LocaleFormat;

pub(crate) const REFRESH_DETAIL: Key = Key::surface(SURFACE, "detail-refresh");
pub(crate) const EDIT_TIMING: Key = Key::surface(SURFACE, "detail-edit-timing");
pub(crate) const CHANGE_ACCESS: Key = Key::surface(SURFACE, "detail-change-access");
pub(crate) const ACCESS_DEFAULT: Key = Key::surface(SURFACE, "detail-access-default");
pub(crate) const ACCESS_WORKSPACE: Key = Key::surface(SURFACE, "detail-access-workspace");
pub(crate) const ACCESS_SAVE: Key = Key::surface(SURFACE, "detail-access-save");
pub(crate) const ACCESS_CANCEL: Key = Key::surface(SURFACE, "detail-access-cancel");
pub(crate) const OLDER_HISTORY: Key = Key::surface(SURFACE, "detail-occurrences-older");
pub(crate) fn occurrence_key(id: uuid::Uuid) -> Key {
    Key::model(id.as_u128(), "routine-occurrence")
}
pub(super) fn access_key(access: BotAccess) -> Key {
    match access {
        BotAccess::Default => ACCESS_DEFAULT,
        BotAccess::Workspace => ACCESS_WORKSPACE,
    }
}
pub(super) fn heading(
    data: &crate::bots::routines::detail::Data,
    schedule: ScheduleId,
    lang: Lang,
) -> Option<String> {
    let detail = data.held.as_ref().filter(|d| d.schedule_id == schedule)?;
    Some(match &detail.live {
        RoutineDetailLive::Current { name, .. } => name.as_str().to_owned(),
        RoutineDetailLive::NotLive {} => t(lang, Words::BotsRoutine).into(),
        RoutineDetailLive::Unavailable {} => t(lang, Words::BotsRoutineSetup).into(),
    })
}
/// The access choices, when they are open on the selected Routine.
fn form(all: &Data) -> Option<&control::AccessForm> {
    let selected = all.detail.selected.map(|(_, schedule)| schedule);
    all.controls
        .access
        .as_ref()
        .filter(|form| Some(form.schedule) == selected)
}
/// Whether *Change access* may be pressed: the Routine is paused, nothing is
/// with the owner, and the choices are not open already.
fn access_offered(all: &Data) -> bool {
    all.access_changeable && all.controllable() && form(all).is_none()
}
pub(super) fn keys(all: &Data, enabled_only: bool) -> Vec<Key> {
    let data = &all.detail;
    if data.selected.is_none() {
        return vec![];
    }
    let mut keys = Vec::new();
    if let Some(snapshot) = &data.snapshot {
        keys.extend(
            snapshot
                .occurrences
                .iter()
                .filter(|o| o.message.is_some() && (!enabled_only || data.navigable))
                .map(|o| occurrence_key(o.id)),
        );
        if snapshot.next.is_some() && (!enabled_only || !data.loading) {
            keys.push(OLDER_HISTORY);
        }
    }
    if let Some(detail) = &data.held {
        if matches!(detail.live, RoutineDetailLive::Current { .. }) {
            if !enabled_only || all.timing {
                keys.push(EDIT_TIMING);
            }
            if !enabled_only || access_offered(all) {
                keys.push(CHANGE_ACCESS);
            }
        }
        if form(all).is_some() {
            if !enabled_only || !all.controls.busy() {
                keys.extend([ACCESS_DEFAULT, ACCESS_WORKSPACE, ACCESS_CANCEL]);
                if !enabled_only || data.navigable {
                    keys.push(ACCESS_SAVE);
                }
            }
        }
    }
    if !enabled_only || !data.loading {
        keys.push(REFRESH_DETAIL);
    }

    keys
}
pub(super) fn action(all: &Data, key: Key) -> Option<Action> {
    let data = &all.detail;
    if !keys(all, true).contains(&key) {
        return None;
    }
    match key {
        REFRESH_DETAIL => return Some(Action::RefreshDetail),
        OLDER_HISTORY => return Some(Action::OlderHistory),
        EDIT_TIMING => return Some(Action::EditTiming),
        CHANGE_ACCESS => return Some(Action::ChangeAccess),
        ACCESS_DEFAULT => return Some(Action::ChooseAccess(BotAccess::Default)),
        ACCESS_WORKSPACE => return Some(Action::ChooseAccess(BotAccess::Workspace)),
        ACCESS_SAVE => return Some(Action::SaveAccess),
        ACCESS_CANCEL => return Some(Action::CancelAccess),
        _ => {}
    }
    if let Some(snapshot) = &data.snapshot {
        if snapshot
            .occurrences
            .iter()
            .any(|o| o.message.is_some() && occurrence_key(o.id) == key)
        {
            return Some(Action::Open(snapshot.thread));
        }
    }
    None
}
pub(super) fn content(all: &Data, surface: &Surface, engine: &mut TextEngine, lang: Lang) -> El {
    let data = &all.detail;
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
        // Access now, what it means, and the access it was set up with.
        content = content
            .child(line(access_now(lang, &detail.access)))
            .child(line(match (detail.access.access, &all.home) {
                (BotAccess::Default, _) => t(lang, Words::BotsRoutineAccessAsks).to_owned(),
                (BotAccess::Workspace, Some(home)) => {
                    fill(t(lang, Words::BotsRoutineAccessAnyPage), &[home.as_str()])
                }
                (BotAccess::Workspace, None) => {
                    t(lang, Words::BotsRoutineAccessAnyPageHome).to_owned()
                }
            }));
        let count = detail.access_at_setup.selected_page_count.to_string();
        content = content.child(line(match detail.access_at_setup.access {
            BotAccess::Default => fill(t(lang, Words::BotsRoutineAccessDefault), &[&count]),
            BotAccess::Workspace => t(lang, Words::BotsRoutineAccessWorkspace).into(),
        }));
        let live = matches!(detail.live, RoutineDetailLive::Current { .. });
        if let Some(form) = form(all) {
            content = content.child(access_form(all, form, surface, engine, lang));
        } else if live {
            content = content.child(row("Routine access change").child(surface.button(
                engine,
                CHANGE_ACCESS,
                t(lang, Words::BotsRoutineChangeAccess).into(),
                !access_offered(all),
            )));
            // Why it cannot be changed, said rather than only dimmed.
            if !all.access_changeable && !data.loading && !data.stale {
                content = content.child(line(t(lang, Words::BotsRoutineErrPauseForAccess)));
            }
        }
        match &detail.live {
            RoutineDetailLive::Current {
                effective_at_ms,
                recurrence,
                next,
                owner_zone,
                ..
            } => {
                let exact = match next {
                    RoutineNextObservation::Exact { at_ms } => {
                        koru_recurrence::Occurrence::from_instant_ms(*at_ms).ok()
                    }
                    _ => None,
                };
                let floating =
                    matches!(recurrence, koru_recurrence::RecurrenceRule::Floating { .. });
                let projection = if floating {
                    ScheduleProjection::owner_local(recurrence)
                } else {
                    ScheduleProjection::zoned(recurrence, exact)
                };
                if let Some(words) = projection.ok().and_then(|p| {
                    schedule_words(&p, &LocaleFormat::for_system(Some(lang.tag()), None)).ok()
                }) {
                    content = content.child(line(words.rule));
                    // A floating rule this computer owns has an exact next
                    // time: said as that time, not as the owner's own.
                    if floating && let RoutineNextObservation::Exact { at_ms } = next {
                        content = content.child(line(fill(
                            t(lang, Words::BotsRoutineNextAt),
                            &[&date(lang, *at_ms)],
                        )));
                    } else if matches!(
                        next,
                        RoutineNextObservation::Exact { .. }
                            | RoutineNextObservation::OwnerLocal {}
                    ) && let Some(next) = words.next
                    {
                        content = content.child(line(next));
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
                content = content.child(line(zone_words(lang, owner_zone)));
                content = content.child(line(fill(
                    t(lang, Words::BotsRoutineEffectiveFrom),
                    &[&date(lang, *effective_at_ms)],
                )));
                content = content.child(row("Routine timing").child(surface.button(
                    engine,
                    EDIT_TIMING,
                    t(lang, Words::BotsRoutineEditTiming).into(),
                    !all.timing,
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
        // What each past minute became. Missed work is a line here; nothing
        // is caught up.
        content = content.child(
            text(t(lang, Words::BotsRoutineHistoryTitle))
                .named("Routine history heading")
                .semantics(Role::Heading)
                .name(Words::BotsRoutineHistoryTitle)
                .role(roles::UI),
        );
        if let Some(snapshot) = &data.snapshot {
            if snapshot.occurrences.is_empty() {
                content = content.child(line(t(lang, Words::BotsRoutineNothingRan)));
            }
            for item in &snapshot.occurrences {
                let date = date(lang, item.occurrence_ms);
                let words = match item.state.as_str() {
                    "missed" => fill(t(lang, Words::BotsRoutineMissed), &[&date]),
                    "skipped" => fill(t(lang, Words::BotsRoutineSkippedBusy), &[&date]),
                    state => format!(
                        "{} · {}{}",
                        date,
                        t(
                            lang,
                            match state {
                                "queued" => Words::RunStatusCreated,
                                "running" => Words::RunStatusRunning,
                                "needs_you" => Words::ChatsStatusNeedsYou,
                                "answered" => Words::RunStatusCompleted,
                                "cancelled" => Words::RunStatusCancelled,
                                _ => Words::RunStatusFailed,
                            }
                        ),
                        if item.manual {
                            format!(" · {}", t(lang, Words::BotsRoutineRunNow))
                        } else {
                            String::new()
                        }
                    ),
                };
                let mut entry = col("Routine occurrence").gap(Space::S1);
                entry = entry.child(line(words));
                if let Some(preview) = &item.preview {
                    // This is the exact historical reply, never the thread's
                    // newest answer. The conversation link opens its full log.
                    entry = entry.child(
                        text(&preview.markdown)
                            .named("Routine result preview")
                            .role(roles::UI)
                            .wrap(),
                    );
                }
                if item.message.is_some() {
                    entry = entry.child(row("Open Routine conversation").child(surface.button(
                        engine,
                        occurrence_key(item.id),
                        t(lang, Words::BotsChildOpenConversation).into(),
                        !data.navigable,
                    )));
                }
                content = content.child(entry);
            }
            if snapshot.next.is_some() {
                content = content.child(row("Older Routine occurrences").child(surface.button(
                    engine,
                    OLDER_HISTORY,
                    t(lang, Words::BotsRoutineHistoryOlder).into(),
                    data.loading,
                )));
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
/// The time zone a Routine's times are in: a named zone, this computer's
/// when this computer owns it, or the owner's own.
pub(crate) fn zone_words(lang: Lang, zone: &RoutineOwnerZoneObservation) -> String {
    let value = match zone {
        RoutineOwnerZoneObservation::Fixed { iana } => iana.clone(),
        RoutineOwnerZoneObservation::ThisComputer { iana } => fill(
            t(lang, Words::BotsRoutineZoneThisComputer),
            &[iana.as_str()],
        ),
        RoutineOwnerZoneObservation::OwnerLocal {} => {
            t(lang, Words::BotsRoutineZoneOwnerLocal).to_owned()
        }
    };
    fill(t(lang, Words::BotsRoutineZone), &[&value])
}
/// The two approved choices, what each means, and Save.
fn access_form(
    all: &Data,
    form: &control::AccessForm,
    surface: &Surface,
    engine: &mut TextEngine,
    lang: Lang,
) -> El {
    let saving = all.controls.busy();
    let mut choices = row("Routine access choices").gap(Space::S1);
    for (access, words) in [
        (BotAccess::Default, Words::BotsRoutineAccessChoiceDefault),
        (
            BotAccess::Workspace,
            Words::BotsRoutineAccessChoiceWorkspace,
        ),
    ] {
        choices = choices.child(
            surface
                .button(engine, access_key(access), t(lang, words).into(), saving)
                .selected(form.access == access),
        );
    }
    let group = col("Routine access form")
        .semantics(Role::Group)
        .name(Words::BotsRoutineChangeAccess)
        .gap(Space::S1)
        .child(choices)
        .child(line(match (form.access, &all.home) {
            (BotAccess::Default, _) => t(lang, Words::BotsRoutineAccessAsks).to_owned(),
            (BotAccess::Workspace, Some(home)) => {
                fill(t(lang, Words::BotsRoutineAccessAnyPage), &[home.as_str()])
            }
            (BotAccess::Workspace, None) => t(lang, Words::BotsRoutineAccessAnyPageHome).to_owned(),
        }))
        .child(line(t(lang, Words::BotsRoutineAccessStartsDefault)));

    group.child(
        row("Routine access actions")
            .gap(Space::S1)
            .child(
                surface.button(
                    engine,
                    ACCESS_SAVE,
                    t(
                        lang,
                        if saving {
                            Words::BotsRoutineSaving
                        } else {
                            Words::BotsRoutineSave
                        },
                    )
                    .into(),
                    saving || !all.detail.navigable,
                ),
            )
            .child(surface.button(
                engine,
                ACCESS_CANCEL,
                t(lang, Words::BotsDialogCancel).into(),
                saving,
            )),
    )
}
