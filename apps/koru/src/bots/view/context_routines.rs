// SPDX-License-Identifier: MPL-2.0
//! Routine content in the existing context frame. No second surface or reader.
use super::{BotView, context_library::Surface};
use crate::bots::routines::Routines;
use koru_ai_core::{ChatId, bots::routines::*};
use koru_delegate_core::ScheduleId;
use koru_delegate_schedule::ScheduleState;
use koru_draw::{
    el::{El, Key, Role, col, row, text},
    text::TextEngine,
    theme::{Ink, Space, roles},
};
use koru_i18n::{Key as Words, Lang, fill, t};
use std::rc::Rc;

pub(crate) mod detail;

const SURFACE: &str = "bot-context-routines";
pub(crate) const REFRESH: Key = Key::surface(SURFACE, "refresh");
pub(crate) const NEXT: Key = Key::surface(SURFACE, "next");
pub(crate) fn details_key(schedule: ScheduleId) -> Key {
    Key::model(schedule.as_uuid().as_u128(), "routine-details")
}
pub(crate) fn chat_key(chat: uuid::Uuid) -> Key {
    Key::model(chat.as_u128(), "routine-conversation")
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Data {
    pub revision: u64,
    page: Option<Rc<BotRoutineListPage>>,
    loading: bool,
    stale: bool,
    notice: Option<Words>,
    expanded: Option<ScheduleId>,
    detail: crate::bots::routines::detail::Data,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Refresh,
    Next,
    RefreshDetail,
    NextHistory,
    Details(ScheduleId),
    Open(ChatId),
}
impl Data {
    pub(super) fn all_keys(&self) -> Vec<Key> {
        let mut keys = vec![REFRESH];
        if let Some(page) = &self.page {
            if page.next_schedule.is_some() {
                keys.push(NEXT);
            }
            keys.extend(page.items.iter().map(|i| details_key(i.schedule_id)));
        }
        keys.extend(detail::keys(&self.detail, false));
        keys
    }
    pub(super) fn selected(&self, key: Key) -> bool {
        self.expanded.is_some_and(|id| details_key(id) == key)
    }
    pub fn from_state(state: &Routines) -> Self {
        Self {
            revision: state.revision,
            page: state.held.clone(),
            loading: state.loading(),
            stale: state.stale,
            notice: state.notice,
            expanded: state.expanded,
            detail: state.detail.data(),
        }
    }
    pub fn keys(&self) -> Vec<Key> {
        let mut keys = Vec::new();
        if let Some(page) = &self.page {
            keys.extend(page.items.iter().map(|i| details_key(i.schedule_id)));
        }
        keys.extend(detail::keys(&self.detail, true));
        if !self.loading {
            keys.push(REFRESH);
            if self
                .page
                .as_ref()
                .is_some_and(|p| p.next_schedule.is_some())
            {
                keys.push(NEXT);
            }
        }
        keys
    }
    pub fn action(&self, key: Key) -> Option<Action> {
        if !self.keys().contains(&key) {
            return None;
        }
        if key == REFRESH {
            return Some(Action::Refresh);
        }
        if key == NEXT {
            return Some(Action::Next);
        }
        for item in &self.page.as_ref()?.items {
            if key == details_key(item.schedule_id) {
                return Some(Action::Details(item.schedule_id));
            }
        }
        detail::action(&self.detail, key)
    }
    pub fn content(&self, surface: &Surface, engine: &mut TextEngine, lang: Lang) -> El {
        let mut content = col("Routine overview").gap(Space::S2).pad(Space::S3);
        if let Some(notice) = self.notice {
            content = content.child(line(t(lang, notice)));
        }
        if self.loading {
            content = content.child(line(t(lang, Words::BotsLoading)));
        }
        if self.stale && self.notice != Some(Words::BotsRoutinesStale) {
            content = content.child(line(t(lang, Words::BotsRoutinesStale)));
        }
        if let Some(page) = &self.page {
            content = content.child(line(match &page.effective_model {
                BotRoutineModelObservation::Configured {
                    provider,
                    requested_model,
                } => fill(
                    t(lang, Words::BotsRoutineModel),
                    &[provider.as_str(), requested_model],
                ),
                _ => t(lang, Words::BotsRoutineModelUnavailable).into(),
            }));
            if page.items.is_empty() {
                content = content.child(line(t(lang, Words::BotsRoutinesEmpty)));
            }
            for item in &page.items {
                let (mut name, mut state) = match &item.live {
                    BotRoutineLiveObservation::Current { name, state, .. } => (
                        name.as_str().to_owned(),
                        match state {
                            ScheduleState::Active { .. } => Words::ScheduleNavigatorActive,
                            ScheduleState::Paused { .. } => Words::ScheduleNavigatorPaused,
                            ScheduleState::HandoffPending { .. } => {
                                Words::ScheduleNavigatorHandoffPending
                            }
                            ScheduleState::Retired => Words::BotsRoutineRetired,
                        },
                    ),
                    BotRoutineLiveObservation::NotLive {} => (
                        t(lang, Words::BotsRoutine).into(),
                        Words::BotsRoutineNotLive,
                    ),
                    _ => (
                        t(lang, Words::BotsRoutineSetup).into(),
                        Words::BotsRoutineLiveUnavailable,
                    ),
                };
                if self.expanded == Some(item.schedule_id) {
                    if let Some(current) = detail::heading(&self.detail, item.schedule_id, lang) {
                        (name, state) = current;
                    }
                }
                let mut group = col("Routine")
                    .id(Key::model(
                        item.schedule_id.as_uuid().as_u128(),
                        "routine-group",
                    ))
                    .semantics(Role::Group)
                    .name_text(name.clone())
                    .gap(Space::S1)
                    .child(
                        text(name.clone())
                            .named("Routine name")
                            .role(roles::ROW)
                            .wrap(),
                    )
                    .child(line(t(lang, state)))
                    .child(line(t(
                        lang,
                        match item.creation.state {
                            BotRoutineCreationState::Prepared => Words::BotsMemoryPrepared,
                            BotRoutineCreationState::Unconfirmed => Words::BotsMemoryUnconfirmed,
                            BotRoutineCreationState::Bound => Words::BotsRoutineBound,
                            BotRoutineCreationState::Conflict => Words::BotsRoutineConflict,
                        },
                    )))
                    .child(line(fill(
                        t(lang, Words::BotsRoutineUpdated),
                        &[&date(lang, item.creation.updated_at_ms)],
                    )));
                let count = item.grant.selected_page_count.to_string();
                if let Some(refusal) = item.creation.refusal {
                    group = group.child(line(t(
                        lang,
                        match refusal {
                            BotRoutineCreationRefusal::OriginMismatch => {
                                Words::BotsRoutineOriginChanged
                            }
                            BotRoutineCreationRefusal::DocumentChanged => {
                                Words::BotsRoutineDocumentChanged
                            }
                            BotRoutineCreationRefusal::OwnerChanged => {
                                Words::BotsRoutineOwnerChanged
                            }
                            BotRoutineCreationRefusal::AuthorityChanged => {
                                Words::BotsRoutineAuthorityChanged
                            }
                        },
                    )));
                }
                group = group
                    .child(line(match item.grant.access {
                        koru_ai_core::bots::BotAccess::Default => {
                            fill(t(lang, Words::BotsRoutineAccessDefault), &[&count])
                        }
                        koru_ai_core::bots::BotAccess::Workspace => {
                            t(lang, Words::BotsRoutineAccessWorkspace).into()
                        }
                    }))
                    .child(
                        row("Routine history disclosure").child(
                            surface
                                .button(
                                    engine,
                                    details_key(item.schedule_id),
                                    t(lang, Words::BotsRoutineDetails).into(),
                                    false,
                                )
                                .selected(self.expanded == Some(item.schedule_id))
                                .name_text(format!(
                                    "{}: {}",
                                    t(lang, Words::BotsRoutineDetails),
                                    name
                                )),
                        ),
                    );
                if self.expanded == Some(item.schedule_id) {
                    group = group.child(detail::content(&self.detail, surface, engine, lang));
                }
                content = content.child(group);
            }
        }
        content = content.child(row("Refresh routines").child(surface.button(
            engine,
            REFRESH,
            t(lang, Words::BotsRoutinesRefresh).into(),
            self.loading,
        )));
        if self
            .page
            .as_ref()
            .is_some_and(|p| p.next_schedule.is_some())
        {
            content = content.child(row("Next routines").child(surface.button(
                engine,
                NEXT,
                t(lang, Words::BotsRoutinesNext).into(),
                self.loading,
            )));
        }
        content
    }
}
fn line(value: impl Into<String>) -> El {
    let value = value.into();
    text(value.clone())
        .name_text(value)
        .named("Routine observation")
        .role(roles::META)
        .ink(Ink::Secondary)
        .wrap()
}
pub(crate) fn date(lang: Lang, ms: i64) -> String {
    let value = crate::format::absolute_local(lang, ms);
    if value.is_empty() {
        t(lang, Words::BotsRoutineDateUnavailable).into()
    } else {
        value
    }
}
impl BotView {
    pub(super) fn routines_pass(&mut self) -> bool {
        let Some(command) = self.routines.read().or_else(|| self.routines.detail.read()) else {
            return false;
        };
        let detail = matches!(
            &command,
            crate::bots::link::Command::ReadRoutineDetail { .. }
        );
        let sent = self.link.as_ref().is_some_and(|link| link.send(command));
        if !sent {
            if detail {
                self.routines.detail_unsent();
            } else {
                self.routines.unsent();
            }
        }
        let mut work = self.work.get();
        work.commands += usize::from(sent);
        self.work.set(work);
        true
    }
    pub(crate) fn perform_context_routine(&mut self, action: Action) -> bool {
        match action {
            Action::Refresh => self.routines.refresh(false),
            Action::Next => self.routines.refresh(true),
            Action::Details(schedule) => self.routines.toggle(schedule),
            Action::RefreshDetail => self.routines.detail_refresh(false),
            Action::NextHistory => self.routines.detail_refresh(true),
            Action::Open(chat) => {
                let Some(command) = self.routines.open(chat) else {
                    return false;
                };
                if !self.link.as_ref().is_some_and(|link| link.send(command)) {
                    self.routines.invalidate();
                }
                true
            }
        }
    }
    pub(crate) fn take_routine_open(&mut self) -> Option<(koru_ai_core::bots::BotId, ChatId)> {
        self.routines.take_open()
    }
}
