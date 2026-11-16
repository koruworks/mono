// SPDX-License-Identifier: MPL-2.0
//! Routine content in the existing context frame. No second surface or reader.
//!
//! **Each row is one Routine and its controls** (P4.3 piece 4): its name, its
//! switch, its state in words, its next time, and Test while it is paused.
//! The switch shows the state the owner last confirmed and nothing else: a
//! control on its way is said in words beside it and cannot be pressed.
use super::{BotView, context_library::Surface, controls::ToggleLeaf};
use crate::bots::routines::{Routines, control};
use koru_ai_core::{
    ChatId,
    bots::{BotAccess, routines::*},
};
use koru_delegate_core::ScheduleId;
use koru_delegate_schedule::ScheduleState;
use koru_draw::{
    el::{El, Key, Role, col, leaf, row, text},
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
/// A Routine's switch: Enable while it is paused, Pause while it is on.
pub(crate) fn toggle_key(schedule: ScheduleId) -> Key {
    Key::model(schedule.as_uuid().as_u128(), "routine-toggle")
}
pub(crate) fn test_key(schedule: ScheduleId) -> Key {
    Key::model(schedule.as_uuid().as_u128(), "routine-test")
}
/// The conversation of the Run that waits for the person's decision.
pub(crate) fn decide_key(schedule: ScheduleId) -> Key {
    Key::model(schedule.as_uuid().as_u128(), "routine-decide")
}

/// What the window knows of the Bot's conversations at work now, from the
/// roster's activity read: nothing is asked to learn it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    /// Conversations waiting for the person (an approval, a decision).
    pub needs_you: Vec<uuid::Uuid>,
    pub running: Vec<uuid::Uuid>,
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
    controls: control::Data,
    activity: Activity,
    /// The Bot's home workspace, by the name the window's catalog gives it.
    home: Option<String>,
    /// Whether the selected Routine's timing may be edited now.
    timing: bool,
    access_changeable: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Refresh,
    Next,
    RefreshDetail,
    NextHistory,
    Details(ScheduleId),
    Open(ChatId),
    /// The row's switch.
    Toggle(ScheduleId),
    Test(ScheduleId),
    /// Open the conversation whose Run waits for a decision.
    Decide(ChatId),
    EditTiming,
    ChangeAccess,
    ChooseAccess(BotAccess),
    SaveAccess,
    CancelAccess,
    OlderHistory,
}

/// One row's Routine, as the list read confirmed it.
struct Live<'a> {
    name: &'a str,
    state: &'a ScheduleState,
    next: &'a RoutineNextObservation,
}
fn live(item: &BotRoutineListItem) -> Option<Live<'_>> {
    match &item.live {
        BotRoutineLiveObservation::Current {
            name, state, next, ..
        } => Some(Live {
            name: name.as_str(),
            state,
            next,
        }),
        _ => None,
    }
}
/// Whether the row has a switch: only a Routine that is on or paused.
fn switchable(item: &BotRoutineListItem) -> Option<bool> {
    match live(item)?.state {
        ScheduleState::Active { .. } => Some(true),
        ScheduleState::Paused { .. } => Some(false),
        _ => None,
    }
}
fn testable(item: &BotRoutineListItem) -> bool {
    live(item).is_some_and(|live| {
        matches!(
            live.state,
            ScheduleState::Paused {
                reason: koru_delegate_core::SchedulePauseReason::Manual,
                ..
            }
        )
    })
}
fn conversations(item: &BotRoutineListItem) -> impl Iterator<Item = uuid::Uuid> + '_ {
    let items = match &item.local_conversations {
        BotRoutineLocalConversations::Available { items, .. } => items.as_slice(),
        _ => &[],
    };
    items
        .iter()
        .filter(|row| row.conversation == BotRoutineConversationVisibility::Available)
        .map(|row| row.chat_id)
}

impl Data {
    /// The conversation of `item` that waits for the person, if one does.
    fn deciding(&self, item: &BotRoutineListItem) -> Option<uuid::Uuid> {
        conversations(item).find(|chat| self.activity.needs_you.contains(chat))
    }
    fn running(&self, item: &BotRoutineListItem) -> bool {
        conversations(item).any(|chat| self.activity.running.contains(&chat))
    }
    /// Whether a new control may be pressed: the list is fresh and nothing
    /// is with the owner.
    fn controllable(&self) -> bool {
        !self.loading && !self.stale && !self.controls.busy()
    }
    fn test_enabled(&self, item: &BotRoutineListItem) -> bool {
        self.controllable()
            && testable(item)
            && self
                .controls
                .test_wait
                .is_none_or(|(waiting, _)| waiting != item.schedule_id)
    }
    /// **A switch's state for assistive technology**: on or off, as last
    /// confirmed. `None` for a key that is not a switch.
    pub(crate) fn switch_state(&self, key: Key) -> Option<bool> {
        self.page.as_ref()?.items.iter().find_map(|item| {
            (toggle_key(item.schedule_id) == key)
                .then(|| switchable(item))
                .flatten()
        })
    }
    fn item_keys(&self, item: &BotRoutineListItem, enabled_only: bool) -> Vec<Key> {
        let mut keys = Vec::new();
        let id = item.schedule_id;
        if switchable(item).is_some() && (!enabled_only || self.controllable()) {
            keys.push(toggle_key(id));
        }
        if testable(item) && (!enabled_only || self.test_enabled(item)) {
            keys.push(test_key(id));
        }
        if self.deciding(item).is_some() && (!enabled_only || (!self.loading && !self.stale)) {
            keys.push(decide_key(id));
        }
        keys.push(details_key(id));
        keys
    }
    pub(super) fn all_keys(&self) -> Vec<Key> {
        let mut keys = vec![REFRESH];
        if let Some(page) = &self.page {
            if page.next_schedule.is_some() {
                keys.push(NEXT);
            }
            for item in &page.items {
                keys.extend(self.item_keys(item, false));
            }
        }
        keys.extend(detail::keys(self, false));
        keys
    }
    pub(super) fn selected(&self, key: Key) -> bool {
        self.expanded.is_some_and(|id| details_key(id) == key)
            || self
                .controls
                .access
                .as_ref()
                .is_some_and(|form| detail::access_key(form.access) == key)
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
            controls: state.controls_data(),
            activity: Activity::default(),
            home: None,
            timing: state.timing_seed().is_some() && !state.control_busy(),
            access_changeable: state.access_changeable(),
        }
    }
    /// With what the window knows of the Bot's conversations at work, and
    /// the name of the Bot's home workspace.
    pub fn knowing(mut self, activity: Activity, home: Option<String>) -> Self {
        self.activity = activity;
        self.home = home;
        self
    }
    pub fn keys(&self) -> Vec<Key> {
        let mut keys = Vec::new();
        if let Some(page) = &self.page {
            for item in &page.items {
                keys.extend(self.item_keys(item, true));
            }
        }
        keys.extend(detail::keys(self, true));
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
            let id = item.schedule_id;
            if key == details_key(id) {
                return Some(Action::Details(id));
            }
            if key == toggle_key(id) {
                return Some(Action::Toggle(id));
            }
            if key == test_key(id) {
                return Some(Action::Test(id));
            }
            if key == decide_key(id) {
                let chat = self.deciding(item)?;
                return ChatId::parse(&chat.to_string()).map(Action::Decide);
            }
        }
        detail::action(self, key)
    }
    /// The names of the Routines in `ids`, as the list shows them.
    fn names(&self, ids: &[ScheduleId], lang: Lang) -> String {
        let page = self.page.as_ref();
        ids.iter()
            .map(|id| {
                page.and_then(|page| page.items.iter().find(|item| item.schedule_id == *id))
                    .and_then(live)
                    .map_or_else(
                        || t(lang, Words::BotsRoutine).to_owned(),
                        |live| live.name.to_owned(),
                    )
            })
            .collect::<Vec<_>>()
            .join(if lang == Lang::Ja || lang == Lang::ZhHans {
                "、"
            } else if lang == Lang::Ar {
                "، "
            } else {
                ", "
            })
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
        // What a Restore or an Unarchive said of the Routines.
        if let Some(restored) = &self.controls.restored {
            content = content.child(line(t(lang, Words::BotsRoutinesStayPaused)));
            if !restored.still_on.is_empty() {
                content = content.child(line(fill(
                    t(lang, Words::BotsRoutinesMayStillBeOn),
                    &[&self.names(&restored.still_on, lang)],
                )));
            }
        }
        if let Some(page) = &self.page {
            if page.items.iter().any(|item| self.deciding(item).is_some()) {
                content = content.child(line(t(lang, Words::BotsRoutinesNeedYou)));
            }
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
                content = content.child(self.group(item, surface, engine, lang));
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

    /// The state of a row in words: what the owner confirmed, or the control
    /// that is on its way. Never a state nobody confirmed.
    fn state_words(&self, item: &BotRoutineListItem) -> Words {
        let id = item.schedule_id;
        match self.controls.doing(id) {
            Some(control::Doing::TurningOn) => return Words::BotsRoutineTurningOn,
            Some(control::Doing::Pausing) => return Words::BotsRoutinePausing,
            _ => {}
        }
        if self.controls.checking == Some(id) {
            return Words::BotsRoutineChecking;
        }
        match &item.live {
            BotRoutineLiveObservation::Current { state, .. } => match state {
                ScheduleState::Active { .. } => Words::BotsRoutineOn,
                ScheduleState::Paused { .. } => Words::BotsRoutinePausedState,
                ScheduleState::HandoffPending { .. } => Words::ScheduleNavigatorHandoffPending,
                ScheduleState::Retired => Words::BotsRoutineRetired,
            },
            BotRoutineLiveObservation::NotLive {} => Words::BotsRoutineNotLive,
            _ => Words::BotsRoutineLiveUnavailable,
        }
    }

    fn group(
        &self,
        item: &BotRoutineListItem,
        surface: &Surface,
        engine: &mut TextEngine,
        lang: Lang,
    ) -> El {
        let id = item.schedule_id;
        let open = self.expanded == Some(id);
        let mut name = match &item.live {
            BotRoutineLiveObservation::Current { name, .. } => name.as_str().to_owned(),
            BotRoutineLiveObservation::NotLive {} => t(lang, Words::BotsRoutine).into(),
            _ => t(lang, Words::BotsRoutineSetup).into(),
        };
        // The selected detail names it, unless that detail is being read
        // again: the row then holds the newer word (a saved name).
        if open
            && !self.detail.loading
            && !self.detail.stale
            && let Some(current) = detail::heading(&self.detail, id, lang)
        {
            name = current;
        }
        let mut head = row("Routine heading").gap(Space::S2).items_center().child(
            text(name.clone())
                .named("Routine name")
                .role(roles::ROW)
                .fill()
                .wrap(),
        );
        if let Some(on) = switchable(item) {
            let pending = self.controls.doing(id).is_some() || self.controls.checking == Some(id);
            let disabled = !self.controllable();
            let focused = surface.focus == Some(toggle_key(id));
            head = head.child(
                leaf(
                    "Routine switch",
                    ToggleLeaf {
                        on,
                        enabled: !disabled,
                        pending,
                        focused,
                        rtl: lang.is_rtl(),
                    },
                )
                .semantic()
                .id(toggle_key(id))
                .hit()
                .focusable()
                .disabled(disabled)
                .semantics(Role::Button)
                .selected(on)
                .name_text(fill(
                    t(
                        lang,
                        if on {
                            Words::BotsRoutinePauseName
                        } else {
                            Words::BotsRoutineTurnOnName
                        },
                    ),
                    &[&name],
                )),
            );
        }
        let mut group = col("Routine")
            .id(Key::model(id.as_uuid().as_u128(), "routine-group"))
            .semantics(Role::Group)
            .name_text(name.clone())
            .gap(Space::S1)
            .child(head)
            .child(line(t(lang, self.state_words(item))));
        if let Some(live) = live(item)
            && matches!(live.state, ScheduleState::Active { .. })
        {
            group = group.child(line(match live.next {
                RoutineNextObservation::Exact { at_ms } => {
                    fill(t(lang, Words::BotsRoutineNextAt), &[&date(lang, *at_ms)])
                }
                RoutineNextObservation::OwnerLocal {} => {
                    t(lang, Words::BotsRoutineNextOwnerLocal).into()
                }
                RoutineNextObservation::Inactive {} => {
                    t(lang, Words::BotsRoutineNextInactive).into()
                }
                RoutineNextObservation::Unavailable {} => {
                    t(lang, Words::BotsRoutineNextUnavailable).into()
                }
            }));
        }
        if switchable(item) == Some(false) && self.running(item) {
            group = group.child(line(t(lang, Words::BotsRoutineRunWillFinish)));
        }
        if self.deciding(item).is_some() {
            group = group
                .child(line(t(lang, Words::BotsRoutineNeedsYou)))
                .child(
                    row("Routine decision").child(
                        surface
                            .button(
                                engine,
                                decide_key(id),
                                t(lang, Words::BotsRoutineOpenDecision).into(),
                                self.loading || self.stale,
                            )
                            .name_text(format!(
                                "{}: {}",
                                t(lang, Words::BotsRoutineOpenDecision),
                                name
                            )),
                    ),
                );
        }
        if self
            .controls
            .restored
            .as_ref()
            .is_some_and(|restored| restored.needing_access.contains(&id))
        {
            group = group.child(line(t(lang, Words::BotsRoutineChooseAccessAgain)));
        }
        for note in self.controls.notes(id) {
            group = group.child(line(t(lang, *note)));
        }
        group = group
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
        if let Some(refusal) = item.creation.refusal {
            group = group.child(line(t(
                lang,
                match refusal {
                    BotRoutineCreationRefusal::OriginMismatch => Words::BotsRoutineOriginChanged,
                    BotRoutineCreationRefusal::DocumentChanged => Words::BotsRoutineDocumentChanged,
                    BotRoutineCreationRefusal::OwnerChanged => Words::BotsRoutineOwnerChanged,
                    BotRoutineCreationRefusal::AuthorityChanged => {
                        Words::BotsRoutineAuthorityChanged
                    }
                },
            )));
        }
        // The access the Routine runs under now.
        group = group.child(line(access_now(lang, &item.grant)));
        let mut actions = row("Routine actions").gap(Space::S1);
        if testable(item) {
            let label = match (self.controls.doing(id), self.controls.test_wait) {
                (Some(control::Doing::Testing), _) => {
                    t(lang, Words::BotsRoutineTestStarting).into()
                }
                (_, Some((waiting, at))) if waiting == id => fill(
                    t(lang, Words::BotsRoutineTestAvailableAt),
                    &[&time(lang, at)],
                ),
                _ => t(lang, Words::BotsRoutineTest).to_owned(),
            };
            actions = actions.child(
                surface
                    .button(
                        engine,
                        test_key(id),
                        label.clone(),
                        !self.test_enabled(item),
                    )
                    .name_text(format!("{label}: {name}")),
            );
        }
        actions = actions.child(
            surface
                .button(
                    engine,
                    details_key(id),
                    t(lang, Words::BotsRoutineDetails).into(),
                    false,
                )
                .selected(open)
                .name_text(format!("{}: {}", t(lang, Words::BotsRoutineDetails), name)),
        );
        group = group.child(actions);
        if open {
            group = group.child(detail::content(self, surface, engine, lang));
        }
        group
    }
}
/// The access a Routine has now, in one line.
pub(super) fn access_now(lang: Lang, grant: &BotRoutineGrantSummary) -> String {
    match grant.access {
        BotAccess::Default => fill(
            t(lang, Words::BotsRoutineAccessNowDefault),
            &[&grant.selected_page_count.to_string()],
        ),
        BotAccess::Workspace => t(lang, Words::BotsRoutineAccessNowWorkspace).into(),
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
/// A clock time on this computer, the reader's way.
pub(crate) fn time(lang: Lang, ms: i64) -> String {
    match crate::format::local_parts(ms) {
        Some(parts) => {
            koru_i18n::time::LocaleFormat::for_system(Some(lang.tag()), None).time_input(parts)
        }
        None => t(lang, Words::BotsRoutineDateUnavailable).into(),
    }
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
impl BotView {
    pub(super) fn routines_pass(&mut self) -> bool {
        // A Test's wait or its minute ended: the clock the tab's deadline
        // asked for. Nothing is sent by it but one read of the history.
        let ticked = self.routines.control_tick(now_ms());
        let Some(command) = self.routines.read().or_else(|| self.routines.detail.read()) else {
            return ticked;
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
    /// When the Routines shown next need the clock: a Test's wait or minute
    /// ending. `None` while they are hidden.
    pub(super) fn routines_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let at = self.routines.control_deadline()?;
        let wait = u64::try_from(at.saturating_sub(now_ms())).unwrap_or(0);
        Some(
            now.checked_add(std::time::Duration::from_millis(wait))
                .unwrap_or(now),
        )
    }
    /// Send one owner control, or say that nothing was sent.
    fn send_routine_control(&mut self, command: Option<crate::bots::link::Command>) -> bool {
        let Some(command) = command else {
            return false;
        };
        if !self.link.as_ref().is_some_and(|link| link.send(command)) {
            self.routines.control_unsent();
        }
        true
    }
    pub(crate) fn perform_context_routine(&mut self, action: Action) -> bool {
        match action {
            Action::Refresh => self.routines.refresh(false),
            Action::Next => self.routines.refresh(true),
            Action::Details(schedule) => self.routines.toggle(schedule),
            Action::RefreshDetail => self.routines.detail_refresh(false),
            Action::NextHistory => self.routines.detail_refresh(true),
            Action::OlderHistory => self.routines.history_older(),
            Action::Toggle(schedule) => {
                let command = self.routines.toggle_routine(schedule);
                self.send_routine_control(command)
            }
            Action::Test(schedule) => {
                let command = self.routines.test_routine(schedule);
                self.send_routine_control(command)
            }
            Action::ChangeAccess => self.routines.open_access(),
            Action::ChooseAccess(access) => self.routines.choose_access(access),
            Action::CancelAccess => self.routines.cancel_access(),
            Action::SaveAccess => {
                let command = self.routines.save_access();
                self.send_routine_control(command)
            }
            Action::EditTiming => {
                let Some(seed) = self.routines.timing_seed() else {
                    return false;
                };
                self.routine_timing_request = Some(seed);
                true
            }
            Action::Open(chat) | Action::Decide(chat) => {
                let command = if matches!(action, Action::Decide(_)) {
                    self.routines.open_decision(chat)
                } else {
                    self.routines.open(chat)
                };
                let Some(command) = command else {
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
    /// The Routine whose timing dialog was asked for, taken once: the window
    /// opens it.
    pub(crate) fn take_routine_timing_request(&mut self) -> Option<control::TimingSeed> {
        self.routine_timing_request.take()
    }
    /// **Save in the timing dialog**: one command, or the sentence for why
    /// nothing was sent.
    pub(crate) fn save_routine_timing(
        &mut self,
        schedule: ScheduleId,
        name: koru_delegate_schedule::ScheduleName,
        recurrence: koru_recurrence::RecurrenceRule,
    ) -> Result<(), Words> {
        let command = self.routines.save_timing(schedule, name, recurrence)?;
        if !self.link.as_ref().is_some_and(|link| link.send(command)) {
            self.routines.control_unsent();
        }
        Ok(())
    }
}
